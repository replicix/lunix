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
Done in the experiments phase: P0 variants, P1, P2, P3/P4, G9, G5, G1/G3/G4, G7 (upper bound). Not done: zipf and hot/cold workloads with per-segment temperature (G6); a per-segment usage table with event-driven passes (G2 proper); grace from tip death (G9);
G5's "skip segments expected to die within the payoff time" and debt pacing; the gate-side hooks P5/P6 need the collector's forecast (`next_delete_in` is a stub
that returns none); P7 (cap clock from the last stop exit) needs a product change in the gate and cannot be a simulator-side hook; the report's first-fill window
is "overwrite start to first delete plus 30 s" and the dead-time split has no "waiting to be a candidate" term yet.

## Experiments: candidate policies at two store-amplification levels

Run by `spacesim experiments -s <cell> -n 8 --merge <m>`: 8 seeds per row, the phase of the collector's idle cadence drawn per seed, medians. The amplification knob is
`merge`, the share of a cut's distinct chunks that reach the store (writes merged before the cut): RAM 64K `merge` 1.0 gives 4.2 stored bytes per admitted byte (the
calibrated level) and 0.4 gives 2.8 (the measured amplification); the 16K and M15 cells use 1.0 and 0.66; the network-volume cell is the no-regression check. **`main` itself only
reproduces the measured stall at the 4.2 level** (at 2.8: stops 1/1, longest 12 s, first stop 29 s), so a candidate's gain at 2.8 is measured against a baseline that already stalls less.
Columns: stops in the first-fill window / steady state (first fill = overwrite start to first delete + 30 s); first stop (median seconds, seeds that stopped in the first cell); longest completion
(max over cells, median over seeds); p99.9 of the first cell; IOPS first fill / steady; burst IOPS (first 10 s); peak `used`/limit; amplification; reclaim by path (whole-dead segments / compaction sources,
GB over the run); collector busy share. P7 (cap from the last stop exit) changes the gate and has no row; P5/P6 were not run. "Grace 0 s" is a grace of zero with the pass cadence unchanged.

### Ranked recommendation (what to implement for real, in order)

A candidate counts as a win only if it wins at both amplification levels on RAM 64K and does not lose on the others.

1. **P1, pace in store bytes against the projected headroom** (gate only; no correctness dependency). RAM 64K: stops 4/7 -> 1/0 (4.2) and 1/1 -> 0/0 (2.8); longest completion 30.5 -> 11.8 s
   and 12.1 -> 2.4 s; first-fill IOPS unchanged, steady IOPS -15 % at 4.2; RAM 16K 0 stops at both levels (2/2 -> 0/0); M15 8 GiB: little (longest 32.6 -> 28.9 s, 32.0 -> 22.5 s: the cap, not the pace, is
   the limit, `R x tau` against the room), network volume unchanged. Cost: p99.9 of the first cell rises (1.0 -> 6.2 s at 4.2: a crawl instead of a stop), `A_hat` becomes a control input. Expected gain: removes most stops on volumes whose
   garbage room covers the dead time; confidence medium-high (mechanism is the one measured: the band crossed in 5 s at the store amplification). First because it needs nothing else.
2. **P4/P3: a short grace under pressure with a 1 s pass gap** (needs reader quiescence, P3). Grace 5 s + gap 1 s: RAM 64K longest 30.5 -> 9.1 s (4.2) and 12.1 -> 0.2 s (2.8), IOPS 24.7/14.3 K -> 47/39 K and 41/26 K -> 72/62 K;
   RAM 16K and M15 (longest 32.6 -> 7.7 s at 1.0, 32.0 -> 5.3 s at 0.66) improve the same way; grace alone (P3, 30 -> 5 s, gap unchanged) is much weaker at 4.2 (longest 29.7 s) because the 5 s pass cadence and detection
   dominate; grace below 5 s changes nothing (the cadence floors it). Cost: collector busy 15 % -> 40 %, more marks. This is the only candidate that raises the sustained rate at a given cap (the `R x tau` law), at every cell. Depends on a
   reader-quiescence mechanism or a read that re-resolves on `NotFound` (candidate note, section 1) and its property tests and kill loops; confidence medium (the model, not the code, prices the dead time).
3. **G7, free dead ranges without copying** (upper bound; needs the FREEZE work: a dead-range record or a segment layout that tolerates holes, and hole-punch support: xfs, ext4 and tmpfs have it). Already with the 30 s grace:
   RAM 64K longest 9.6 s (4.2) and 1.0 s (2.8), stops 2/5 and 0/0; M15 longest 32.6 -> 6.2 s; with a 5 s grace the sustained IOPS reaches 28-44 K (4.2) and 55-78 K (2.8). It beats G5 by a wide margin and, unlike the grace work, does not need quiescence;
   it does need the format change and an fs-semantics audit. Confidence low-medium (an upper bound: punching costs and fragmentation are not modelled).
4. **P2 growth-triggered passes, G9 grace from death** (cheap, small). P2 (1 GiB): first stop moves 13 -> 17 s at 4.2, M15 at 0.66 longest 32 -> 22 s; nothing at 2.8. G9: RAM 64K at 4.2 longest 30.5 -> 19.1 s and IOPS +15 %, nothing else.
   G9 needs an argument that a reader cannot take a hint to a segment after its last chunk died (the retained FULLs still name it): confidence low.
5. **P0 (the C2 over-count), as hygiene.** (b) true-up at the seal and (c) anchoring the limit on the disk figure are safe against a real `ENOSPC` (they release earlier, or lower the limit); (a) reserving the compressed
   estimate is **not** safe for incompressible data unless it falls back to the uncompressed bound per chunk. Gains are small: RAM 64K stops in the first window 4 -> 2-4, first stop 13 -> 17-19 s, no change elsewhere.
6. **Combinations:** `P2 + grace 5 s + gap 1 s` is the grace work again (RAM 64K longest 3.0 s at 4.2). `P2 + P1` removes the stops at RAM 64K (1/0, 0/0) without the grace work. **`G7 + P2 + grace 5 s + gap 1 s`** is
   the best in every cell (RAM 64K 1/7 stops, longest 1.0 s at 4.2 and 0.1 s at 2.8; M15 0 stops, longest 0.3 s at 1.0) and the ceiling of what GC-side work can give. Recommended order of work: P1 now; then the grace and gap
   work behind the quiescence mechanism; G7 only if the FREEZE cost is accepted.
