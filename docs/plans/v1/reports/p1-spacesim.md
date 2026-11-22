# p1-spacesim: a simulator of the space gate for trying policies in seconds

Branch `p1/spacesim` from main `1738f2e`. New dev-only crate `crates/lunix-spacesim` (library and the binary `spacesim`, `publish = false`, not a
dependency of anything). Built and run on the remote host only. No on-disk format and no product behaviour changed; the one product change is
visibility (below).

## What is real and what is modelled

| piece | status |
|---|---|
| space gate: ledger, limit from the file system, soft/hard marks, token bucket, delay cap from arrival, full latch, ring pace, `reserve_since` with its classes, waiters, the 10 s ingest and reclaim meters | **real** (`lunix_space::LunixSpace` with `background_timers = false`, `SimFs`, `ManualClock`; `tick` and `refresh` are called by the simulation) |
| pressure level, hold, floor grace, budget factor, rate factor | **real** for `main`: `Pressure`, `PressureTracker`, `Pressure::budget/params` |
| ring pace state machine, drain history (cut bound by drain rate) | **real**: `RingPacer`, `DrainHistory` |
| ADR-33 yield to ring-parked writes | `yielding()` real; the rule that decides when (`gc_yields_to_ring`, 2 s hold) is a port |
| `gate-gc` (p1/gate-gc 6bc041c) and `leadzone` (p1/leadzone 9ed3b6d) | **ported** from the branch diff and from the report (the code is not on main); small `impl Policy` each |
| workload | modelled: closed loop, 128 x 8 in flight, 4 KiB random (or sequential) overwrite; `agg` = 16 writes per gate request (the gate sees bytes; one request of 16 records is one reservation) |
| journal ring: capacity, parking, retry with `ReserveClass::Retry`, release at C9 | modelled |
| cuts: trigger, size rule (`bound_cut_bytes`: drain budget, one file, half ring), duration, C2 bound = uncompressed bytes of the dirty chunks reserved with `reserve_headroom`, true-up at C4, share of the coordinator for GC after a cut (`gc_share`) | modelled (ports of the code's rules) |
| store: every segment individually, live chunks drawn per cut from the seed (binomial), wholly-dead rule, 3 retained FULLs, condemn, grace, recheck, delete, LFS cost-benefit compaction, unnamed outputs, sources freed one cut after naming | modelled |
| collector schedule: pipelined `due`/`min_gap`, idle 30 s vs busy 5 s cadence, `grace_ended_since_mark`, mark cost (fixed + per chunk), per-object condemn/delete cost (5.5/13 ms, gcrate), compaction at the rate cap and the device, ladder urgent pass with its back-off | modelled (ports) |
| device: RAM (CPU-bound: 110 K appends/s, 1.9 GB/s cut, 0.8 GB/s compaction) and network volume (one 70 MB/s device shared by journal, cuts and GC) | modelled, constants fitted (below) |

**Product change (visibility only, listed per the brief):** `lunix-engine` exposes `DrainHistory`, `RingPacer` (made `pub`, with their methods and the
constants `HISTORY`, `PACE_*`) and re-exports `yielding`, `HOLD`, `BUDGET_FACTOR`, `YIELD_*`. `crates/lunix-engine/src/{cut,lib}.rs` only. No logic touched.

## How to run

```
spacesim run -s ram-64k -p main --trace out.csv          # one run, summary and per-second CSV
spacesim seeds -s ram-64k -p leadzone -n 8               # 8 seeds, phase of the collector's 30 s cadence drawn per seed; median (min-max)
spacesim sweep -s ram-64k --grid cache_max=12G,16G,24G --grid soft_percent=15,40 -n 5
spacesim calibrate -n 6                                  # every scenario x main/gate-gc/leadzone
spacesim run -s ram-64k -p tunable --params candidate.toml
```

Scenarios: `ram-64k`, `ram-16k`, `netvol-64k`, `netvol-16k`, `m15-8g`. Any parameter of `Scenario::set` is a `--set key=value`. A 600 s run takes 0.3 s
(release), a 190 s three-cell run 0.1 s. `policy.key` addresses the policy's own parameters (`tunable`: `pressure_from`, `grace_s`, `busy_interval_s`,
`growth_gib`, `p1`; `gate-gc`: `amp_percent`; `leadzone`: `pass_periods`). A parameter file is flat TOML.

**Adding a policy:** implement `Policy` (`settings()` returns the collector's level, parameters, budget, share, per-pass cap, cadence, growth trigger,
compaction threshold; `gate()` the throttle configuration; `admission_rate()` an extra pace in front of the real gate for candidates that change what the
gate charges, P1/P5/P6 of the candidate note) and add it to `policy_by_name`. Per-segment tables for G2/G9 and hot/cold workloads are not in yet (below).