7. **No gain, do not implement:** G5 (threshold from headroom), G4 (two collector threads: busy share 60 %, no effect: the coordinator is not the bottleneck), G3, `gate-gc`, `leadzone`. G1 lowers the collector's share from 24 % to 10 % and is worth
   having for that (steady IOPS +30 % at 4.2 in the RAM cell) but does not touch the stall.

Limits of these numbers: one workload shape per cell, the device and amplification uncertainty above, no skew, and a model of the pass schedule; the `merge` knob stands for whatever lowers the real store growth and also lowers the chunk
death rate. The ranking is by the effect on stops and longest completion at both levels; the IOPS gains of the grace candidates (2-3x) are the model's and are bounded in reality by the 110 K append ceiling.


### ram-64k, high amplification (merge 1.0)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 4 / 7 | 13 (8/8) | 30.5 | 1.02 | 24.7K / 14.3K | 96.4K | 1.017 | 4.2 | 32.1 / 27.6 | 15 % |
| gate-gc | 3 / 6 | 13 (7/8) | 28.6 | 1.02 | 23.3K / 18.4K | 96.1K | 1.023 | 4.2 | 37.0 / 22.4 | 18 % |
| leadzone | 4 / 6 | 13 (8/8) | 30.6 | 1.02 | 24.7K / 14.5K | 96.4K | 1.017 | 4.2 | 32.1 / 25.2 | 14 % |
| P0a bound compressed | 2 / 6 | 18 (8/8) | 30.2 | 1.02 | 25.8K / 16.3K | 96.0K | 1.017 | 4.2 | 34.1 / 28.8 | 14 % |
| P0b true-up early | 4 / 6 | 19 (8/8) | 31.0 | 5.53 | 26.3K / 15.4K | 95.6K | 1.017 | 4.2 | 37.6 / 26.2 | 13 % |
| P0c limit anchored on disk | 4 / 5 | 17 (8/8) | 31.2 | 0.97 | 24.7K / 14.6K | 88.8K | 1.017 | 4.2 | 28.8 / 24.5 | 16 % |
| P1 pace in store bytes | 1 / 0 | 43 (6/8) | 11.8 | 6.21 | 24.8K / 12.2K | 88.1K | 1.003 | 4.2 | 23.4 / 37.2 | 23 % |
| P2 growth pass 1 GiB | 4 / 6 | 17 (8/8) | 30.5 | 1.02 | 25.5K / 15.0K | 94.2K | 1.017 | 4.0 | 33.0 / 27.9 | 17 % |
| P2 growth pass 0.25 GiB | 3 / 8 | 15 (8/8) | 31.1 | 9.39 | 22.9K / 13.6K | 92.1K | 1.017 | 4.1 | 28.4 / 30.5 | 15 % |
| G9 grace from death | 4 / 9 | 13 (8/8) | 19.1 | 1.02 | 28.5K / 17.0K | 96.4K | 1.017 | 4.2 | 29.3 / 40.4 | 22 % |
| P3 grace 15 s | 4 / 9 | 13 (8/8) | 29.0 | 1.02 | 29.1K / 19.7K | 96.4K | 1.017 | 4.2 | 45.0 / 29.6 | 19 % |
| P3 grace 10 s | 4 / 8 | 13 (8/8) | 15.8 | 1.02 | 29.8K / 28.2K | 96.4K | 1.016 | 4.2 | 58.5 / 23.7 | 26 % |
| P3 grace 5 s | 3 / 7 | 13 (8/8) | 29.7 | 1.02 | 32.9K / 26.3K | 96.4K | 1.016 | 4.2 | 48.2 / 30.1 | 20 % |
| P3 grace 2 s | 2 / 7 | 13 (8/8) | 29.7 | 1.02 | 32.9K / 26.3K | 96.4K | 1.017 | 4.2 | 48.2 / 26.4 | 20 % |
| P3 grace 0 s | 2 / 7 | 13 (8/8) | 29.7 | 1.02 | 32.9K / 26.3K | 96.4K | 1.017 | 4.2 | 48.2 / 26.4 | 20 % |
| P4 grace 5 s + gap 1 s | 4 / 8 | 13 (8/8) | 9.1 | 1.02 | 47.3K / 39.2K | 96.4K | 1.017 | 4.2 | 73.6 / 52.5 | 40 % |
| P4 grace 0 s + gap 1 s | 3 / 9 | 13 (8/8) | 4.7 | 1.02 | 52.4K / 38.3K | 96.4K | 1.016 | 4.2 | 73.0 / 57.0 | 40 % |
| G5 threshold from headroom | 5 / 6 | 13 (8/8) | 32.1 | 1.02 | 25.0K / 14.3K | 96.4K | 1.017 | 4.2 | 35.5 / 26.0 | 14 % |
| G1 batched index work | 4 / 9 | 14 (8/8) | 29.6 | 1.02 | 26.4K / 18.6K | 96.2K | 1.019 | 4.2 | 31.3 / 41.7 | 7 % |
| G3 incremental mark | 3 / 8 | 13 (8/8) | 30.4 | 1.02 | 23.8K / 15.7K | 96.4K | 1.022 | 4.2 | 31.5 / 28.0 | 14 % |
| G4 two collector threads | 4 / 6 | 15 (7/8) | 30.2 | 1.02 | 24.2K / 14.7K | 95.3K | 1.017 | 4.2 | 29.5 / 30.2 | 39 % |
| G1+G3+G4 | 4 / 8 | 15 (8/8) | 23.6 | 1.02 | 26.2K / 19.2K | 94.9K | 1.021 | 4.2 | 29.3 / 44.5 | 11 % |
| G7 punch, grace 30 s | 2 / 5 | 14 (7/8) | 9.6 | 1.02 | 28.0K / 21.0K | 94.1K | 1.014 | 4.2 | 56.5 / 0.0 | 17 % |
| G7 punch, grace 5 s | 3 / 7 | 15 (8/8) | 18.0 | 1.02 | 44.5K / 28.1K | 94.1K | 1.015 | 4.2 | 69.5 / 0.0 | 25 % |
| combo A: P2 + grace 5 s + gap 1 s | 3 / 9 | 14 (8/8) | 3.0 | 1.01 | 49.6K / 39.2K | 94.8K | 1.016 | 4.0 | 66.2 / 61.7 | 42 % |
| combo B: A + P0a | 1 / 0 | 15 (4/8) | 2.5 | 0.64 | 50.2K / 38.4K | 95.3K | 1.011 | 4.0 | 67.2 / 62.7 | 43 % |
| combo C: P2 + P1 | 1 / 0 | 38 (8/8) | 16.9 | 7.06 | 23.8K / 12.0K | 90.3K | 1.003 | 4.1 | 20.3 / 37.9 | 24 % |
| combo D: P2 + G9 + P0a | 1 / 3 | 19 (8/8) | 19.2 | 1.02 | 31.4K / 18.4K | 94.3K | 1.016 | 4.0 | 27.7 / 44.4 | 24 % |
| combo E: G7 + P2 + grace 5 s | 1 / 7 | 37 (8/8) | 1.0 | 0.89 | 58.8K / 47.2K | 95.7K | 1.012 | 4.2 | 95.3 / 0.0 | 43 % |

### ram-64k, amplification 2.8 (merge 0.4)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 1 | 29 (8/8) | 12.1 | 1.02 | 40.9K / 26.1K | 110.0K | 1.010 | 2.7 | 10.2 / 46.3 | 24 % |
| gate-gc | 2 / 3 | 27 (8/8) | 13.7 | 1.02 | 43.5K / 31.7K | 110.0K | 1.008 | 2.7 | 11.8 / 46.4 | 26 % |
| leadzone | 1 / 1 | 29 (8/8) | 12.1 | 1.02 | 40.9K / 26.1K | 110.0K | 1.010 | 2.7 | 10.2 / 46.3 | 24 % |
| P0a bound compressed | 1 / 0 | 31 (8/8) | 13.8 | 1.02 | 40.3K / 26.2K | 110.0K | 1.011 | 2.7 | 10.1 / 46.2 | 24 % |
| P0b true-up early | 1 / 0 | 29 (8/8) | 13.8 | 1.02 | 40.5K / 26.2K | 110.0K | 1.011 | 2.7 | 10.0 / 46.0 | 24 % |
| P0c limit anchored on disk | 1 / 0 | 31 (8/8) | 13.7 | 0.92 | 39.8K / 26.3K | 110.0K | 1.012 | 2.7 | 9.9 / 46.1 | 24 % |
| P1 pace in store bytes | 0 / 0 | none | 2.4 | 1.91 | 39.0K / 25.5K | 110.0K | 0.990 | 2.7 | 8.9 / 45.7 | 24 % |
| P2 growth pass 1 GiB | 1 / 3 | 30 (8/8) | 14.0 | 0.92 | 44.6K / 27.2K | 110.0K | 1.014 | 2.6 | 8.7 / 51.3 | 20 % |
| P2 growth pass 0.25 GiB | 1 / 0 | 28 (8/8) | 11.1 | 0.64 | 41.0K / 26.6K | 110.0K | 1.010 | 2.6 | 10.5 / 47.8 | 23 % |
| G9 grace from death | 1 / 0 | 29 (8/8) | 10.9 | 1.02 | 44.0K / 27.9K | 110.0K | 1.010 | 2.7 | 9.1 / 56.2 | 25 % |
| P3 grace 15 s | 0 / 0 | 24 (1/8) | 1.0 | 1.01 | 51.0K / 35.8K | 110.0K | 0.993 | 2.7 | 11.9 / 60.7 | 27 % |
| P3 grace 10 s | 0 / 0 | 24 (1/8) | 1.0 | 1.01 | 53.0K / 43.9K | 110.0K | 0.993 | 2.7 | 18.9 / 61.5 | 28 % |
| P3 grace 5 s | 0 / 0 | none | 0.2 | 0.22 | 60.7K / 42.4K | 110.0K | 0.976 | 2.7 | 14.7 / 68.5 | 27 % |
| P3 grace 2 s | 0 / 0 | none | 0.2 | 0.22 | 60.7K / 42.4K | 110.0K | 0.976 | 2.7 | 14.7 / 68.5 | 27 % |
| P3 grace 0 s | 0 / 0 | none | 0.2 | 0.22 | 60.7K / 42.4K | 110.0K | 0.976 | 2.7 | 14.7 / 68.5 | 27 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 0.2 | 0.14 | 72.4K / 62.2K | 110.0K | 0.966 | 2.7 | 20.7 / 94.2 | 38 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 0.2 | 0.12 | 76.2K / 62.4K | 110.0K | 0.966 | 2.7 | 20.9 / 98.5 | 38 % |
| G5 threshold from headroom | 1 / 0 | 29 (8/8) | 11.8 | 1.02 | 42.8K / 25.8K | 110.0K | 1.015 | 2.7 | 9.0 / 50.7 | 24 % |
| G1 batched index work | 1 / 0 | 26 (8/8) | 18.8 | 1.02 | 40.8K / 27.2K | 110.0K | 1.009 | 2.7 | 9.2 / 59.3 | 10 % |
| G3 incremental mark | 1 / 1 | 30 (8/8) | 13.3 | 0.78 | 42.0K / 27.6K | 110.0K | 1.013 | 2.7 | 8.6 / 50.7 | 21 % |
| G4 two collector threads | 1 / 1 | 31 (8/8) | 13.8 | 0.92 | 40.7K / 26.2K | 110.0K | 1.012 | 2.7 | 9.4 / 45.7 | 60 % |
| G1+G3+G4 | 2 / 0 | 25 (8/8) | 17.7 | 1.02 | 41.9K / 30.8K | 110.0K | 1.008 | 2.7 | 6.6 / 61.6 | 20 % |
| G7 punch, grace 30 s | 0 / 0 | 44 (3/8) | 1.0 | 1.02 | 42.8K / 36.3K | 110.0K | 1.000 | 2.8 | 48.2 / 0.0 | 20 % |
| G7 punch, grace 5 s | 0 / 0 | none | 0.3 | 0.21 | 78.4K / 55.1K | 110.0K | 0.976 | 2.8 | 39.2 / 0.0 | 28 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 0.3 | 0.22 | 72.1K / 61.7K | 110.0K | 0.974 | 2.6 | 22.2 / 96.5 | 38 % |
| combo B: A + P0a | 0 / 0 | none | 0.2 | 0.15 | 72.2K / 62.2K | 110.0K | 0.968 | 2.6 | 24.7 / 93.4 | 38 % |
| combo C: P2 + P1 | 0 / 0 | none | 2.8 | 1.92 | 38.9K / 25.4K | 110.0K | 0.991 | 2.6 | 7.5 / 45.4 | 25 % |
| combo D: P2 + G9 + P0a | 1 / 0 | 30 (8/8) | 7.9 | 0.76 | 45.4K / 28.1K | 110.0K | 1.010 | 2.6 | 10.8 / 53.7 | 24 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.1 | 0.10 | 94.3K / 74.9K | 110.0K | 0.956 | 2.8 | 47.5 / 0.0 | 37 % |