Output per run: stops, episodes and durations, throttled time, p50/p99/p99.9/max completion, burst (first 10 s) and sustained IOPS, the slowest second,
reclaim rate at the first band entry, peak `used`/limit, delayed and capped writes, ring-wait maximum, store amplification (before the band and over the run),
GC write amplification, reclaim by path (whole-dead vs compaction sources), victims by live fraction, collector busy share, the dead-time split
(retention, detection, grace, recheck), first condemn and first delete, first-fill and steady-state windows, and a per-second CSV.

## The uncounted host-file-system bytes (finding)

The measured `limit` fell from 19.0 GiB (t = 10-16 s) to 16.5-17.1 GiB at the stop. From the t6t8 status series (`partA_c64k_q4_w4k_rep1/2`):
`external = total - fs_free - used` is **1023-1034 MiB at every sample with the journal ledger at 0** (stop and idle states, both repetitions) and about 1 GiB
minus the ledger's journal bytes otherwise; it is negative (-0.2 to -1.4 GiB) in samples taken while a cut is in flight.

* **Ring files: 1 GiB = `journal_max`.** The journal's ring files are preallocated and zero-filled at `journal_max` (`lunix-journal/src/lib.rs:4`, 64 MiB files; `sys.rs:240`
  `KEEP_SIZE` allocation), so the host holds all of them whatever the ledger says; the ledger counts only the records (`JournalReserved`). The gate's limit is
  `used_mark + fs_free - floor - reserve` (`lunix-space/src/ledger.rs:108`), so these bytes are accounted correctly **through the probe**, not through the ledger. The
  consequence is that `cache_max` bounds the ledger, not the disk: a capped volume can hold `cache_max + reserve + journal_max - records` on the host (ADR-32's "store <=
  cache_max + the reserved region + one cut" does not mention the ring). Not a bug of the limit; a documentation point for the cap and for the M15 cells.
* **The C2 bound over-counts while a cut runs.** `reserve_bounds` (`lunix-engine/src/coord.rs:299-340`) reserves `n x (chunk_size + ovh + 64)`: the **uncompressed** size,
  about twice the stored size at the measured 1.97 compression, from C2 to the true-up at C4. The ledger's `used` is inflated by the difference for the length of the cut (0.5 s),
  which is why the measured `space.used` exceeds the disk (negative `external`) and why the writes stop at the gate for that time. Because the free-space anchor is
  `used_mark` (`ledger.rs:108`, `gate.rs` `refresh`), a reading taken during a cut also **raises the limit by the over-count** (the 19.0 GiB), and the limit then
  falls back when the cut ends. So a volume can start a cut under an inflated limit and end it above the true limit. If anything in the product is to change,
  it is this: reserve the bound with the expected compressed size (or true up at C3), or anchor the limit on the disk figure alone. Not changed here.
* **Left over, not explained:** at the first stop sample (rep 1, t = 21 s) `external` is 1657 MiB, 0.6 GiB more than the ring; at no other stop sample. Candidates are
  an open segment being written, an index run or a compaction output before the ledger charge; it cannot be told apart from the status alone.

The simulation now takes these bytes from the code's rules: the probe reports `total - (segments on disk + the cut's output as it is written + journal_max + 10 MiB)`,
so the inflated and the true limit both emerge from the real gate code.

## Calibration (policy `main`; 8 seeds with the collector cadence phase drawn from the seed; sim = median (min-max))

Round 2 changed three things: the host bytes above; `cut_trigger` 0.08 (a cut starts at about 36 MiB of journal records, from the measured median `last_cut_bytes` of 30 MiB
and 7 629 writes per cut in `tables-writes-ram.md`; before, cuts were whole-ring); `ring_park` 448 MiB (the peak of the measured `space.journal` over the series, 456 MiB).

| cell | measured | simulated | verdict |
|---|---|---|---|
| RAM 64K first stop | 12-15 s (brief), 20-27 s (t6t8 series) | 12.7 s (10.1-13.9) in 8 of 8 first cells | match (brief) |
| RAM 64K stops per cell | 1-7 (1-4 t6t8) | 4 (1-7); first cell 2 (1-4) | match |
| RAM 64K max completion | about 18-33 s | 19.7 s (0.9-33.1); first cell 20.4 (0.9-32.1) | match |
| RAM 64K reclaim_rate at first band entry | 0 | 0 (0-0) MB/s | match |
| RAM 64K sustained IOPS | 24-25 K | 16.5 K (6.9-29.8) | 33 % low |
| RAM 64K store bytes per admitted byte before the band | about 2.8 | 4.2 (4.0-4.3) | 50 % high |
| RAM 16K | 4 stops in 9 cells, max 5.8 s, 43.6 K IOPS | stops 1 (0-3), max 5.9 s (0.6-12.9), 35.7 K IOPS | max match; IOPS 18 % low; first stop 39.6 s |
| network volume 64K / 16K | 0 stops, 0 delayed, 3.3 K / 8.7 K IOPS, max 20 / 5 s | 0 stops, 0 delayed, 1.3 K / 3.6 K IOPS, max 3.9 / 1.1 s | gate match; IOPS 2.5x low (item 4) |
| M15 8 GiB cap, 16K | stops 0-2 (base), longest 14-38 s, first stop 15 s after entry, reclaim 0 | stops 7 (5-10), longest 32.6 s (28.9-33.7), first stop 17.5 s, reclaim 0 | longest, timing, reclaim match; stops 3x high |

**Null results.** `gate-gc`: stops per cell 3 (0-8), first cell 2 (0-4), longest completion 20.0 s (1.0-31.4) against `main` 4 (1-7), 2 (1-4), 19.7 s;
IOPS 20.3 K against 16.5 K (inside the run-to-run spread of 7-30 K); reclaim at entry 0 (0-19) MB/s. `leadzone`: 3 (1-6), 2 (1-4), 20.4 s, 16.7 K IOPS, reclaim 0: no change. M15:
stops 6 (4-7) and 7 (5-10) against 7 (5-10), longest 40.5 s and 32.6 s against 32.6 s. No material improvement from either.

**Which comes first, in the simulation and in the measurement.** With whole-ring cuts (round 1) the cut's C2 bound (up to 2 GB uncompressed) did not fit under `limit + reserve`
near the limit, the commit failed for space, the ring filled and the writers parked in the ring while the gate stayed in `slowdown`: a reservation failure first.
With cuts of the measured size (36 MiB, bound 0.25 GB) the bound fits until `used` is at the limit, the band is crossed in about 5 s at the store amplification
(the gate paces journal bytes while the headroom burns at 4x), and the gate stops first with the ring nearly empty, as measured (journal 0 at the stop). The stop then lasts
until the first condemned segments pass their floor grace: that is the 20-33 s stall. The result is sensitive to the cut size: whole-ring cuts do not stop at the gate.

**Sensitivity of the fitted or measured-but-uncertain constants** (`sweep`, first cell 60 s, 6 seeds, +-50 %; stops per cell / longest completion s / IOPS):

| constant | -50 % | default | +50 % |
|---|---|---|---|
| `engine_iops` 110 K (burst ceiling 65-100 K, fitted to the 77 K average) | 3 / 19.5 / 17.5 K | 2 / 20 / 16 K | 3 / 26.0 / 29.4 K |
| `ring_park` 448 MiB (peak of `space.journal`) | 2 / 31.2 / 17.8 K | 2 / 20 / 16 K | 2 / 20.6 / 31.3 K |
| `store_bw` 1.9 GB/s (528 ms for 345 MiB of journal, from the cut stats) | 4 / 19.0 / 19.0 K | 2 / 20 / 16 K | 4 / 26.2 / 24.4 K |
| `gc_bw` 0.8 GB/s (fitted) | 3 / 20.3 / 28.2 K | 2 / 20 / 16 K | 4 / 20.4 / 28.2 K |
| `cut_trigger` 0.08 (from the measured cut size) | 2 / 31.3 / 26.9 K | 2 / 20 / 16 K | 2 / 25.8 / 28.3 K |

The stall (stops 2-4 per first cell, longest completion 19-31 s) holds across the +-50 % ranges; the sustained IOPS moves 16-31 K and is what the constants decide
(`engine_iops` and `ring_park` the most): the 24-25 K measured is inside the range. Still fitted: `engine_iops`, `gc_bw`, `cut_fixed_ms`. Code or trace derived: `ring_park`
(peak of the ledger's journal), `cut_trigger` (median cut), `store_bw` (cut durations), the 1 GiB ring files, the C2 bound, compression 0.506 (`stored_clen/stored_ulen`).

**Network volume (item 4).** The shared device is now work-conserving by shares (cuts 60 %, journal 40 % while a cut runs) with the cut size taken from the measured 14 K writes
per cut. IOPS rose from 1.0 K / 1.7 K to 1.3 K / 3.6 K against 3.3 K / 8.7 K, which is still 2.5x low, not within 1.5x. The model charges every cut's store bytes to the one
bandwidth at the amplification of the cut (4.7 bytes per journal byte at 64K) and gives 2.9 K IOPS in the closed form; the remaining factor is the ring stall while a cut
holds the device, and the real volume's seal and `fdatasync` overlap (T8 shows the seals not scaling with parallel seals) which the model does not have. The gate is not engaged in
either case, as measured.

**Distribution over 8 seeds (RAM 64K, `main`).** First stop at 10.1-13.9 s (8 of 8 first cells stop); stops per cell 1-7 (median 4); the t6t8 series' 20-27 s first stop is not
what the model gives (it gives the brief's 12-15 s); the two measurements disagree with each other by that factor, and the series' first sample is 11 s after the start, so its
first stop is bracketed late.

### What still does not reproduce

* **Amplification before the band: 4.2 against 2.8 measured, and sustained IOPS 33 % low.** The measured cuts are small (n = 7.6 K) yet the store grew 2.8x the admitted bytes;
  uniform random overwrite of 7.6 K writes over 32 K chunks would write 6.9 K chunks per cut (7x). Something deduplicates or merges the writes of consecutive cuts in the real
  system (the overlay holds the dirty chunks until the cut; chunks are re-dirtied while the cut is in flight, and a cut may cover a prefix by chunk) that the model does not.
  The stop timing matches because the headroom burn is dominated by the journal debt, not by this ratio.
* **Condemned garbage at the stop:** measured `condemned_pending` is about 1 GB at the first stop; the model condemns far more wholly-dead segments (it has up to 11 GB), because
  its per-cut death probability is higher than the real one. Same root cause as the first item. The stall length is nevertheless set by the grace, as measured.
* **Network-volume IOPS** (above), **M15 stops** (7 against 0-2; the cell's workload is assumed), **RAM 16K first stop** (39.6 s, measured stops in 4 of 9 cells).

## Model limits

Segments are drawn per cut from the binomial of a uniform random overwrite; no skew (zipf, hot/cold) and no per-segment temperature yet. One lane, one
ring; the lane imbalance of the 4-queue cell (two lanes used) is folded into `ring_park`. Metadata bytes (map, index, commit objects) are not charged. Reads,
FUA writes, discards and quiesce are absent. The collector's marks are a cost model, not a walk. Compaction output is charged progressively and trued up at
the end of the pass. A compaction output is skipped by the cut it was written before (one cut of liveness inaccuracy).

## Left for later (the second lead note, GC candidates), in order

Done: three retained FULLs (`retain_cuts` = 3); mark cost = fixed + per chunk, condemn 5.5 ms and delete 13 ms per segment, switches `batch_index` (G1),
`incremental_mark` (G3) and `gc_workers` (G4, collector off the coordinator); compaction threshold hook (G5, `compact_threshold` in `GcSettings`); growth-triggered
pass (G2 approximation, `growth_gib`); reclaim by path, victim histogram and collector share in the report.
Not done: zipf and hot/cold workloads with per-segment temperature (G6); a per-segment usage table with event-driven passes (G2 proper); grace from tip death (G9);
G5's "skip segments expected to die within the payoff time" and debt pacing; the gate-side hooks P5/P6 need the collector's forecast (`next_delete_in` is a stub
that returns none); P7 (cap clock from the last stop exit) needs a product change in the gate and cannot be a simulator-side hook; the report's first-fill window
is "overwrite start to first delete plus 30 s" and the dead-time split has no "waiting to be a candidate" term yet.

## Gates and deviations

* `tools/ci.sh` on the remote: `ci: all green` (fmt, clippy `-D warnings`, `cargo test --workspace` including 25 simulator tests, the crashpoint builds, ownership
  `[spacesim]`). The simulator's own tests run in 1.4 s. No kill loops: no product behaviour changed.
* `tools/ownership.toml` has a `[spacesim]` entry with exactly the changed paths (`Cargo.lock` in a separate `deps:` commit).
* Nothing in `lunix-gc` or `lunix-index` was touched. The only product change is the visibility change in `lunix-engine/src/{cut,lib}.rs` above.
* The simulator's G1 cost constants (`condemn_ms` 5.5, `delete_ms` 13) are the gcrate figures; a pass log of the p1/m16 RAM run shows the real phase times
  (condemn about 4 ms and delete about 5 ms per segment unit, mark 12 ms at that size), to be replaced by p1/gcbatch's measurements.
* The last coordinator message (remote CPUs 0-7 only) arrived while the full CI run (pinned 0-15 when it started) was finishing; nothing was run on 8-15 afterwards.