### ram-16k, high amplification (merge 1.0)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 2 / 2 | 40 (8/8) | 6.1 | 1.02 | 50.0K / 29.9K | 110.0K | 1.009 | 2.0 | 2.7 / 47.7 | 26 % |
| gate-gc | 2 / 2 | 39 (7/8) | 5.9 | 1.02 | 50.2K / 31.3K | 110.0K | 1.004 | 2.0 | 3.1 / 48.1 | 31 % |
| leadzone | 2 / 2 | 40 (8/8) | 6.1 | 1.02 | 50.0K / 29.9K | 110.0K | 1.009 | 2.0 | 2.7 / 47.7 | 26 % |
| P0a bound compressed | 1 / 2 | 41 (8/8) | 6.1 | 1.02 | 50.3K / 29.7K | 110.0K | 1.007 | 2.0 | 2.8 / 47.6 | 26 % |
| P0b true-up early | 1 / 2 | 39 (8/8) | 6.0 | 1.02 | 50.2K / 29.7K | 110.0K | 1.009 | 2.0 | 3.2 / 47.4 | 26 % |
| P0c limit anchored on disk | 1 / 1 | 41 (8/8) | 6.1 | 1.02 | 50.4K / 29.7K | 110.0K | 1.007 | 2.0 | 2.7 / 47.8 | 25 % |
| P1 pace in store bytes | 0 / 0 | none | 1.4 | 1.26 | 50.2K / 28.2K | 110.0K | 0.988 | 2.0 | 2.3 / 47.1 | 28 % |
| P2 growth pass 1 GiB | 2 / 0 | 41 (8/8) | 6.0 | 1.02 | 50.2K / 28.1K | 110.0K | 1.008 | 2.0 | 2.8 / 48.5 | 26 % |
| P2 growth pass 0.25 GiB | 1 / 0 | 41 (8/8) | 6.0 | 1.02 | 49.7K / 27.6K | 110.0K | 1.007 | 1.9 | 2.5 / 48.6 | 27 % |
| G9 grace from death | 1 / 0 | 39 (5/8) | 1.0 | 1.01 | 52.2K / 36.8K | 110.0K | 1.002 | 2.0 | 3.3 / 52.4 | 28 % |
| P3 grace 15 s | 0 / 0 | none | 0.4 | 0.34 | 61.1K / 41.9K | 110.0K | 0.984 | 2.0 | 2.6 / 61.2 | 27 % |
| P3 grace 10 s | 0 / 0 | none | 0.7 | 0.34 | 64.1K / 42.1K | 110.0K | 0.991 | 2.0 | 8.7 / 60.0 | 27 % |
| P3 grace 5 s | 0 / 0 | none | 0.2 | 0.17 | 68.8K / 51.7K | 110.0K | 0.966 | 2.0 | 4.0 / 67.7 | 28 % |
| P3 grace 2 s | 0 / 0 | none | 0.2 | 0.17 | 68.8K / 51.7K | 110.0K | 0.966 | 2.0 | 4.0 / 67.7 | 28 % |
| P3 grace 0 s | 0 / 0 | none | 0.2 | 0.17 | 68.8K / 51.7K | 110.0K | 0.966 | 2.0 | 4.0 / 67.7 | 28 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 0.1 | 0.11 | 77.1K / 62.5K | 110.0K | 0.953 | 2.0 | 3.0 / 87.2 | 33 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 0.1 | 0.11 | 77.1K / 62.5K | 110.0K | 0.953 | 2.0 | 3.0 / 87.2 | 33 % |
| G5 threshold from headroom | 2 / 1 | 40 (8/8) | 6.3 | 1.02 | 49.2K / 27.9K | 110.0K | 1.016 | 2.0 | 4.2 / 48.7 | 26 % |
| G1 batched index work | 2 / 0 | 39 (8/8) | 5.8 | 1.02 | 50.6K / 35.3K | 110.0K | 1.004 | 2.0 | 1.6 / 60.6 | 16 % |
| G3 incremental mark | 1 / 2 | 39 (8/8) | 10.5 | 1.02 | 50.6K / 36.6K | 110.0K | 1.006 | 2.0 | 2.8 / 49.9 | 21 % |
| G4 two collector threads | 2 / 2 | 39 (8/8) | 6.1 | 1.02 | 50.3K / 28.9K | 110.0K | 1.009 | 2.0 | 2.6 / 47.6 | 61 % |
| G1+G3+G4 | 2 / 0 | 39 (8/8) | 5.2 | 1.02 | 50.4K / 39.0K | 110.0K | 1.002 | 2.0 | 1.2 / 62.7 | 23 % |
| G7 punch, grace 30 s | 0 / 0 | none | 0.5 | 0.46 | 56.0K / 41.9K | 110.0K | 0.988 | 2.0 | 47.1 / 0.0 | 23 % |
| G7 punch, grace 5 s | 0 / 0 | none | 0.2 | 0.11 | 85.8K / 67.3K | 110.0K | 0.962 | 2.0 | 31.6 / 0.0 | 25 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 0.1 | 0.10 | 77.0K / 62.6K | 110.0K | 0.950 | 2.0 | 2.9 / 88.8 | 33 % |
| combo B: A + P0a | 0 / 0 | none | 0.1 | 0.11 | 77.1K / 62.7K | 110.0K | 0.948 | 2.0 | 2.8 / 89.0 | 33 % |
| combo C: P2 + P1 | 0 / 0 | none | 1.4 | 1.32 | 48.8K / 27.4K | 110.0K | 0.987 | 2.0 | 2.7 / 47.7 | 27 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 1.0 | 1.01 | 55.8K / 35.7K | 110.0K | 0.997 | 2.0 | 3.4 / 53.1 | 27 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.1 | 0.09 | 96.4K / 79.6K | 110.0K | 0.945 | 2.0 | 16.2 / 0.0 | 29 % |

### ram-16k, lower amplification (merge 0.66)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 0 | none | 1.0 | 1.01 | 66.5K / 40.5K | 110.0K | 0.992 | 1.3 | 0.9 / 49.8 | 28 % |
| gate-gc | 0 / 0 | none | 0.7 | 0.71 | 64.8K / 42.6K | 110.0K | 0.991 | 1.3 | 1.7 / 50.4 | 31 % |
| leadzone | 0 / 0 | none | 1.0 | 1.01 | 66.5K / 40.3K | 110.0K | 0.992 | 1.3 | 0.9 / 49.8 | 28 % |
| P0a bound compressed | 0 / 0 | none | 1.0 | 1.01 | 66.5K / 40.3K | 110.0K | 0.992 | 1.3 | 0.9 / 49.6 | 28 % |
| P0b true-up early | 0 / 0 | none | 1.0 | 1.01 | 66.6K / 40.5K | 110.0K | 0.992 | 1.3 | 0.8 / 49.5 | 28 % |
| P0c limit anchored on disk | 0 / 0 | none | 1.0 | 0.97 | 66.6K / 40.6K | 110.0K | 0.991 | 1.3 | 0.8 / 49.5 | 28 % |
| P1 pace in store bytes | 0 / 0 | none | 0.8 | 0.73 | 66.5K / 40.7K | 110.0K | 0.987 | 1.3 | 0.8 / 49.3 | 28 % |
| P2 growth pass 1 GiB | 0 / 0 | none | 0.8 | 0.59 | 65.7K / 40.7K | 110.0K | 0.991 | 1.3 | 0.9 / 48.6 | 26 % |
| P2 growth pass 0.25 GiB | 0 / 0 | none | 0.7 | 0.48 | 66.7K / 40.7K | 110.0K | 0.990 | 1.3 | 0.7 / 50.3 | 27 % |
| G9 grace from death | 0 / 0 | none | 0.5 | 0.45 | 70.9K / 52.6K | 110.0K | 0.986 | 1.3 | 0.3 / 51.4 | 29 % |
| P3 grace 15 s | 0 / 0 | none | 0.2 | 0.17 | 77.4K / 60.8K | 110.0K | 0.962 | 1.3 | 0.2 / 60.4 | 27 % |
| P3 grace 10 s | 0 / 0 | none | 0.2 | 0.10 | 86.9K / 72.0K | 110.0K | 0.972 | 1.3 | 4.0 / 65.0 | 28 % |
| P3 grace 5 s | 0 / 0 | none | 0.1 | 0.07 | 88.5K / 73.7K | 110.0K | 0.938 | 1.3 | 0.5 / 66.3 | 27 % |
| P3 grace 2 s | 0 / 0 | none | 0.1 | 0.07 | 88.5K / 73.7K | 110.0K | 0.938 | 1.3 | 0.5 / 66.3 | 27 % |
| P3 grace 0 s | 0 / 0 | none | 0.1 | 0.07 | 88.5K / 73.7K | 110.0K | 0.938 | 1.3 | 0.5 / 66.3 | 27 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 0.1 | 0.05 | 97.4K / 88.1K | 110.0K | 0.930 | 1.3 | 0.9 / 77.9 | 31 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 0.1 | 0.05 | 97.4K / 88.1K | 110.0K | 0.930 | 1.3 | 0.9 / 77.9 | 31 % |
| G5 threshold from headroom | 0 / 0 | 49 (3/8) | 1.0 | 0.79 | 63.1K / 41.0K | 110.0K | 0.998 | 1.3 | 0.6 / 49.4 | 28 % |
| G1 batched index work | 0 / 0 | none | 0.8 | 0.81 | 65.1K / 53.0K | 110.0K | 0.991 | 1.3 | 0.1 / 53.7 | 17 % |
| G3 incremental mark | 0 / 0 | none | 0.7 | 0.74 | 69.8K / 47.7K | 110.0K | 0.989 | 1.3 | 0.3 / 51.2 | 22 % |
| G4 two collector threads | 0 / 0 | none | 1.0 | 0.99 | 67.1K / 41.1K | 110.0K | 0.992 | 1.3 | 0.8 / 49.6 | 69 % |
| G1+G3+G4 | 0 / 0 | none | 0.6 | 0.59 | 66.2K / 59.4K | 110.0K | 0.987 | 1.3 | 0.0 / 59.6 | 30 % |
| G7 punch, grace 30 s | 0 / 0 | none | 0.2 | 0.19 | 60.8K / 65.0K | 110.0K | 0.972 | 1.3 | 37.7 / 0.0 | 19 % |
| G7 punch, grace 5 s | 0 / 0 | none | 0.1 | 0.12 | 84.8K / 95.7K | 110.0K | 0.954 | 1.3 | 25.7 / 0.0 | 22 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 0.1 | 0.06 | 95.5K / 87.2K | 110.0K | 0.932 | 1.3 | 1.1 / 74.7 | 32 % |
| combo B: A + P0a | 0 / 0 | none | 0.1 | 0.06 | 95.6K / 87.4K | 110.0K | 0.930 | 1.3 | 1.2 / 80.2 | 32 % |
| combo C: P2 + P1 | 0 / 0 | none | 0.6 | 0.48 | 65.7K / 39.8K | 110.0K | 0.985 | 1.3 | 0.8 / 48.2 | 27 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 0.3 | 0.22 | 70.5K / 53.4K | 110.0K | 0.972 | 1.3 | 0.3 / 52.1 | 28 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.1 | 0.02 | 109.1K / 100.3K | 110.0K | 0.926 | 1.3 | 36.9 / 0.0 | 25 % |

### m15-8g, high amplification (merge 1.0)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 3 / 4 | 17 (8/8) | 32.6 | 21.45 | 20.9K / 14.8K | 83.2K | 1.035 | 2.0 | 0.0 / 33.9 | 16 % |
| gate-gc | 2 / 3 | 14 (8/8) | 40.5 | 3.70 | 21.0K / 13.4K | 83.2K | 1.035 | 2.0 | 0.0 / 31.4 | 13 % |
| leadzone | 3 / 4 | 17 (8/8) | 32.6 | 21.45 | 20.9K / 14.8K | 83.2K | 1.035 | 2.0 | 0.0 / 33.9 | 16 % |
| P0a bound compressed | 2 / 2 | 18 (8/8) | 32.6 | 27.09 | 21.1K / 14.8K | 83.3K | 1.035 | 2.0 | 0.0 / 33.0 | 14 % |
| P0b true-up early | 3 / 5 | 17 (8/8) | 32.4 | 14.58 | 20.9K / 14.8K | 83.3K | 1.035 | 2.0 | 0.0 / 33.9 | 16 % |
| P0c limit anchored on disk | 3 / 4 | 17 (8/8) | 32.6 | 21.45 | 20.9K / 14.8K | 83.2K | 1.035 | 2.0 | 0.0 / 33.9 | 16 % |
| P1 pace in store bytes | 2 / 3 | 22 (8/8) | 28.9 | 12.34 | 21.5K / 14.4K | 81.0K | 1.030 | 2.0 | 0.0 / 34.0 | 20 % |
| P2 growth pass 1 GiB | 3 / 3 | 14 (8/8) | 32.2 | 21.57 | 19.7K / 14.9K | 83.5K | 1.035 | 2.0 | 0.0 / 33.8 | 17 % |
| P2 growth pass 0.25 GiB | 3 / 4 | 20 (8/8) | 32.2 | 14.49 | 19.8K / 14.7K | 83.5K | 1.035 | 2.0 | 0.0 / 33.6 | 17 % |
| G9 grace from death | 3 / 4 | 17 (8/8) | 32.3 | 27.40 | 20.8K / 14.8K | 83.2K | 1.035 | 2.0 | 0.0 / 34.0 | 15 % |
| P3 grace 15 s | 2 / 3 | 17 (8/8) | 32.9 | 21.68 | 21.1K / 15.1K | 83.2K | 1.035 | 2.0 | 0.0 / 34.0 | 12 % |
| P3 grace 10 s | 3 / 2 | 17 (8/8) | 13.4 | 1.02 | 27.4K / 22.3K | 83.2K | 1.034 | 2.0 | 0.0 / 46.5 | 24 % |
| P3 grace 5 s | 2 / 5 | 17 (8/8) | 13.3 | 12.40 | 28.2K / 27.2K | 83.2K | 1.028 | 2.0 | 0.0 / 52.7 | 23 % |
| P3 grace 2 s | 2 / 5 | 17 (8/8) | 13.3 | 12.40 | 28.2K / 27.2K | 83.2K | 1.028 | 2.0 | 0.0 / 52.7 | 23 % |
| P3 grace 0 s | 2 / 5 | 17 (8/8) | 13.3 | 12.40 | 28.2K / 27.2K | 83.2K | 1.028 | 2.0 | 0.0 / 52.7 | 23 % |
| P4 grace 5 s + gap 1 s | 2 / 0 | 17 (8/8) | 7.7 | 0.55 | 42.8K / 34.7K | 83.2K | 1.034 | 2.0 | 0.0 / 69.6 | 31 % |
| P4 grace 0 s + gap 1 s | 1 / 0 | 16 (6/8) | 7.0 | 0.34 | 43.4K / 34.8K | 83.2K | 1.026 | 2.0 | 0.0 / 69.7 | 31 % |
| G5 threshold from headroom | 3 / 3 | 17 (8/8) | 32.6 | 31.33 | 20.9K / 14.4K | 83.2K | 1.035 | 2.0 | 0.0 / 31.4 | 12 % |
| G1 batched index work | 2 / 3 | 17 (8/8) | 31.8 | 20.82 | 21.1K / 14.7K | 83.2K | 1.035 | 2.0 | 0.0 / 35.2 | 14 % |
| G3 incremental mark | 3 / 4 | 15 (8/8) | 29.6 | 21.42 | 22.4K / 14.4K | 83.3K | 1.035 | 2.0 | 0.0 / 35.3 | 10 % |
| G4 two collector threads | 3 / 4 | 17 (8/8) | 32.4 | 21.38 | 21.0K / 14.6K | 83.2K | 1.035 | 2.0 | 0.0 / 33.6 | 26 % |
| G1+G3+G4 | 3 / 4 | 15 (8/8) | 34.4 | 9.51 | 20.8K / 15.5K | 83.4K | 1.035 | 2.0 | 0.0 / 36.7 | 9 % |
| G7 punch, grace 30 s | 4 / 5 | 22 (8/8) | 6.2 | 1.02 | 23.9K / 20.4K | 83.4K | 1.013 | 2.0 | 1.0 / 0.0 | 17 % |
| G7 punch, grace 5 s | 3 / 0 | 22 (8/8) | 6.1 | 1.01 | 35.2K / 42.4K | 83.4K | 1.010 | 2.0 | 0.7 / 0.0 | 21 % |
| combo A: P2 + grace 5 s + gap 1 s | 3 / 1 | 18 (8/8) | 10.5 | 0.82 | 38.9K / 34.0K | 83.5K | 1.034 | 2.0 | 0.0 / 67.7 | 31 % |
| combo B: A + P0a | 2 / 1 | 18 (8/8) | 10.5 | 0.65 | 42.2K / 33.7K | 83.6K | 1.034 | 2.0 | 0.0 / 69.0 | 31 % |
| combo C: P2 + P1 | 1 / 4 | 22 (8/8) | 28.1 | 12.24 | 21.1K / 14.3K | 81.4K | 1.028 | 2.0 | 0.0 / 32.8 | 20 % |
| combo D: P2 + G9 + P0a | 2 / 3 | 22 (8/8) | 32.2 | 27.32 | 19.7K / 14.7K | 83.6K | 1.035 | 2.0 | 0.0 / 33.4 | 16 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.3 | 0.18 | 62.4K / 52.5K | 83.5K | 0.985 | 2.0 | 0.4 / 0.0 | 29 % |

### m15-8g, lower amplification (merge 0.66)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 2 / 3 | 25 (8/8) | 32.0 | 2.34 | 30.3K / 21.7K | 110.0K | 1.035 | 1.3 | 0.0 / 31.6 | 19 % |
| gate-gc | 2 / 5 | 18 (8/8) | 29.7 | 1.02 | 32.2K / 22.2K | 110.0K | 1.029 | 1.3 | 0.0 / 35.5 | 20 % |
| leadzone | 2 / 3 | 25 (8/8) | 32.0 | 2.55 | 30.3K / 22.2K | 110.0K | 1.035 | 1.3 | 0.0 / 31.0 | 19 % |
| P0a bound compressed | 2 / 2 | 26 (8/8) | 31.9 | 2.40 | 30.6K / 21.9K | 110.0K | 1.035 | 1.3 | 0.0 / 32.1 | 19 % |
| P0b true-up early | 2 / 3 | 25 (8/8) | 31.9 | 2.26 | 30.3K / 21.6K | 110.0K | 1.034 | 1.3 | 0.0 / 31.6 | 19 % |
| P0c limit anchored on disk | 2 / 3 | 25 (8/8) | 32.0 | 2.34 | 30.3K / 21.7K | 110.0K | 1.035 | 1.3 | 0.0 / 31.6 | 19 % |
| P1 pace in store bytes | 1 / 3 | 26 (8/8) | 22.5 | 1.33 | 31.7K / 21.4K | 110.0K | 1.028 | 1.3 | 0.0 / 32.9 | 20 % |
| P2 growth pass 1 GiB | 1 / 4 | 23 (8/8) | 22.2 | 2.00 | 33.0K / 21.5K | 110.0K | 1.032 | 1.3 | 0.0 / 32.7 | 19 % |
| P2 growth pass 0.25 GiB | 1 / 4 | 25 (8/8) | 22.6 | 2.71 | 33.2K / 21.5K | 110.0K | 1.034 | 1.3 | 0.0 / 33.2 | 19 % |
| G9 grace from death | 2 / 4 | 25 (8/8) | 32.0 | 1.02 | 30.9K / 23.5K | 110.0K | 1.035 | 1.3 | 0.0 / 35.5 | 19 % |
| P3 grace 15 s | 2 / 6 | 25 (8/8) | 13.8 | 1.02 | 36.0K / 27.2K | 110.0K | 1.035 | 1.3 | 0.0 / 40.1 | 22 % |
| P3 grace 10 s | 2 / 5 | 25 (8/8) | 13.4 | 1.02 | 37.9K / 32.1K | 110.0K | 1.035 | 1.3 | 0.0 / 42.2 | 22 % |
| P3 grace 5 s | 0 / 0 | 24 (2/8) | 1.0 | 0.28 | 47.0K / 41.7K | 110.0K | 0.997 | 1.3 | 0.0 / 52.9 | 24 % |
| P3 grace 2 s | 0 / 0 | 24 (2/8) | 1.0 | 0.28 | 47.0K / 41.7K | 110.0K | 0.997 | 1.3 | 0.0 / 52.9 | 24 % |
| P3 grace 0 s | 0 / 0 | 24 (2/8) | 1.0 | 0.28 | 47.0K / 41.7K | 110.0K | 0.997 | 1.3 | 0.0 / 52.9 | 24 % |
| P4 grace 5 s + gap 1 s | 1 / 0 | 21 (4/8) | 5.3 | 0.31 | 59.0K / 51.4K | 110.0K | 1.013 | 1.3 | 0.0 / 67.8 | 29 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | 22 (2/8) | 0.5 | 0.16 | 60.4K / 51.9K | 110.0K | 0.989 | 1.3 | 0.0 / 68.5 | 29 % |
| G5 threshold from headroom | 2 / 3 | 25 (8/8) | 32.3 | 5.11 | 29.0K / 20.6K | 110.0K | 1.035 | 1.3 | 0.0 / 30.4 | 16 % |
| G1 batched index work | 2 / 4 | 24 (8/8) | 27.8 | 1.02 | 31.0K / 20.9K | 110.0K | 1.032 | 1.3 | 0.0 / 35.4 | 15 % |
| G3 incremental mark | 1 / 6 | 24 (8/8) | 23.9 | 1.02 | 32.9K / 21.4K | 110.0K | 1.033 | 1.3 | 0.0 / 35.8 | 12 % |
| G4 two collector threads | 2 / 4 | 25 (8/8) | 28.9 | 1.81 | 30.7K / 21.5K | 110.0K | 1.035 | 1.3 | 0.0 / 33.5 | 34 % |
| G1+G3+G4 | 2 / 1 | 22 (8/8) | 24.1 | 1.01 | 31.9K / 21.4K | 110.0K | 1.028 | 1.3 | 0.0 / 36.1 | 12 % |
| G7 punch, grace 30 s | 1 / 0 | 38 (8/8) | 1.0 | 0.84 | 32.6K / 29.7K | 110.0K | 1.002 | 1.3 | 0.8 / 0.0 | 17 % |
| G7 punch, grace 5 s | 1 / 0 | 38 (5/8) | 1.0 | 0.49 | 54.3K / 63.4K | 110.0K | 1.001 | 1.3 | 0.3 / 0.0 | 19 % |
| combo A: P2 + grace 5 s + gap 1 s | 1 / 0 | 19 (5/8) | 1.0 | 0.27 | 59.9K / 51.6K | 110.0K | 1.005 | 1.3 | 0.0 / 67.7 | 30 % |
| combo B: A + P0a | 1 / 0 | 19 (6/8) | 1.0 | 0.23 | 58.6K / 50.8K | 110.0K | 1.003 | 1.3 | 0.0 / 67.7 | 30 % |
| combo C: P2 + P1 | 1 / 4 | 23 (8/8) | 22.4 | 1.33 | 32.7K / 20.9K | 110.0K | 1.023 | 1.3 | 0.0 / 32.3 | 21 % |
| combo D: P2 + G9 + P0a | 1 / 5 | 22 (8/8) | 15.3 | 1.02 | 36.0K / 23.7K | 110.0K | 1.028 | 1.3 | 0.0 / 36.0 | 21 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.1 | 0.08 | 86.7K / 78.9K | 110.0K | 0.944 | 1.3 | 0.3 / 0.0 | 27 % |

### netvol-64k, high amplification (merge 1.0)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| gate-gc | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| leadzone | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P0a bound compressed | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.146 | 7.5 | 0.0 / 0.0 | 1 % |
| P0b true-up early | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P0c limit anchored on disk | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P1 pace in store bytes | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P2 growth pass 1 GiB | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P2 growth pass 0.25 GiB | 0 / 0 | none | 5.9 | 4.38 | 1.8K / 0.0K | 7.9K | 0.146 | 7.5 | 0.0 / 0.0 | 1 % |
| G9 grace from death | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P3 grace 15 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P3 grace 10 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P3 grace 5 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P3 grace 2 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P3 grace 0 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| G5 threshold from headroom | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| G1 batched index work | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| G3 incremental mark | 0 / 0 | none | 3.4 | 3.35 | 1.8K / 0.0K | 7.6K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| G4 two collector threads | 0 / 0 | none | 5.0 | 4.20 | 2.0K / 1.2K | 7.6K | 0.137 | 7.5 | 0.0 / 7.4 | 76 % |
| G1+G3+G4 | 0 / 0 | none | 5.1 | 3.23 | 2.0K / 1.2K | 7.6K | 0.140 | 7.5 | 0.0 / 8.0 | 70 % |
| G7 punch, grace 30 s | 0 / 0 | none | 4.4 | 4.38 | 2.1K / 1.2K | 7.9K | 0.110 | 7.5 | 0.0 / 0.0 | 0 % |
| G7 punch, grace 5 s | 0 / 0 | none | 4.4 | 4.38 | 2.1K / 1.2K | 7.9K | 0.110 | 7.5 | 0.0 / 0.0 | 0 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| combo B: A + P0a | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.146 | 7.5 | 0.0 / 0.0 | 1 % |
| combo C: P2 + P1 | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.149 | 7.5 | 0.0 / 0.0 | 1 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 4.4 | 4.38 | 1.8K / 0.0K | 7.9K | 0.146 | 7.5 | 0.0 / 0.0 | 1 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 4.4 | 4.38 | 2.1K / 1.2K | 7.9K | 0.110 | 7.5 | 0.0 / 0.0 | 0 % |

### netvol-64k, lower amplification (merge 0.66)

| candidate | stops ff / steady | first stop s | longest s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| gate-gc | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| leadzone | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P0a bound compressed | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.139 | 5.0 | 0.0 / 0.0 | 5 % |
| P0b true-up early | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.140 | 5.0 | 0.0 / 0.0 | 5 % |
| P0c limit anchored on disk | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P1 pace in store bytes | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P2 growth pass 1 GiB | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P2 growth pass 0.25 GiB | 0 / 0 | none | 5.2 | 3.37 | 2.3K / 0.0K | 7.6K | 0.140 | 5.0 | 0.0 / 0.0 | 5 % |
| G9 grace from death | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P3 grace 15 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P3 grace 10 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P3 grace 5 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P3 grace 2 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P3 grace 0 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| G5 threshold from headroom | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| G1 batched index work | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| G3 incremental mark | 0 / 0 | none | 5.1 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| G4 two collector threads | 0 / 0 | none | 4.5 | 3.23 | 2.5K / 1.7K | 7.6K | 0.134 | 5.0 | 0.0 / 7.2 | 76 % |
| G1+G3+G4 | 0 / 0 | none | 4.6 | 3.23 | 2.6K / 1.7K | 7.6K | 0.130 | 5.0 | 0.0 / 7.6 | 70 % |
| G7 punch, grace 30 s | 0 / 0 | none | 3.1 | 3.12 | 2.7K / 1.8K | 7.6K | 0.106 | 5.0 | 0.0 / 0.0 | 1 % |
| G7 punch, grace 5 s | 0 / 0 | none | 3.1 | 3.12 | 2.7K / 1.8K | 7.6K | 0.106 | 5.0 | 0.0 / 0.0 | 1 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| combo B: A + P0a | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.139 | 5.0 | 0.0 / 0.0 | 5 % |
| combo C: P2 + P1 | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.141 | 5.0 | 0.0 / 0.0 | 5 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 5.0 | 3.26 | 2.3K / 0.0K | 7.6K | 0.139 | 5.0 | 0.0 / 0.0 | 5 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 3.4 | 3.12 | 2.7K / 1.8K | 7.6K | 0.106 | 5.0 | 0.0 / 0.0 | 1 % |

## Gates and deviations

* `tools/ci.sh` on the remote: `ci: all green` (fmt, clippy `-D warnings`, `cargo test --workspace` including 25 simulator tests, the crashpoint builds, ownership
  `[spacesim]`). The simulator's own tests run in 1.4 s. No kill loops: no product behaviour changed.
* `tools/ownership.toml` has a `[spacesim]` entry with exactly the changed paths (`Cargo.lock` in a separate `deps:` commit).
* Nothing in `lunix-gc` or `lunix-index` was touched. The only product change is the visibility change in `lunix-engine/src/{cut,lib}.rs` above.
* The simulator's G1 cost constants (`condemn_ms` 5.5, `delete_ms` 13) are the gcrate figures; a pass log of the p1/m16 RAM run shows the real phase times
  (condemn about 4 ms and delete about 5 ms per segment unit, mark 12 ms at that size), to be replaced by p1/gcbatch's measurements.
* The last coordinator message (remote CPUs 0-7 only) arrived while the full CI run (pinned 0-15 when it started) was finishing; nothing was run on 8-15 afterwards.
