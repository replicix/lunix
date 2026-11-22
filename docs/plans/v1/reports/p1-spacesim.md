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

## Round 2 model work (before the round-2 experiment matrix)

Status of the six items: (2), (3 as far as possible), (6) and the cost part of (5) are done; (1), (4) and the btrfs part of (5) are not.

1. **Amplification mechanism: not resolved; what the code and the data say.** The calibrated `main` still has 4.2 stored bytes per admitted byte against about 2.8 measured; `merge` stays a
   sensitivity knob (1.0 = 4.2, 0.4 = 2.8; the experiments run both). Checked: (a) the fio job (`bench/phase1/perf/fio/w4k.fio`) is 8 jobs on disjoint 256 MiB slices (`offset_increment`), the same
   `randseed` for every job, `refill_buffers=1`, `buffer_compress_percentage=50`, fio's default random map (a block is rewritten only after a whole pass over the slice): per cut this still touches about 0.9 distinct
   chunks per write, not fewer; (b) the overlay coalesces writes to a chunk within a cut, which the model does (`distinct = W (1 - e^(-n/W))`); (c) dedup hits are 0 and no chunk is zero in the status; (d) the resident
   journal window serves reads, it does not merge writes. The status data are consistent with two populations of cuts: the 1 Hz samples of `last_cut_bytes` show both 30 MiB (110 ms) and 228-374 MiB (400-530 ms)
   cuts, and `commits` rises by 8 per 5 s at 300 MB/s (190 MB of journal per commit if all were data cuts), but a cut of 30 MiB (7.2 K writes) would write 6.5 K chunks = 215 MB, which at 9 cuts a second is more than the store
   grew (0.5-0.9 GB/s). Either most small commits are collector-only (moves, naming) and carry no journal bytes, or the store writes fewer bytes per dirty chunk than the model (the cut's pack may skip a chunk whose
   dirty pieces equal the stored copy, or write partial chunks); the status alone cannot tell, and the code does not show a coalescing step that would explain a factor of 1.5. The right next measurement is one status
   trace with `stage_ns` and the commit counter at 100 ms, or a counter of store bytes per cut. Not done: modelling it.
2. **`next_delete_in` and the forecast: done.** `Store::forecast` lists `(time until a condemned segment's or punch request's grace ends, bytes)`; `Gc::next_mark_at` gives the earliest mark (the pass cadence: `min_gap` after
   the last pass); the policy sees both in `AdmissionView` (`forecast`, `next_delete_in` = the later of the two, `gap`, `store_rate`). P5 (time-to-limit pacing, horizon 40 s, budget = room + what falls due) and P6 (the
   gate's curve with an ETA floor, `r_floor = (H_proj - m) / t_next`, clamped 0.25-4 MiB/s) are `tunable` policies, applied in front of the real gate and charged `A_hat` per byte. First results (RAM 64K, 8 seeds):
   P5 at 4.2: stops 1/4, longest 1.0 s, but IOPS 13 K / 10 K (it paces for the whole run); at 2.8: 0/0, 0.3 s, 27 K / 24 K. P6 at 4.2: stops 0/0 in the first window but a longest completion of 18.5 s and a first stop in 2 of 8 seeds
   (the floor is too weak when the first graces are not yet listed); at 2.8: 0/0, 2.4 s, same as P1. Not yet in the ranking.
3. **P7: not expressible, tried an emulation, it does nothing.** The real gate grants the queue at the instant the headroom returns (inside `release`/`transfer`, `ledger.rs` `grant_waiters`), with every request past its cap
   admitted together; a simulator-side cancel and re-issue at the state change runs after those grants and so changes nothing (`p7` in `Scenario`, identical output to `main`). It needs a change in the gate's cap clock
   (`ledger.rs` `cap_at`) and a sim-only patched copy of `ledger.rs` was not made. No P7 row in the matrix.
4. **Skew workloads (zipf, hot/cold) and G6: not done.** A segment carries one live count; per-class counts, per-class death probabilities and K open segments by class need the store's segment type reworked
   (a vector of classes per segment, class-wise distinct counts per cut, segregated segment writes). It is the next structural piece of work.
5. **Punch cost and fragmentation: cost done, the rest not.** `punch_call_us` (8) and `punch_sync_ms` (19 per 8 192 punches, the middle of `punch-fs-test.md`'s 7-31 ms) are charged to the pass that lists the dead
   ranges (one call per segment with new dead bytes); `GcStats::punch_calls` counts the calls. The G7 rows in round 1 had no such cost; with it the RAM 64K G7 rows are: grace 30 s stops 2/7, longest 10.5 s (was 9.6),
   grace 5 s 2/7, 7.8 s: the cost does not move them. Extent growth and the btrfs mode (only whole-extent ranges freed) are not modelled.
6. **Pass-cadence floor and incremental condemnation: done.** The dead time of a wholly dead segment, from the model (RAM 64K, one seed): grace 30 s gives retention 1.5 + detection 14.9 + grace 30 + recheck 0.3 s;
   grace 5 s gives 1.5 + 14.9 + 5 + 8.7; grace 2 s gives 1.5 + 14.9 + 2 + 11.7. **Grace and recheck always sum to about 13-30 s, so shortening the grace only moves time into the recheck: the delete is made by the first
   pass whose mark began after the grace ended, and passes are `min_gap` = max(`busy_interval` 5 s, 3 x mark time) apart (`collector.rs` `due`, `min_gap`).** The 14.9 s of detection is the idle cadence (a pass that did no reclaim
   work waits `pass_interval` 30 s). With a 1 s gap the sum falls to 3.7 + 9.2 + 2 + 3.7 and the detection is what is left. `condemn_at_cut` (G2: a usage table updated at every cut condemns wholly dead segments at once, without a mark)
   takes detection to 0.0 s (1.9 + 0 + 2 + 4.0 s). **Is it consistent with the code's safety rules?** Only in part: condemning needs the liveness the mark computes (a segment named by a retained root or an unnamed output must not
   be flagged; dedup must stop returning it); a per-segment live count kept at the cut and the retained-set check give the same information, so flagging at the cut is safe if the flag step (the list, then the index flags) is done with the retained
   roots in hand; the delete keeps its rule (a later mark that began after the grace). In the matrix: RAM 64K at 4.2, G2 alone: stops 4/11, longest 30.5 -> 20.0 s, IOPS 24.7 K / 14.3 K -> 30.7 K / 18.3 K; at 2.8: 1/0, 12.1 -> 10.6 s.
   Combined with a short grace and gap it is what removes the 15 s detection term.

The calibration table of round 2 stands: none of these changes touches `main`'s defaults (the new switches are off), so its numbers are unchanged.

## Experiments: candidate policies at two store-amplification levels

Run by `spacesim experiments -s <cell> -n 8 --merge <m>`: 8 seeds per row, the phase of the collector's idle cadence drawn per seed, medians. The amplification knob is
`merge`, the share of a cut's distinct chunks that reach the store (writes merged before the cut): RAM 64K `merge` 1.0 gives 4.2 stored bytes per admitted byte (the
calibrated level) and 0.4 gives 2.8 (the measured amplification); the 16K and M15 cells use 1.0 and 0.66; the network-volume cell is the no-regression check. **`main` itself only
reproduces the measured stall at the 4.2 level** (at 2.8: stops 1/1, longest 12 s, first stop 29 s), so a candidate's gain at 2.8 is measured against a baseline that already stalls less.
Columns: stops in the first-fill window / steady state (first fill = overwrite start to first delete + 30 s); first stop (median seconds, seeds that stopped in the first cell); longest completion
(max over cells, median over seeds); p99.9 of the first cell; IOPS first fill / steady; burst IOPS (first 10 s); peak `used`/limit; amplification; reclaim by path (whole-dead segments / compaction sources,
GB over the run); collector busy share. P7 (cap from the last stop exit) changes the gate and has no row; P5/P6 were not run. "Grace 0 s" is a grace of zero with the pass cadence unchanged.

### Ranked recommendation of round 1 (superseded by the Round 2 ranking where they differ: that model had a 36 MiB cut rule and 3 retained FULLs)

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

## Round 2: the model fix, the t6t8 confirmations, the re-baselined matrix

### Model changes and what the code says

* **Retention is 2 FULLs after the death cut** (settled): `coord.rs:918-919` sets `chain_floor = commit_seq - (retain_last - 1)`; GC keeps the FULLs from the lowest floor any retained FULL records (`docs/reference/gc.md`
  "Roots": the tip, its predecessor and one below). A segment whose last chunk died in cut `k` is last named by FULL `k-1`, and the kept window at cut `m` is `m-2 ..= m`, so it is free at `m = k+2`: `retain_cuts = 2`
  (round 1 used 3, which the first GC note's "three FULLs" suggested; the three are the window, not the delay).
* **Cut triggers are the code's** (`coord.rs:499-533`): `cut_trigger` 1.0 (the ring is full or a write parked), `T` = 30 s, GC naming cuts once per second while moves are pending (`has_unnamed`, `MOVES_CUT_GAP`),
  and a drain-budgeted prefix; no 36 MiB rule. `merge` is 1.0 by default and stays only as a sensitivity knob; the old calibrated regime is `cut_trigger = 0.08`, kept as a **stall regime** for the matrix (below).
  `ring_park` 448 MiB is the peak of the measured ledger journal: two loaded lanes (see the confirmations) of 4 x 64 MiB files.
* **Collector order**: the mark condemns, the grace starts after every flag is set (`shift_condemned`, `condemn_ms` per segment), and the bytes of the deletes are released when the pass's index work ends, not at the mark
  (`collector.rs:1399-1420, :1492, :1558`). The pass cadence floor is `min_gap = max(busy_interval, 3 x mark)` after the previous pass ends (`collector.rs:925-934, :965-976`).
* **P1'**: the floor is in journal bytes (`max(curve / A, r_min)`), `A_hat` has a +-15 % deterministic wobble. The gate's own delay cap (10 s from arrival) applies.
* **Statistics**: 32 seeds (24 for M15, 16 for 16K, 8 for the network volume), a separate random stream for the timing jitter, a fixed first-fill window (the first 90 s) for every row, and `longest` as median / p90 of the
  per-run longest completion.

### Confirmations from the t6t8 data (origin/p1/m16, `writes-ram`)

* **Loaded lanes**: `freed_files_by_lane` is `[0, 0, n, m]` in all six RAM cells: two lanes carry the load, so the fill ratio is at most 0.5 and the fill trigger never fires.
* **Commits per second** from the 1 Hz status samples: 0.2-1.2 per second in `slowdown` (median about 0.5), 0-0.4 in `stop`, 0.4-1.8 in `open`: in the band the FULLs come about every 2 s, not every 100 ms.
* **Last-cut bytes** (samples): 1 to 450 MiB in the same cell (29, 33, 228, 1, 20, 350, 30, 117 MiB in rep 1), i.e. small naming/GC cuts and large ring cuts side by side, as the note said.
* **The first scheduled GC pass came before the stop** (rep 1, `lunixd.1.log`): throttle on at 955.3 s, pass 1 (`how=step`, 134 ms) at 961.2 s condemned **779 segments (12.5 GB)**, the stop at 961.8 s, an urgent pass at 961.8 s
  condemned 97 more, the deleting pass at 994.3 s (33 s after the condemn: the 30 s grace plus about 3 s) took 3.1 s wall (delete phase 1 993 ms for 1 144 units) and freed 12 237 MB; throttle off at 996.5 s. So the stall is one grace,
  and the whole-dead pool was 12.5 GB at the stop (round 1's "about 1 GB condemned" was `condemned_pending` after the first deletes).
* **Index work per object on tmpfs**: condemn 12 ms for 885 units (0.014 ms), delete 1.7 ms per unit, mark 80-98 ms for 15 units. The gcrate figures (5.5 / 13 ms) are disk figures; the defaults of `condemn_ms` / `delete_ms` are those
  and over-price the RAM cell (set `--set condemn_ms=0.015 --set delete_ms=1.7` for it).

### New calibration (`main`, faithful triggers, 32 seeds)

| cell | measured | simulated | verdict |
|---|---|---|---|
| RAM 64K store bytes per admitted byte, pre-band | about 2.8 | 2.4 (2.3-2.4) | 15 % low, no knob (was 4.2 with the crutch) |
| first stop | about 20-27 s (t6t8); 12-15 s (brief) | 25 s (24-27), 32 of 32 | match (t6t8) |
| stops per cell (first-fill / steady) | 1-7 | 1 / 2 | low |
| longest completion, median / p90 over runs | 18-33 s | 35.1 / 68.3 s | stall present, bimodal and heavy-tailed |
| sustained IOPS first-fill / steady | 24-25 K | 35.8 K / 28.9 K | +45 % / +17 % |
| reclaim at first band entry | 0 | 3 (0-8) MB/s | match |
| RAM 16K longest | 5.8 s, 43.6 K IOPS | 42 s (2 of 16 stop), 35 K | **does not reproduce** |
| network volume 64K | 0 stops, 3.3 K IOPS | 0 stops, 2.0 K (0.8 K steady), longest 7.6 s | gate match; IOPS 1.7x low |
| M15 8 GiB | stops 0-2, 14-38 s | stops 1 / 1, 36.8 s | match |

The stall regime (`cut_trigger` 0.08, the round-1 calibration) gives RAM 64K: first stop 13 s, stops 4 / 5, longest 31.0 / 35.9 s, IOPS 20.7 K / 15.2 K, A 4.2; RAM 16K longest 13.0 s. The two regimes bracket the measurement
(64K: faithful reproduces the first stop, A and the stall length but runs too fast; stall regime reproduces the stop count and IOPS but not A), and **a candidate counts as a winner only if it wins in both**.

### Results (full tables below; `longest` is median / p90 s)

RAM 64K, faithful / stall regime:

| candidate | faithful: stops, longest, IOPS | stall regime: stops, longest, IOPS |
|---|---|---|
| base (main) | 1/2, 35.1 / 68.3, 35.8 K / 28.9 K | 4/5, 31.0 / 35.9, 20.7 K / 15.2 K |
| P1 (pace in store bytes, journal-unit floor) | 0/0, 4.3 / 50.9 | 2/0, 19.5 / 23.6 |
| P5 (time-to-limit) | 0/0, 4.6 / 10.4, 31 K | 1/4, 1.0 / 5.2, **12.2 K** |
| P6 (ETA floor) | 0/0, 3.5 / 9.3 | 1/0, 19.3 / 27.1 |
| C6 gap 1 s only | 1/4, 13.8 / 68.3 | 4/5, 31.0 / 32.9 |
| G2/C5 condemn at each cut | 2/4, 8.3 / 11.4, 40 K | 6/10, 19.7 / 28.8, 25 K |
| C5 + gap 1 s | 3/4, 6.9 / 9.6, 45 K | 7/12, 22.0 / 29.8 |
| P1 + C5 + gap 1 s | 0/0, 1.0 / 4.8, 41 K | 1/0, 5.4 / 12.4, 25 K |
| C5 + gap 1 s + grace 2 s (needs reader fix) | 0/0, 0.7 / 0.7, 67 K | 3/4, 29.1 / 29.2, 43 K |
| P4 grace 0 s + gap 1 s | 0/0, 0.7 / 6.6, 67 K | 6/7, 6.7 / 8.8, 46 K |
| G7 punch, grace 30 s | 2/3, 11.2 / 40.9 | 3/7, 15.1 / 30.0 |
| G9 grace from death | 2/1, 5.4 / 12.1 | 6/11, 19.5 / 30.9 |
| P0a / P0c | 13.8 / 31.6 and 12.7 / 68.3 | 31.0 / 35.6 and 31.1 / 32.5 |
| G1+G3+G4, G5, P2 | no gain (P2 1 GiB is worse: 64.7 s) | no gain |

Across M15 (cap-bound: the dead time must shrink), RAM 16K and the network volume: only the grace-shortening rows move the M15 cell (P4 5 s + gap: 12.3 / 17.0 s; C5 + gap + grace 2 s: 9.0 / 0.8 s; combo E 10.1 / 0.6 s), P1 does not (35.4 s),
P1 + C5 + gap does little (29.6 / 29.0 s). On RAM 16K C5 + gap 1 s gives 1.0 / 1.0 s in both regimes and P1 + C5 + gap 1 s 0.9 / 0.8 s. The network volume is unchanged by every row (no regression).

### Ranked recommendation (confidence in brackets; low where the two regimes disagree)

1. **P1' + condemn-on-cut + a 1 s pass gap under pressure** (no reader-quiescence dependency, no FREEZE): the only row that wins in both regimes on both RAM cells (64K: longest 1.0 / 5.4 s against 35 / 31 s, stops 0-1; 16K: 0.9 / 0.8 s).
   It does nothing for a cap-bound volume (M15). Confidence: medium. Parts: P1' is the gate-only piece (largest single effect on 64K, none on M15), condemn-on-cut and the gap are collector scheduling changes (the safety argument is in the round-2 item 6 text above).
2. **Add a short grace (2-5 s) behind reader quiescence or a revalidating read**: the only lever on a cap-bound volume (M15 longest 36.8 s -> 9.0 s with C5 + gap + grace 2 s, sustained IOPS 17 K -> 49 K) and on the rate law `G / D`.
   Confidence: medium on the effect, and the correctness work is the dependency. In the stall regime at RAM 64K it is not enough alone (29 s): it needs the gap and condemn-on-cut, and still P1' for the first fill.
3. **P5 (time-to-limit pacing)**: removes the stall (1.0 / 5.2 s) at the price of halving the IOPS in the stall regime (12 K against 20 K); needs tuning (a bounded envelope) before it is a candidate. Confidence: low.
4. **G7 and G9**: behind the above: G7 at the 30 s grace only reaches 11-15 s at RAM 64K and its grace-5 s rows do not hold in both regimes; G9 5.4 / 19.5 s. Not recommended before the model has skew and the real punch cost.
5. **No gain, do not implement:** P0a/b/c (hygiene only), P2, G5, G1/G3/G4 alone (G1 lowers the collector's share from 18 % to 8 %), `gate-gc`, `leadzone`.

Caveats: the faithful baseline does not reproduce RAM 16K (42 s against 5.8 s measured), runs 45 % too fast at 64K and under-counts stops, so every ranking is conditional on the two regimes agreeing; skew workloads, G6 and the realistic G7 rows of the
note (C8, C9), 16 GiB working sets, the P7 row (C4: not expressible, see round-2 item 3) and the revalidating-read variant (C7, priced here only as a short grace) are **not done**; P5' with the whole-pipeline envelope (C3) is the original P5 here.


#### RAM 64K, faithful triggers

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 2 | 25 (32/32) | 35.1 / 68.3 | 0.66 | 35.8K / 28.9K | 64.7K | 1.011 | 2.4 | 30.7 / 16.8 | 18 % |
| gate-gc | 1 / 2 | 25 (32/32) | 37.7 / 68.3 | 0.66 | 36.7K / 28.6K | 64.7K | 1.012 | 2.4 | 32.2 / 17.9 | 17 % |
| leadzone | 1 / 2 | 25 (32/32) | 38.8 / 68.3 | 0.66 | 35.8K / 29.3K | 64.7K | 1.011 | 2.4 | 31.3 / 14.1 | 18 % |
| P0a bound compressed | 2 / 3 | 35 (29/32) | 13.8 / 31.6 | 0.98 | 38.0K / 29.2K | 64.7K | 1.016 | 2.4 | 32.8 / 22.3 | 25 % |
| P0b true-up early | 1 / 2 | 25 (32/32) | 17.2 / 68.4 | 0.66 | 35.8K / 29.9K | 64.7K | 1.012 | 2.4 | 32.6 / 17.5 | 19 % |
| P0c limit anchored on disk | 1 / 3 | 25 (32/32) | 12.7 / 68.3 | 0.66 | 35.5K / 30.0K | 64.7K | 1.012 | 2.4 | 32.2 / 18.6 | 21 % |
| P1 pace in store bytes | 0 / 0 | 40 (1/32) | 4.3 / 50.9 | 0.78 | 36.1K / 29.9K | 64.7K | 1.008 | 2.4 | 30.1 / 23.0 | 25 % |
| P2 growth pass 1 GiB | 1 / 1 | 25 (32/32) | 64.7 / 71.5 | 0.65 | 20.8K / 27.2K | 65.1K | 1.010 | 2.4 | 26.5 / 10.4 | 15 % |
| P2 growth pass 0.25 GiB | 1 / 1 | 25 (32/32) | 64.7 / 71.5 | 0.65 | 20.8K / 27.2K | 65.1K | 1.010 | 2.4 | 26.5 / 10.4 | 15 % |
| G9 grace from death | 2 / 1 | 25 (32/32) | 5.4 / 12.1 | 0.66 | 43.0K / 40.6K | 64.7K | 1.012 | 2.4 | 45.7 / 21.0 | 20 % |
| P3 grace 15 s | 1 / 1 | 25 (32/32) | 12.2 / 43.5 | 0.66 | 41.1K / 40.9K | 64.7K | 1.012 | 2.4 | 48.1 / 17.7 | 28 % |
| P3 grace 10 s | 1 / 1 | 25 (32/32) | 12.7 / 15.4 | 0.66 | 48.2K / 42.8K | 64.7K | 1.010 | 2.4 | 52.7 / 17.0 | 29 % |
| P3 grace 5 s | 1 / 0 | 25 (24/32) | 11.5 / 14.8 | 0.65 | 53.7K / 49.2K | 64.7K | 1.009 | 2.4 | 61.4 / 18.6 | 33 % |
| P3 grace 2 s | 1 / 0 | 25 (24/32) | 12.4 / 62.7 | 0.65 | 53.7K / 48.5K | 64.7K | 1.009 | 2.4 | 60.5 / 18.6 | 33 % |
| P3 grace 0 s | 1 / 0 | 25 (24/32) | 12.5 / 68.3 | 0.65 | 53.7K / 47.4K | 64.7K | 1.009 | 2.4 | 60.3 / 18.3 | 33 % |
| P4 grace 5 s + gap 1 s | 1 / 0 | 25 (17/32) | 3.1 / 6.8 | 0.65 | 63.3K / 65.1K | 64.7K | 1.008 | 2.4 | 70.0 / 32.5 | 46 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | 25 (11/32) | 0.7 / 6.6 | 0.65 | 66.5K / 66.7K | 64.7K | 0.985 | 2.4 | 75.6 / 31.5 | 46 % |
| G5 threshold from headroom | 1 / 2 | 25 (32/32) | 35.1 / 68.3 | 0.66 | 35.6K / 28.9K | 64.7K | 1.011 | 2.4 | 32.0 / 16.7 | 19 % |
| G1 batched index work | 1 / 3 | 25 (30/32) | 13.2 / 63.3 | 0.66 | 37.4K / 33.9K | 64.7K | 1.014 | 2.4 | 31.0 / 25.8 | 8 % |
| G3 incremental mark | 1 / 2 | 25 (32/32) | 36.9 / 66.9 | 0.66 | 35.8K / 28.9K | 64.7K | 1.011 | 2.4 | 32.4 / 17.5 | 17 % |
| G4 two collector threads | 1 / 2 | 25 (32/32) | 38.8 / 62.6 | 0.66 | 35.4K / 26.6K | 64.7K | 1.011 | 2.4 | 30.6 / 15.8 | 30 % |
| G1+G3+G4 | 1 / 4 | 25 (30/32) | 15.5 / 62.4 | 0.66 | 37.8K / 36.6K | 64.7K | 1.014 | 2.4 | 30.5 / 31.0 | 9 % |
| G7 punch, grace 30 s | 2 / 3 | 25 (32/32) | 11.2 / 40.9 | 0.66 | 39.0K / 36.1K | 64.7K | 1.012 | 2.4 | 58.2 / 0.0 | 19 % |
| G7 punch, grace 5 s | 2 / 3 | 25 (30/32) | 1.2 / 42.0 | 0.65 | 61.4K / 57.8K | 64.7K | 1.012 | 2.4 | 73.4 / 0.0 | 36 % |
| P5 time-to-limit pacing | 0 / 0 | 52 (3/32) | 4.6 / 10.4 | 0.65 | 31.3K / 32.6K | 48.4K | 1.007 | 2.5 | 23.1 / 28.9 | 26 % |
| P6 ETA floor | 0 / 0 | 40 (1/32) | 3.5 / 9.3 | 0.77 | 36.0K / 29.8K | 64.7K | 1.008 | 2.4 | 30.3 / 24.2 | 24 % |
| G2 condemn at each cut | 2 / 4 | 25 (32/32) | 8.3 / 11.4 | 0.66 | 40.3K / 38.3K | 64.7K | 1.014 | 2.4 | 41.6 / 23.3 | 15 % |
| P1' + P6 (ETA floor) | 0 / 0 | 40 (1/32) | 3.5 / 9.3 | 0.77 | 36.0K / 29.8K | 64.7K | 1.008 | 2.4 | 30.3 / 24.2 | 24 % |
| C6 gap 1 s only | 1 / 4 | 25 (29/32) | 13.8 / 68.3 | 0.65 | 39.0K / 36.3K | 64.7K | 1.014 | 2.4 | 28.3 / 36.0 | 31 % |
| C5 condemn at cut + gap 1 s | 3 / 4 | 25 (32/32) | 6.9 / 9.6 | 0.66 | 44.7K / 39.5K | 64.7K | 1.015 | 2.4 | 21.6 / 56.8 | 29 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 0 / 0 | none | 0.7 / 0.7 | 0.65 | 66.8K / 66.9K | 64.7K | 0.933 | 2.4 | 102.8 / 7.7 | 47 % |
| P1' + C5 + gap 1 s | 0 / 0 | 39 (2/32) | 1.0 / 4.8 | 0.65 | 41.2K / 35.0K | 64.7K | 1.012 | 2.4 | 21.6 / 54.6 | 34 % |
| combo A: P2 + grace 5 s + gap 1 s | 2 / 0 | 25 (29/32) | 1.4 / 3.5 | 0.65 | 59.3K / 64.6K | 65.1K | 1.011 | 2.4 | 69.5 / 32.0 | 51 % |
| combo B: A + P0a | 1 / 0 | 31 (23/32) | 3.4 / 7.2 | 0.67 | 59.1K / 63.6K | 65.1K | 1.013 | 2.4 | 70.4 / 30.1 | 52 % |
| combo C: P2 + P1 | 0 / 0 | 41 (3/32) | 1.0 / 42.8 | 0.86 | 35.9K / 28.8K | 65.1K | 1.008 | 2.4 | 32.1 / 18.7 | 29 % |
| combo D: P2 + G9 + P0a | 1 / 0 | 31 (26/32) | 1.2 / 5.9 | 1.01 | 44.1K / 42.3K | 65.1K | 1.008 | 2.4 | 42.2 / 26.1 | 35 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | 52 (1/32) | 0.7 / 0.7 | 0.65 | 66.5K / 67.0K | 64.9K | 0.967 | 2.4 | 81.8 / 0.0 | 43 % |

#### RAM 64K, stall regime (cut_trigger 0.08)

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 4 / 5 | 13 (32/32) | 31.0 / 35.9 | 1.02 | 20.7K / 15.2K | 94.3K | 1.031 | 4.1 | 38.5 / 16.3 | 13 % |
| gate-gc | 4 / 4 | 13 (30/32) | 31.1 / 36.0 | 1.02 | 20.9K / 16.4K | 94.3K | 1.023 | 4.1 | 37.5 / 18.1 | 15 % |
| leadzone | 4 / 5 | 13 (32/32) | 31.0 / 35.9 | 1.02 | 20.7K / 15.2K | 94.3K | 1.029 | 4.1 | 38.4 / 16.2 | 13 % |
| P0a bound compressed | 2 / 4 | 19 (32/32) | 31.0 / 35.6 | 1.02 | 21.2K / 16.4K | 94.0K | 1.029 | 4.1 | 41.3 / 19.4 | 15 % |
| P0b true-up early | 4 / 5 | 14 (31/32) | 31.1 / 35.9 | 1.02 | 20.0K / 14.5K | 94.1K | 1.031 | 4.1 | 38.3 / 17.1 | 14 % |
| P0c limit anchored on disk | 4 / 4 | 17 (32/32) | 31.1 / 32.5 | 1.02 | 20.3K / 14.4K | 89.4K | 1.043 | 4.2 | 34.3 / 20.9 | 15 % |
| P1 pace in store bytes | 2 / 0 | 30 (25/32) | 19.5 / 23.6 | 1.02 | 19.8K / 13.3K | 87.9K | 1.013 | 4.2 | 24.3 / 32.7 | 24 % |
| P2 growth pass 1 GiB | 4 / 5 | 14 (32/32) | 31.2 / 35.9 | 1.02 | 21.0K / 15.7K | 93.8K | 1.035 | 4.1 | 39.8 / 18.4 | 14 % |
| P2 growth pass 0.25 GiB | 4 / 5 | 15 (32/32) | 31.2 / 35.5 | 9.32 | 20.6K / 13.3K | 92.8K | 1.026 | 4.0 | 35.6 / 19.0 | 13 % |
| G9 grace from death | 6 / 11 | 13 (32/32) | 19.5 / 30.9 | 5.96 | 25.8K / 18.1K | 94.3K | 1.043 | 4.1 | 48.0 / 24.1 | 17 % |
| P3 grace 15 s | 5 / 4 | 13 (32/32) | 30.7 / 33.1 | 1.02 | 22.2K / 18.6K | 94.3K | 1.028 | 4.1 | 45.0 / 18.5 | 15 % |
| P3 grace 10 s | 5 / 6 | 13 (32/32) | 17.9 / 33.2 | 1.02 | 28.8K / 23.4K | 94.3K | 1.021 | 4.1 | 57.3 / 21.1 | 21 % |
| P3 grace 5 s | 4 / 4 | 13 (32/32) | 31.5 / 33.2 | 1.02 | 28.2K / 17.8K | 94.3K | 1.020 | 4.1 | 51.9 / 17.2 | 15 % |
| P3 grace 2 s | 4 / 4 | 13 (32/32) | 31.4 / 33.1 | 1.02 | 27.9K / 16.8K | 94.3K | 1.023 | 4.1 | 49.6 / 16.1 | 13 % |
| P3 grace 0 s | 4 / 4 | 13 (32/32) | 31.4 / 33.1 | 1.02 | 27.9K / 16.8K | 94.3K | 1.023 | 4.1 | 49.6 / 16.1 | 13 % |
| P4 grace 5 s + gap 1 s | 5 / 8 | 13 (32/32) | 31.0 / 33.0 | 1.02 | 28.4K / 33.7K | 94.3K | 1.048 | 4.1 | 70.4 / 30.2 | 26 % |
| P4 grace 0 s + gap 1 s | 6 / 7 | 13 (32/32) | 6.7 / 8.8 | 1.02 | 46.2K / 40.0K | 94.3K | 1.034 | 4.1 | 99.3 / 31.5 | 35 % |
| G5 threshold from headroom | 4 / 4 | 13 (32/32) | 31.3 / 35.8 | 1.02 | 20.6K / 13.4K | 94.3K | 1.017 | 4.1 | 34.6 / 17.6 | 12 % |
| G1 batched index work | 4 / 7 | 13 (32/32) | 29.8 / 35.7 | 1.02 | 23.2K / 19.5K | 94.2K | 1.017 | 4.1 | 36.6 / 35.4 | 6 % |
| G3 incremental mark | 4 / 5 | 13 (32/32) | 30.7 / 35.1 | 1.02 | 19.5K / 15.4K | 94.3K | 1.022 | 4.1 | 36.4 / 20.2 | 13 % |
| G4 two collector threads | 4 / 5 | 13 (32/32) | 31.3 / 35.8 | 1.02 | 19.9K / 15.5K | 94.3K | 1.017 | 4.1 | 34.6 / 22.2 | 35 % |
| G1+G3+G4 | 4 / 7 | 13 (32/32) | 29.0 / 35.0 | 1.02 | 22.8K / 18.4K | 94.2K | 1.017 | 4.1 | 30.8 / 40.6 | 10 % |
| G7 punch, grace 30 s | 3 / 7 | 13 (32/32) | 15.1 / 30.0 | 1.02 | 21.9K / 18.6K | 94.2K | 1.036 | 4.2 | 58.8 / 0.0 | 16 % |
| G7 punch, grace 5 s | 3 / 4 | 13 (32/32) | 30.6 / 33.0 | 1.02 | 30.7K / 20.2K | 94.3K | 1.047 | 4.2 | 66.0 / 0.0 | 14 % |
| P5 time-to-limit pacing | 1 / 4 | 53 (22/32) | 1.0 / 5.2 | 0.78 | 12.2K / 10.6K | 25.4K | 1.011 | 6.5 | 2.0 / 52.4 | 31 % |
| P6 ETA floor | 1 / 0 | 33 (22/32) | 19.3 / 27.1 | 3.69 | 20.1K / 13.2K | 87.9K | 1.010 | 4.2 | 25.0 / 33.5 | 25 % |
| G2 condemn at each cut | 6 / 10 | 13 (32/32) | 19.7 / 28.8 | 1.06 | 25.4K / 17.2K | 94.2K | 1.021 | 4.1 | 50.7 / 22.6 | 12 % |
| P1' + P6 (ETA floor) | 1 / 0 | 33 (22/32) | 19.3 / 27.1 | 3.69 | 20.1K / 13.2K | 87.9K | 1.010 | 4.2 | 25.0 / 33.5 | 25 % |
| C6 gap 1 s only | 4 / 5 | 13 (32/32) | 31.0 / 32.9 | 1.02 | 20.1K / 15.8K | 94.3K | 1.025 | 4.1 | 36.1 / 22.4 | 13 % |
| C5 condemn at cut + gap 1 s | 7 / 12 | 13 (32/32) | 22.0 / 29.8 | 4.84 | 25.3K / 17.7K | 94.2K | 1.038 | 4.1 | 41.3 / 38.8 | 17 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 3 / 4 | 22 (29/32) | 29.1 / 29.2 | 0.43 | 43.0K / 41.6K | 101.2K | 1.032 | 4.1 | 154.0 / 3.0 | 24 % |
| P1' + C5 + gap 1 s | 1 / 0 | 29 (19/32) | 5.4 / 12.4 | 1.02 | 24.8K / 15.3K | 87.9K | 1.008 | 4.2 | 38.9 / 42.8 | 28 % |
| combo A: P2 + grace 5 s + gap 1 s | 6 / 8 | 13 (32/32) | 12.8 / 31.7 | 2.70 | 40.4K / 34.1K | 94.6K | 1.052 | 4.1 | 72.2 / 39.3 | 30 % |
| combo B: A + P0a | 5 / 7 | 17 (32/32) | 6.3 / 31.4 | 2.58 | 40.7K / 35.1K | 94.4K | 1.042 | 4.1 | 71.9 / 43.4 | 35 % |
| combo C: P2 + P1 | 2 / 0 | 30 (29/32) | 19.9 / 23.2 | 1.02 | 20.4K / 13.7K | 89.1K | 1.014 | 4.1 | 24.9 / 35.4 | 24 % |
| combo D: P2 + G9 + P0a | 3 / 8 | 17 (32/32) | 20.8 / 31.3 | 1.01 | 25.1K / 18.6K | 93.7K | 1.035 | 4.1 | 39.1 / 29.3 | 19 % |
| combo E: G7 + P2 + grace 5 s | 5 / 8 | 22 (32/32) | 5.0 / 6.0 | 0.49 | 51.7K / 42.9K | 95.4K | 1.077 | 4.1 | 116.0 / 0.0 | 35 % |

#### RAM 16K, faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 2 | 48 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.7K | 83.6K | 1.010 | 1.4 | 9.8 / 32.2 | 33 % |
| gate-gc | 0 / 1 | 50 (3/16) | 42.3 / 42.6 | 0.40 | 34.9K / 45.2K | 83.6K | 1.009 | 1.4 | 8.3 / 33.1 | 29 % |
| leadzone | 0 / 2 | 48 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.4K | 83.6K | 1.010 | 1.4 | 10.2 / 32.4 | 33 % |
| P0a bound compressed | 1 / 0 | 55 (2/16) | 31.0 / 31.0 | 0.67 | 35.1K / 46.5K | 83.6K | 1.017 | 1.4 | 11.1 / 30.0 | 35 % |
| P0b true-up early | 0 / 2 | 47 (3/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.8K | 83.6K | 1.014 | 1.4 | 10.4 / 32.3 | 35 % |
| P0c limit anchored on disk | 0 / 1 | 47 (4/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.2K | 83.6K | 1.011 | 1.4 | 8.2 / 33.0 | 32 % |
| P1 pace in store bytes | 0 / 0 | none | 37.3 / 42.6 | 0.40 | 34.9K / 43.4K | 83.6K | 0.999 | 1.4 | 9.6 / 31.6 | 31 % |
| P2 growth pass 1 GiB | 0 / 2 | 47 (7/16) | 5.8 / 22.3 | 0.44 | 54.7K / 40.5K | 83.6K | 1.014 | 1.4 | 14.7 / 32.5 | 41 % |
| P2 growth pass 0.25 GiB | 0 / 1 | 47 (6/16) | 1.0 / 22.3 | 0.44 | 54.7K / 39.8K | 83.7K | 1.015 | 1.4 | 15.5 / 32.3 | 41 % |
| G9 grace from death | 0 / 0 | 48 (2/16) | 1.0 / 16.7 | 0.40 | 55.1K / 49.0K | 83.6K | 1.003 | 1.4 | 11.0 / 39.1 | 35 % |
| P3 grace 15 s | 0 / 0 | 48 (1/16) | 42.3 / 42.7 | 0.40 | 34.9K / 51.5K | 83.6K | 0.998 | 1.4 | 11.9 / 30.8 | 39 % |
| P3 grace 10 s | 0 / 1 | 48 (1/16) | 15.5 / 15.9 | 0.40 | 53.6K / 50.1K | 83.6K | 1.011 | 1.4 | 15.9 / 28.2 | 43 % |
| P3 grace 5 s | 0 / 0 | none | 15.5 / 15.9 | 0.40 | 53.6K / 67.2K | 83.6K | 0.991 | 1.4 | 15.7 / 38.6 | 43 % |
| P3 grace 2 s | 0 / 0 | none | 15.5 / 15.9 | 0.40 | 53.6K / 67.2K | 83.6K | 0.991 | 1.4 | 15.7 / 38.6 | 43 % |
| P3 grace 0 s | 0 / 0 | none | 15.5 / 15.9 | 0.40 | 53.6K / 67.2K | 83.6K | 0.991 | 1.4 | 15.7 / 38.6 | 43 % |
| P4 grace 5 s + gap 1 s | 0 / 1 | 43 (1/16) | 7.6 / 9.5 | 0.40 | 53.5K / 64.9K | 83.6K | 1.001 | 1.4 | 17.1 / 40.9 | 51 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 7.6 / 9.5 | 0.40 | 54.6K / 76.6K | 83.6K | 0.988 | 1.4 | 15.7 / 42.5 | 52 % |
| G5 threshold from headroom | 0 / 2 | 48 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.7K | 83.6K | 1.012 | 1.4 | 10.6 / 30.5 | 33 % |
| G1 batched index work | 0 / 2 | 43 (4/16) | 33.0 / 33.4 | 0.40 | 41.7K / 43.7K | 83.6K | 1.011 | 1.4 | 9.4 / 32.4 | 29 % |
| G3 incremental mark | 0 / 3 | 50 (3/16) | 39.3 / 39.4 | 0.40 | 35.5K / 45.6K | 83.6K | 1.011 | 1.4 | 8.2 / 34.1 | 29 % |
| G4 two collector threads | 0 / 2 | 50 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 44.2K | 83.6K | 1.012 | 1.4 | 10.8 / 31.4 | 54 % |
| G1+G3+G4 | 0 / 1 | 45 (3/16) | 61.0 / 61.4 | 0.40 | 34.9K / 42.2K | 83.6K | 1.006 | 1.4 | 7.8 / 31.9 | 38 % |
| G7 punch, grace 30 s | 0 / 0 | none | 1.5 / 13.0 | 0.40 | 59.7K / 60.3K | 83.6K | 0.988 | 1.4 | 41.7 / 0.0 | 27 % |
| G7 punch, grace 5 s | 0 / 0 | none | 0.4 / 0.5 | 0.40 | 75.1K / 79.7K | 83.6K | 0.986 | 1.4 | 33.4 / 0.0 | 34 % |
| P5 time-to-limit pacing | 2 / 0 | none | 13.1 / 13.8 | 0.41 | 33.4K / 41.6K | 76.6K | 1.011 | 1.4 | 6.2 / 32.0 | 40 % |
| P6 ETA floor | 0 / 0 | none | 42.3 / 43.0 | 0.40 | 34.9K / 43.5K | 83.6K | 0.998 | 1.4 | 9.8 / 29.6 | 33 % |
| G2 condemn at each cut | 1 / 0 | 45 (10/16) | 0.9 / 2.4 | 0.84 | 60.5K / 46.7K | 83.6K | 1.004 | 1.4 | 16.8 / 37.5 | 35 % |
| P1' + P6 (ETA floor) | 0 / 0 | none | 42.3 / 43.0 | 0.40 | 34.9K / 43.5K | 83.6K | 0.998 | 1.4 | 9.8 / 29.6 | 33 % |
| C6 gap 1 s only | 0 / 3 | 47 (3/16) | 42.3 / 42.6 | 0.40 | 34.9K / 49.6K | 83.6K | 1.008 | 1.4 | 8.8 / 34.7 | 33 % |
| C5 condemn at cut + gap 1 s | 1 / 0 | 46 (8/16) | 1.0 / 2.4 | 0.46 | 60.8K / 56.6K | 83.6K | 1.007 | 1.4 | 11.3 / 48.0 | 41 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 0 / 0 | none | 0.4 / 0.4 | 0.40 | 80.2K / 79.5K | 83.6K | 0.965 | 1.4 | 67.2 / 8.1 | 61 % |
| P1' + C5 + gap 1 s | 0 / 0 | none | 0.9 / 1.0 | 0.55 | 59.8K / 54.0K | 83.6K | 0.994 | 1.4 | 11.5 / 47.2 | 40 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 1 | 42 (4/16) | 0.7 / 1.0 | 0.41 | 66.7K / 63.2K | 83.6K | 1.004 | 1.4 | 19.0 / 46.0 | 60 % |
| combo B: A + P0a | 0 / 0 | 43 (1/16) | 0.9 / 1.0 | 0.44 | 66.0K / 59.4K | 83.6K | 0.993 | 1.4 | 20.3 / 44.1 | 61 % |
| combo C: P2 + P1 | 2 / 0 | 54 (9/16) | 14.8 / 15.0 | 1.02 | 48.6K / 44.6K | 83.6K | 1.010 | 1.4 | 15.1 / 31.1 | 42 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 0.4 / 0.5 | 0.40 | 62.5K / 49.0K | 83.6K | 0.967 | 1.4 | 10.6 / 42.8 | 44 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.4 / 0.4 | 0.40 | 81.0K / 81.6K | 83.6K | 0.952 | 1.4 | 39.8 / 0.0 | 38 % |

#### RAM 16K, stall regime

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 1 | 39 (16/16) | 13.0 / 15.1 | 1.02 | 46.1K / 25.8K | 110.0K | 1.007 | 2.0 | 4.4 / 47.3 | 26 % |
| gate-gc | 2 / 2 | 39 (13/16) | 12.7 / 15.0 | 1.02 | 46.6K / 26.2K | 110.0K | 1.005 | 2.0 | 3.8 / 46.1 | 28 % |
| leadzone | 1 / 2 | 39 (16/16) | 13.1 / 15.1 | 1.02 | 46.2K / 26.4K | 110.0K | 1.009 | 2.0 | 4.5 / 46.4 | 26 % |
| P0a bound compressed | 1 / 2 | 40 (16/16) | 13.8 / 15.2 | 1.02 | 46.1K / 26.5K | 110.0K | 1.007 | 2.0 | 4.4 / 47.5 | 26 % |
| P0b true-up early | 2 / 2 | 39 (16/16) | 13.7 / 15.0 | 1.02 | 46.1K / 25.3K | 110.0K | 1.007 | 2.0 | 4.7 / 46.8 | 26 % |
| P0c limit anchored on disk | 1 / 1 | 40 (16/16) | 14.5 / 15.0 | 1.02 | 46.0K / 25.6K | 110.0K | 1.007 | 2.0 | 4.5 / 46.7 | 26 % |
| P1 pace in store bytes | 0 / 0 | 57 (1/16) | 1.0 / 1.0 | 1.02 | 44.1K / 24.0K | 110.0K | 0.991 | 2.0 | 4.2 / 44.7 | 28 % |
| P2 growth pass 1 GiB | 1 / 0 | 41 (16/16) | 6.1 / 6.2 | 1.02 | 46.6K / 25.8K | 110.0K | 1.009 | 2.0 | 2.4 / 50.0 | 28 % |
| P2 growth pass 0.25 GiB | 1 / 0 | 41 (16/16) | 6.1 / 6.2 | 1.02 | 46.6K / 25.9K | 110.0K | 1.008 | 2.0 | 2.8 / 48.9 | 28 % |
| G9 grace from death | 1 / 0 | 39 (10/16) | 1.0 / 1.0 | 1.01 | 46.8K / 39.4K | 110.0K | 1.001 | 2.0 | 3.2 / 52.4 | 29 % |
| P3 grace 15 s | 0 / 0 | 44 (1/16) | 0.4 / 0.7 | 0.36 | 52.2K / 44.5K | 110.0K | 0.983 | 2.0 | 2.4 / 61.4 | 29 % |
| P3 grace 10 s | 0 / 0 | 44 (1/16) | 0.7 / 0.8 | 0.36 | 51.7K / 45.1K | 110.0K | 0.994 | 2.0 | 5.7 / 60.7 | 29 % |
| P3 grace 5 s | 0 / 0 | none | 0.2 / 0.3 | 0.17 | 62.6K / 50.4K | 110.0K | 0.968 | 2.0 | 3.6 / 67.6 | 30 % |
| P3 grace 2 s | 0 / 0 | none | 0.2 / 0.3 | 0.17 | 62.6K / 50.4K | 110.0K | 0.968 | 2.0 | 3.6 / 67.6 | 30 % |
| P3 grace 0 s | 0 / 0 | none | 0.2 / 0.3 | 0.17 | 62.6K / 50.4K | 110.0K | 0.968 | 2.0 | 3.6 / 67.6 | 30 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | 41 (1/16) | 0.4 / 0.7 | 0.31 | 68.7K / 54.1K | 110.0K | 0.983 | 2.0 | 9.0 / 72.5 | 35 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 0.1 / 0.2 | 0.12 | 69.2K / 61.0K | 110.0K | 0.958 | 2.0 | 2.9 / 87.6 | 36 % |
| G5 threshold from headroom | 1 / 1 | 39 (16/16) | 6.0 / 16.9 | 1.02 | 46.1K / 24.6K | 110.0K | 1.014 | 2.0 | 3.6 / 48.3 | 26 % |
| G1 batched index work | 2 / 0 | 39 (16/16) | 5.8 / 6.1 | 1.02 | 45.2K / 37.6K | 110.0K | 1.005 | 2.0 | 1.8 / 59.2 | 16 % |
| G3 incremental mark | 1 / 2 | 39 (16/16) | 11.4 / 18.9 | 1.02 | 46.4K / 30.2K | 110.0K | 1.006 | 2.0 | 2.8 / 49.0 | 21 % |
| G4 two collector threads | 1 / 1 | 39 (16/16) | 13.0 / 15.2 | 1.02 | 46.0K / 26.0K | 110.0K | 1.009 | 2.0 | 4.4 / 47.2 | 57 % |
| G1+G3+G4 | 2 / 0 | 38 (15/16) | 5.1 / 17.0 | 1.02 | 44.9K / 40.2K | 110.0K | 1.003 | 2.0 | 1.3 / 62.5 | 22 % |
| G7 punch, grace 30 s | 0 / 0 | none | 0.6 / 1.0 | 0.47 | 52.4K / 43.0K | 110.0K | 0.989 | 2.0 | 47.6 / 0.0 | 26 % |
| G7 punch, grace 5 s | 0 / 0 | none | 0.2 / 0.4 | 0.12 | 80.5K / 64.2K | 110.0K | 0.972 | 2.0 | 30.4 / 0.0 | 28 % |
| P5 time-to-limit pacing | 0 / 0 | none | 0.2 / 0.3 | 0.08 | 34.9K / 31.8K | 58.2K | 0.962 | 2.0 | 0.0 / 46.2 | 34 % |
| P6 ETA floor | 0 / 0 | none | 1.7 / 2.2 | 1.34 | 44.2K / 23.9K | 110.0K | 0.991 | 2.0 | 4.3 / 45.0 | 28 % |
| G2 condemn at each cut | 0 / 0 | 38 (7/16) | 1.0 / 1.1 | 1.02 | 45.6K / 40.2K | 110.0K | 1.000 | 2.0 | 19.4 / 44.4 | 22 % |
| P1' + P6 (ETA floor) | 0 / 0 | none | 1.7 / 2.2 | 1.34 | 44.2K / 23.9K | 110.0K | 0.991 | 2.0 | 4.3 / 45.0 | 28 % |
| C6 gap 1 s only | 2 / 2 | 37 (16/16) | 11.3 / 14.6 | 1.02 | 46.1K / 36.8K | 110.0K | 1.003 | 2.0 | 2.4 / 52.2 | 34 % |
| C5 condemn at cut + gap 1 s | 0 / 0 | 38 (3/16) | 1.0 / 1.0 | 1.02 | 45.7K / 42.1K | 110.0K | 0.998 | 2.0 | 16.0 / 51.2 | 30 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 0 / 0 | none | 0.1 / 0.1 | 0.07 | 91.2K / 85.2K | 110.0K | 0.939 | 2.0 | 103.3 / 19.5 | 28 % |
| P1' + C5 + gap 1 s | 0 / 0 | none | 0.8 / 1.0 | 0.71 | 44.4K / 40.4K | 110.0K | 0.980 | 2.0 | 16.0 / 50.4 | 30 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 0.1 / 0.2 | 0.11 | 72.1K / 61.1K | 110.0K | 0.956 | 2.0 | 2.6 / 84.0 | 36 % |
| combo B: A + P0a | 0 / 0 | none | 0.2 / 0.2 | 0.14 | 72.2K / 61.2K | 110.0K | 0.958 | 2.0 | 2.6 / 83.9 | 36 % |
| combo C: P2 + P1 | 0 / 0 | none | 1.0 / 1.0 | 1.02 | 44.7K / 26.4K | 110.0K | 0.988 | 2.0 | 2.1 / 49.3 | 29 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 1.0 / 1.0 | 1.01 | 47.2K / 39.4K | 110.0K | 0.996 | 2.0 | 3.1 / 53.3 | 29 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 0.1 / 0.1 | 0.08 | 89.9K / 76.7K | 110.0K | 0.944 | 2.0 | 16.4 / 0.0 | 32 % |

#### M15 8 GiB, faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 1 | 55 (17/24) | 36.8 / 37.5 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % |
| gate-gc | 0 / 0 | 98 (10/24) | 37.0 / 65.3 | 9.22 | 15.3K / 11.5K | 82.2K | 0.989 | 1.4 | 0.0 / 19.5 | 8 % |
| leadzone | 1 / 1 | 55 (17/24) | 36.8 / 37.5 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % |
| P0a bound compressed | 2 / 6 | 33 (24/24) | 26.4 / 32.7 | 2.76 | 22.1K / 17.4K | 82.2K | 1.035 | 1.4 | 0.0 / 29.1 | 21 % |
| P0b true-up early | 1 / 1 | 55 (17/24) | 36.8 / 37.5 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % |
| P0c limit anchored on disk | 1 / 1 | 55 (17/24) | 36.8 / 37.5 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % |
| P1 pace in store bytes | 0 / 2 | 63 (14/24) | 35.4 / 57.6 | 2.57 | 17.2K / 12.9K | 82.2K | 1.026 | 1.4 | 0.0 / 22.5 | 20 % |
| P2 growth pass 1 GiB | 1 / 0 | 49 (24/24) | 37.2 / 37.4 | 34.21 | 15.1K / 16.5K | 82.3K | 1.016 | 1.4 | 0.0 / 19.8 | 29 % |
| P2 growth pass 0.25 GiB | 1 / 2 | 65 (22/24) | 36.9 / 37.4 | 28.66 | 13.9K / 16.5K | 82.3K | 1.031 | 1.4 | 0.0 / 19.8 | 27 % |
| G9 grace from death | 1 / 2 | 52 (15/24) | 33.9 / 37.4 | 8.06 | 23.1K / 6.7K | 82.2K | 1.031 | 1.4 | 0.0 / 12.4 | 16 % |
| P3 grace 15 s | 1 / 1 | 55 (17/24) | 36.5 / 39.9 | 2.93 | 20.3K / 12.0K | 82.2K | 1.029 | 1.4 | 0.0 / 20.2 | 26 % |
| P3 grace 10 s | 0 / 0 | 60 (13/24) | 41.4 / 67.7 | 13.93 | 22.8K / 8.3K | 82.2K | 1.006 | 1.4 | 0.0 / 18.0 | 21 % |
| P3 grace 5 s | 1 / 3 | 58 (17/24) | 16.5 / 17.6 | 2.30 | 28.2K / 24.8K | 82.2K | 1.029 | 1.4 | 0.0 / 38.1 | 27 % |
| P3 grace 2 s | 1 / 3 | 58 (17/24) | 16.5 / 17.6 | 2.30 | 28.2K / 24.8K | 82.2K | 1.029 | 1.4 | 0.0 / 38.1 | 27 % |
| P3 grace 0 s | 1 / 3 | 58 (17/24) | 16.5 / 17.6 | 2.30 | 28.2K / 24.8K | 82.2K | 1.029 | 1.4 | 0.0 / 38.1 | 27 % |
| P4 grace 5 s + gap 1 s | 2 / 5 | 38 (17/24) | 12.3 / 15.4 | 1.37 | 32.9K / 35.5K | 82.2K | 1.031 | 1.4 | 0.0 / 52.1 | 37 % |
| P4 grace 0 s + gap 1 s | 2 / 7 | 38 (17/24) | 9.9 / 13.7 | 1.53 | 41.1K / 41.4K | 82.2K | 1.032 | 1.4 | 0.0 / 57.7 | 37 % |
| G5 threshold from headroom | 1 / 1 | 55 (17/24) | 36.7 / 37.9 | 26.69 | 16.5K / 12.0K | 82.2K | 1.032 | 1.4 | 0.0 / 20.0 | 24 % |
| G1 batched index work | 1 / 1 | 54 (16/24) | 32.9 / 65.7 | 6.59 | 17.8K / 11.6K | 82.2K | 1.024 | 1.4 | 0.0 / 19.7 | 25 % |
| G3 incremental mark | 0 / 0 | 87 (10/24) | 62.4 / 65.4 | 16.51 | 15.2K / 6.0K | 82.2K | 0.985 | 1.4 | 0.0 / 13.3 | 26 % |
| G4 two collector threads | 1 / 0 | 58 (16/24) | 36.7 / 65.2 | 26.40 | 15.1K / 12.0K | 82.2K | 1.023 | 1.4 | 0.0 / 19.8 | 32 % |
| G1+G3+G4 | 0 / 0 | 101 (9/24) | 61.0 / 61.4 | 9.99 | 15.2K / 6.0K | 82.2K | 0.985 | 1.4 | 0.0 / 13.3 | 24 % |
| G7 punch, grace 30 s | 0 / 0 | none | 37.0 / 126.1 | 0.43 | 20.3K / 19.9K | 82.2K | 0.986 | 1.4 | 0.4 / 0.0 | 3 % |
| G7 punch, grace 5 s | 0 / 0 | none | 32.4 / 65.4 | 2.06 | 30.4K / 53.3K | 82.2K | 0.987 | 1.4 | 0.5 / 0.0 | 26 % |
| P5 time-to-limit pacing | 0 / 3 | 107 (23/24) | 31.9 / 36.3 | 6.62 | 18.4K / 15.3K | 43.0K | 1.028 | 1.4 | 0.0 / 25.6 | 28 % |
| P6 ETA floor | 0 / 3 | 63 (15/24) | 35.3 / 38.6 | 4.59 | 16.9K / 16.1K | 82.2K | 1.028 | 1.4 | 0.0 / 20.0 | 19 % |
| G2 condemn at each cut | 1 / 2 | 54 (17/24) | 35.4 / 36.1 | 12.38 | 19.1K / 13.4K | 82.2K | 1.029 | 1.4 | 0.0 / 23.0 | 23 % |
| P1' + P6 (ETA floor) | 0 / 3 | 63 (15/24) | 35.3 / 38.6 | 4.59 | 16.9K / 16.1K | 82.2K | 1.028 | 1.4 | 0.0 / 20.0 | 19 % |
| C6 gap 1 s only | 0 / 3 | 59 (15/24) | 36.5 / 66.3 | 6.55 | 15.3K / 15.3K | 82.2K | 1.028 | 1.4 | 0.0 / 23.8 | 19 % |
| C5 condemn at cut + gap 1 s | 0 / 3 | 95 (16/24) | 35.7 / 38.6 | 4.92 | 17.0K / 15.5K | 82.2K | 1.032 | 1.4 | 0.0 / 27.9 | 18 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 2 / 5 | 38 (17/24) | 9.0 / 9.4 | 0.42 | 49.4K / 47.8K | 82.2K | 1.031 | 1.4 | 0.0 / 70.9 | 39 % |
| P1' + C5 + gap 1 s | 1 / 6 | 62 (16/24) | 29.6 / 35.3 | 1.02 | 18.9K / 19.6K | 82.2K | 1.031 | 1.4 | 0.0 / 31.2 | 21 % |
| combo A: P2 + grace 5 s + gap 1 s | 3 / 5 | 28 (24/24) | 12.7 / 15.4 | 1.95 | 35.6K / 36.5K | 82.3K | 1.032 | 1.4 | 0.0 / 53.6 | 39 % |
| combo B: A + P0a | 4 / 4 | 23 (24/24) | 11.6 / 16.1 | 1.01 | 38.3K / 38.2K | 82.3K | 1.034 | 1.4 | 0.1 / 57.6 | 44 % |
| combo C: P2 + P1 | 0 / 5 | 103 (20/24) | 37.4 / 38.6 | 4.09 | 15.3K / 16.8K | 82.3K | 1.030 | 1.4 | 0.0 / 23.7 | 22 % |
| combo D: P2 + G9 + P0a | 4 / 6 | 19 (24/24) | 32.0 / 32.4 | 1.02 | 25.0K / 19.7K | 82.3K | 1.035 | 1.4 | 0.1 / 33.4 | 24 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 10.1 / 12.4 | 0.78 | 64.6K / 56.1K | 82.2K | 0.983 | 1.4 | 0.6 / 0.0 | 40 % |

#### M15 8 GiB, stall regime

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 2 / 3 | 18 (24/24) | 35.3 / 36.1 | 14.63 | 18.4K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 29.8 | 20 % |
| gate-gc | 2 / 3 | 14 (24/24) | 35.5 / 47.0 | 31.75 | 16.2K / 13.4K | 83.4K | 1.035 | 2.0 | 0.0 / 28.5 | 17 % |
| leadzone | 2 / 3 | 18 (24/24) | 35.3 / 36.1 | 14.63 | 18.4K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 29.8 | 20 % |
| P0a bound compressed | 2 / 2 | 18 (24/24) | 32.7 / 35.8 | 14.58 | 18.4K / 14.9K | 83.5K | 1.035 | 2.0 | 0.0 / 30.1 | 19 % |
| P0b true-up early | 2 / 3 | 18 (24/24) | 35.0 / 35.8 | 14.71 | 18.5K / 15.0K | 83.4K | 1.035 | 2.0 | 0.0 / 29.7 | 18 % |
| P0c limit anchored on disk | 2 / 3 | 18 (24/24) | 35.3 / 36.1 | 14.63 | 18.4K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 29.8 | 20 % |
| P1 pace in store bytes | 2 / 3 | 22 (24/24) | 32.2 / 36.8 | 14.41 | 18.2K / 15.0K | 81.2K | 1.028 | 2.0 | 0.0 / 30.7 | 24 % |
| P2 growth pass 1 GiB | 3 / 2 | 14 (24/24) | 35.7 / 35.9 | 27.89 | 18.3K / 15.0K | 83.5K | 1.035 | 2.0 | 0.0 / 29.8 | 22 % |
| P2 growth pass 0.25 GiB | 3 / 3 | 20 (24/24) | 35.7 / 35.8 | 29.78 | 18.2K / 15.0K | 83.5K | 1.035 | 2.0 | 0.0 / 29.6 | 21 % |
| G9 grace from death | 2 / 4 | 18 (24/24) | 32.7 / 34.1 | 14.76 | 18.7K / 15.6K | 83.4K | 1.035 | 2.0 | 0.0 / 34.5 | 19 % |
| P3 grace 15 s | 2 / 4 | 18 (24/24) | 32.6 / 33.4 | 15.88 | 18.7K / 16.1K | 83.4K | 1.035 | 2.0 | 0.0 / 34.5 | 17 % |
| P3 grace 10 s | 3 / 3 | 18 (24/24) | 19.3 / 20.4 | 1.02 | 26.7K / 20.6K | 83.4K | 1.034 | 2.0 | 0.0 / 45.8 | 29 % |
| P3 grace 5 s | 2 / 0 | 18 (23/24) | 19.5 / 21.1 | 0.95 | 27.5K / 26.8K | 83.4K | 1.034 | 2.0 | 0.0 / 52.5 | 37 % |
| P3 grace 2 s | 2 / 0 | 18 (23/24) | 19.5 / 21.1 | 0.95 | 27.5K / 26.8K | 83.4K | 1.034 | 2.0 | 0.0 / 52.5 | 37 % |
| P3 grace 0 s | 2 / 0 | 18 (23/24) | 19.5 / 21.1 | 0.95 | 27.5K / 26.8K | 83.4K | 1.034 | 2.0 | 0.0 / 52.5 | 37 % |
| P4 grace 5 s + gap 1 s | 3 / 5 | 17 (24/24) | 17.0 / 25.9 | 1.02 | 27.5K / 27.8K | 83.4K | 1.035 | 2.0 | 0.0 / 56.7 | 38 % |
| P4 grace 0 s + gap 1 s | 1 / 0 | 17 (22/24) | 11.2 / 18.1 | 0.49 | 35.9K / 32.6K | 83.4K | 1.028 | 2.0 | 0.0 / 65.3 | 46 % |
| G5 threshold from headroom | 2 / 2 | 18 (24/24) | 32.4 / 33.5 | 31.28 | 18.5K / 15.3K | 83.4K | 1.035 | 2.0 | 0.0 / 30.4 | 12 % |
| G1 batched index work | 2 / 4 | 18 (24/24) | 32.0 / 32.2 | 21.31 | 18.2K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 34.6 | 13 % |
| G3 incremental mark | 3 / 5 | 17 (24/24) | 31.1 / 40.0 | 13.21 | 18.2K / 14.8K | 83.4K | 1.035 | 2.0 | 0.0 / 32.3 | 14 % |
| G4 two collector threads | 2 / 4 | 18 (24/24) | 35.1 / 35.9 | 16.80 | 18.1K / 15.0K | 83.4K | 1.035 | 2.0 | 0.0 / 31.5 | 23 % |
| G1+G3+G4 | 2 / 5 | 16 (24/24) | 29.4 / 35.0 | 9.73 | 17.7K / 16.1K | 83.4K | 1.035 | 2.0 | 0.0 / 33.3 | 8 % |
| G7 punch, grace 30 s | 5 / 5 | 22 (24/24) | 6.2 / 31.6 | 1.02 | 22.8K / 19.5K | 83.4K | 1.013 | 2.0 | 1.1 / 0.0 | 22 % |
| G7 punch, grace 5 s | 3 / 0 | 22 (24/24) | 6.1 / 31.6 | 1.01 | 33.9K / 39.5K | 83.4K | 1.010 | 2.0 | 0.4 / 0.0 | 37 % |
| P5 time-to-limit pacing | 1 / 0 | 56 (15/24) | 1.0 / 6.1 | 0.75 | 14.0K / 12.8K | 28.2K | 1.001 | 2.0 | 0.0 / 29.8 | 32 % |
| P6 ETA floor | 2 / 2 | 23 (24/24) | 32.7 / 37.1 | 14.86 | 18.3K / 15.0K | 81.2K | 1.030 | 2.0 | 0.0 / 30.8 | 24 % |
| G2 condemn at each cut | 2 / 4 | 18 (24/24) | 31.7 / 32.1 | 14.53 | 18.2K / 16.3K | 83.4K | 1.035 | 2.0 | 0.0 / 34.4 | 18 % |
| P1' + P6 (ETA floor) | 2 / 2 | 23 (24/24) | 32.7 / 37.1 | 14.86 | 18.3K / 15.0K | 81.2K | 1.030 | 2.0 | 0.0 / 30.8 | 24 % |
| C6 gap 1 s only | 2 / 5 | 17 (24/24) | 32.3 / 35.9 | 21.57 | 18.2K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 34.5 | 21 % |
| C5 condemn at cut + gap 1 s | 2 / 5 | 16 (24/24) | 32.1 / 32.7 | 10.32 | 18.8K / 18.8K | 83.4K | 1.034 | 2.0 | 0.0 / 39.9 | 26 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 1 / 0 | 18 (12/24) | 0.8 / 12.0 | 0.26 | 49.9K / 46.4K | 83.4K | 1.001 | 2.0 | 0.0 / 91.7 | 45 % |
| P1' + C5 + gap 1 s | 3 / 4 | 17 (24/24) | 29.0 / 32.4 | 1.02 | 18.9K / 18.2K | 81.2K | 1.023 | 2.0 | 0.0 / 39.4 | 29 % |
| combo A: P2 + grace 5 s + gap 1 s | 3 / 5 | 17 (24/24) | 11.9 / 16.5 | 1.02 | 30.3K / 28.4K | 83.5K | 1.034 | 2.0 | 0.0 / 58.9 | 41 % |
| combo B: A + P0a | 2 / 4 | 17 (24/24) | 15.9 / 16.7 | 1.02 | 29.7K / 27.8K | 83.6K | 1.035 | 2.0 | 0.0 / 58.5 | 41 % |
| combo C: P2 + P1 | 2 / 5 | 22 (24/24) | 29.5 / 31.1 | 14.09 | 18.3K / 14.3K | 81.4K | 1.029 | 2.0 | 0.0 / 30.1 | 24 % |
| combo D: P2 + G9 + P0a | 2 / 3 | 21 (24/24) | 32.3 / 32.4 | 30.98 | 18.3K / 15.2K | 83.6K | 1.035 | 2.0 | 0.0 / 33.3 | 17 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | 91 (5/24) | 0.6 / 0.7 | 0.36 | 50.5K / 48.8K | 83.5K | 0.996 | 2.0 | 0.5 / 0.0 | 49 % |

#### network volume 64K, faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy |
|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| gate-gc | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| leadzone | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P0a bound compressed | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.107 | 7.5 | 0.0 / 2.8 | 22 % |
| P0b true-up early | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P0c limit anchored on disk | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P1 pace in store bytes | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P2 growth pass 1 GiB | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P2 growth pass 0.25 GiB | 0 / 0 | none | 7.6 / 7.6 | 7.41 | 2.0K / 0.8K | 11.3K | 0.109 | 7.5 | 0.0 / 2.6 | 25 % |
| G9 grace from death | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P3 grace 15 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P3 grace 10 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P3 grace 5 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P3 grace 2 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P3 grace 0 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P4 grace 5 s + gap 1 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P4 grace 0 s + gap 1 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| G5 threshold from headroom | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| G1 batched index work | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| G3 incremental mark | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 21 % |
| G4 two collector threads | 0 / 0 | none | 7.6 / 7.6 | 7.31 | 2.0K / 0.8K | 11.3K | 0.123 | 7.5 | 0.0 / 3.0 | 39 % |
| G1+G3+G4 | 0 / 0 | none | 7.6 / 7.6 | 7.31 | 2.0K / 0.8K | 11.3K | 0.123 | 7.5 | 0.0 / 3.6 | 36 % |
| G7 punch, grace 30 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.7K | 11.3K | 0.090 | 7.5 | 0.0 / 0.0 | 1 % |
| G7 punch, grace 5 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.7K | 11.3K | 0.090 | 7.5 | 0.0 / 0.0 | 1 % |
| P5 time-to-limit pacing | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P6 ETA floor | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| G2 condemn at each cut | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P1' + P6 (ETA floor) | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| C6 gap 1 s only | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| C5 condemn at cut + gap 1 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| C5+C7 condemn at cut, gap 1 s, grace 2 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| P1' + C5 + gap 1 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| combo A: P2 + grace 5 s + gap 1 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| combo B: A + P0a | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.107 | 7.5 | 0.0 / 2.8 | 22 % |
| combo C: P2 + P1 | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.110 | 7.5 | 0.0 / 2.8 | 22 % |
| combo D: P2 + G9 + P0a | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.8K | 11.3K | 0.107 | 7.5 | 0.0 / 2.8 | 22 % |
| combo E: G7 + P2 + grace 5 s | 0 / 0 | none | 7.6 / 7.6 | 7.56 | 2.0K / 0.7K | 11.3K | 0.090 | 7.5 | 0.0 / 0.0 | 1 % |

## Round 3: the missing rows, the gap split, a costed C5, a lagging estimator, the dead-time split and a band-reaching network cell

Same seeds and regimes as round 2 (32 seeds RAM 64K, 24 M15, 8 network volume); `spacesim experiments --round3`. **The RAM 16K rows are not evidence**: the measured cell stopped in 0 of 6 repetitions and the simulation's
16K rows stop in neither regime in the way the cell does (tables at the end, for completeness only). The network-volume cell is now `netvol-band` (cap 6 GiB): it reaches the band at about 92 s and stops in the third cell, so
"unchanged" there is a real no-regression result. New model pieces: `fast_recheck` (a recheck-only pass, no compaction, 1 s after a grace ends), `cut_cadence_s` (a forced FULL every X s while the gate is not open and journal bytes wait),
`c5_cost` (a proposal at each cut costs a list sync plus `condemn_ms` per flag of coordinator time, is skipped while a pass is running, and can fail with `c5_error`), `a_lag` / `a_bias` (A from the last 8 cuts, byte-weighted, maximum
with the last, times 1 + bias; the zero-mean wobble is off), and the delete term of the dead time.

#### 1. The missing rows and the gap split

cells: `stops first-fill/steady; longest median / p90 s; dwell s (throttle on to first stop)`

| row | RAM 64K faithful | RAM 64K stall regime | M15 faithful | M15 stall regime | network volume (band) |
|---|---|---|---|---|---|
| base (main) | 1 / 2; 35.1 / 68.3; 4.6 | 4 / 5; 31.0 / 35.9; 4.7 | 1 / 1; 36.8 / 37.5; 44.5 | 2 / 3; 35.3 / 36.1; 11.5 | 0 / 3; 11.0 / 12.1; 40.6 |
| P1' alone | 0 / 0; 4.3 / 50.9; 20.1 | 2 / 0; 19.5 / 23.6; 22.4 | 0 / 2; 35.4 / 57.6; 52.0 | 2 / 3; 32.2 / 36.8; 15.8 | 0 / 3; 8.5 / 8.6; 36.0 |
| P1' + C5 free (no gap) | 0 / 0; 2.4 / 3.5; 16.0 | 2 / 0; 5.9 / 11.5; 21.5 | 0 / 3; 32.7 / 36.2; 90.8 | 2 / 3; 27.5 / 32.1; 14.4 | 0 / 3; 8.5 / 8.6; 35.6 |
| P1' + gap 1 s (no C5) | 0 / 0; 6.9 / 16.6; 62.6 | 2 / 0; 23.5 / 27.4; 17.0 | 0 / 5; 33.2 / 37.1; 92.3 | 2 / 3; 31.9 / 36.9; 13.2 | 0 / 3; 8.5 / 8.6; 36.0 |
| P1' + C5 free + gap 1 s | 0 / 0; 1.0 / 4.8; 82.7 | 1 / 0; 5.4 / 12.4; 21.1 | 1 / 6; 29.6 / 35.3; 51.7 | 3 / 4; 29.0 / 32.4; 10.7 | 0 / 3; 8.5 / 8.6; 35.6 |
| P1' + C5 free + (a) fast recheck | 0 / 0; 0.8 / 1.1; 16.0 | 2 / 0; 7.0 / 12.4; 22.3 | 0 / 1; 34.9 / 36.6; 89.3 | 2 / 3; 32.2 / 34.1; 14.4 | 0 / 1; 8.5 / 8.6; 35.4 |
| P1' + C5 free + (b) cadence 2 s | 1 / 0; 5.3 / 11.2; 17.2 | 2 / 0; 11.1 / 11.7; 17.8 | 4 / 10; 24.7 / 29.3; 3.8 | 3 / 3; 27.2 / 31.8; 13.7 | 0 / 3; 8.5 / 15.2; 50.5 |
| P1' + (b) cadence 2 s (no C5) | 1 / 2; 12.5 / 30.5; 15.6 | 2 / 2; 20.4 / 30.2; 19.9 | 3 / 6; 27.5 / 32.5; 3.8 | 2 / 2; 32.5 / 36.9; 15.9 | 0 / 3; 8.5 / 15.0; 51.3 |
| gap 1 s (collector pass gap) | 1 / 4; 13.8 / 68.3; 4.0 | 4 / 5; 31.0 / 32.9; 4.7 | 0 / 3; 36.5 / 66.3; 47.9 | 2 / 5; 32.3 / 35.9; 10.4 | 0 / 3; 10.8 / 12.1; 40.6 |
| gap (a) fast recheck-only pass | 1 / 2; 15.5 / 58.8; 4.6 | 4 / 7; 30.4 / 32.0; 4.7 | 1 / 1; 36.8 / 37.6; 44.5 | 3 / 4; 38.5 / 45.0; 11.5 | 0 / 3; 10.8 / 14.7; 39.9 |
| gap (b) cut cadence 1 s | 1 / 2; 33.2 / 39.9; 31.0 | 4 / 4; 31.4 / 35.5; 9.0 | 5 / 9; 33.0 / 35.3; 2.7 | 2 / 3; 35.5 / 36.6; 11.6 | 0 / 5; 8.2 / 14.7; 34.0 |
| gap (b) cut cadence 2 s | 2 / 4; 16.7 / 40.3; 4.2 | 5 / 4; 32.4 / 35.6; 7.3 | 2 / 6; 35.0 / 37.3; 44.2 | 3 / 3; 35.5 / 36.6; 11.6 | 0 / 2; 8.4 / 9.6; 35.1 |
| gap (b) cut cadence 5 s | 1 / 2; 37.7 / 62.7; 4.6 | 4 / 4; 31.5 / 35.4; 4.7 | 1 / 1; 36.8 / 57.6; 44.5 | 3 / 3; 35.5 / 36.0; 11.0 | 0 / 3; 10.6 / 12.3; 40.6 |

Reading: the work is done by C5. `P1' + C5` (no gap) is 2.4 / 3.5 s faithful and 5.9 / 11.5 s in the stall regime; adding the 1 s gap changes it little (1.0 / 4.8 and 5.4 / 12.4), `P1' + gap` without C5 is weak (6.9 / 16.6, 23.5 / 27.4).
The gap split settles the mechanism: the recheck-only pass (a) takes the recheck term from 4.8 s to 1.1 s and gives the best faithful row (`P1' + C5 + (a)`: 0.8 / 1.1 s, 43.5 K IOPS) at a lower collector cost than the 1 s gap, while an
explicit cut cadence (b) does not reproduce the gap's effect (faithful 5.3 / 11.2 s with P1' + C5, 16.7-37.7 s alone): **the gap works through the recheck, not through the cut cadence**, and a fast recheck-only pass is the right
form of it. No row helps M15 (cap-bound): its best is `P1' + C5 + (b) 2 s` at 24.7-27.2 s.

#### 2. C5 with its costs

cells: `stops first-fill/steady; longest median / p90 s; dwell s (throttle on to first stop)`

| row | RAM 64K faithful | RAM 64K stall regime | M15 faithful | M15 stall regime | network volume (band) |
|---|---|---|---|---|---|
| C5 free | 2 / 4; 8.3 / 11.4; 4.3 | 6 / 10; 19.7 / 28.8; 4.7 | 1 / 2; 35.4 / 36.1; 43.5 | 2 / 4; 31.7 / 32.1; 11.7 | 0 / 3; 7.9 / 12.2; 40.4 |
| C5 costed (list sync, flags, skipped while a pass runs) | 2 / 4; 10.7 / 11.6; 4.2 | 5 / 8; 20.4 / 36.6; 8.2 | 1 / 1; 35.3 / 36.3; 46.0 | 3 / 4; 32.2 / 33.2; 9.7 | 0 / 3; 7.9 / 11.4; 41.9 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 2 / 4; 10.7 / 11.6; 4.2 | 6 / 9; 20.2 / 32.3; 8.7 | 1 / 1; 35.3 / 36.3; 46.0 | 3 / 5; 32.2 / 33.2; 9.7 | 0 / 3; 7.9 / 11.4; 41.9 |
| C5 costed, 20 % proposals fail | 2 / 4; 11.0 / 17.6; 4.2 | 5 / 8; 20.8 / 34.6; 19.0 | 1 / 0; 35.5 / 37.2; 46.0 | 2 / 4; 32.2 / 33.3; 9.7 | 0 / 3; 8.6 / 11.4; 41.9 |
| P1' + C5 free + gap 1 s | 0 / 0; 1.0 / 4.8; 82.7 | 1 / 0; 5.4 / 12.4; 21.1 | 1 / 6; 29.6 / 35.3; 51.7 | 3 / 4; 29.0 / 32.4; 10.7 | 0 / 3; 8.5 / 8.6; 35.6 |
| P1' + C5 costed + gap 1 s | 0 / 0; 1.3 / 8.2; 81.5 | 1 / 0; 4.1 / 12.3; 24.8 | 1 / 6; 29.9 / 35.2; 51.5 | 3 / 4; 30.8 / 35.1; 10.9 | 0 / 3; 8.6 / 8.6; 35.6 |

Costing C5 (a list sync of 0.1 ms on tmpfs or 3 ms on disk, 0.015 / 5.5 ms per flag, a proposal skipped while a pass is running, 20 % failures) moves it little: RAM 64K faithful 8.3 / 11.4 s -> 10.7 / 11.6 s (11.0 / 17.6 s with
20 % failures), stall regime 19.7 / 28.8 -> 20.4 / 36.6 s; the combination with P1' and the gap 1.0 / 4.8 -> 1.3 / 8.2 s and 5.4 / 12.4 -> 4.1 / 12.3 s. **C5 still raises the stop count** (stops 1/2 -> 2/4 faithful, 4/5 -> 5/8 and 6/10 in the stall
regime; M15 1/1 -> 1/1 costed): the dead time falls (detection+retention 15.6 s -> 4.7-6.5 s) and the volume restarts more often, each stop shorter. With P1' in front the stop count is 0 / 0 in the faithful regime and 1-2 / 0 in the stall regime.

#### 3. The estimator of A: lag and bias

cells: `stops first-fill/steady; longest median / p90 s; dwell s (throttle on to first stop)`

| row | RAM 64K faithful | RAM 64K stall regime | M15 faithful | M15 stall regime | network volume (band) |
|---|---|---|---|---|---|
| P1' alone | 0 / 0; 4.3 / 50.9; 20.1 | 2 / 0; 19.5 / 23.6; 22.4 | 0 / 2; 35.4 / 57.6; 52.0 | 2 / 3; 32.2 / 36.8; 15.8 | 0 / 3; 8.5 / 8.6; 36.0 |
| P1' alone, lagged A, bias +15 % | 0 / 0; 1.0 / 13.9; 26.2 | 2 / 0; 12.1 / 20.1; 27.3 | 0 / 5; 31.2 / 37.6; 56.9 | 2 / 3; 30.3 / 36.7; 16.0 | 0 / 3; 8.6 / 12.7; 37.2 |
| P1' alone, lagged A, bias -15 % | 0 / 0; 5.7 / 61.6; - | 2 / 2; 19.8 / 23.7; 23.8 | 1 / 2; 36.5 / 70.3; 49.2 | 2 / 2; 32.2 / 36.8; 15.6 | 0 / 3; 8.5 / 8.6; 35.8 |

P1' alone is sensitive to the estimate: with A over-estimated by 15 % the faithful row improves (1.0 / 13.9 s: the pace is a little slower and the stalls disappear); with A under-estimated by 15 % the median is still 5.7 s but the p90 is 61.6 s
(the heavy tail: seeds in which the first fill reaches the band with a journal debt the estimate did not cover). The exact-estimator row of round 2 (4.3 / 50.9 s) sits between. In the stall regime the three rows are 19.5 / 23.6, 12.1 / 20.1 and 19.8 / 23.7 s.

#### 4. Dead time split and the band dwell

Dead time of a wholly dead segment, seconds, `retention+detection / grace / recheck / delete` (delete = the pass's index work between the recheck and the release of the bytes); dwell = throttle on to the first stop (**measured at RAM 64K: 6.5 s**, rep 1: on at 955.3 s, stop at 961.8 s).

| row | RAM 64K faithful: dead time; dwell | RAM 64K stall regime: dead time; dwell | network volume (band): dead time; dwell |
|---|---|---|---|
| base (main) | 15.6 / 30.0 / 4.8 / 3.8; 4.6 | 12.6 / 30.0 / 3.5 / 8.2; 4.7 | 7.4 / 30.0 / 3.2 / 0.8; 40.6 |
| P1' alone | 11.7 / 30.0 / 3.8 / 4.0; 20.1 | 10.3 / 30.0 / 3.8 / 5.9; 22.4 | 7.4 / 30.0 / 15.8 / 0.8; 36.0 |
| C5 free | 4.7 / 30.0 / 3.6 / 2.7; 4.3 | 2.1 / 30.0 / 4.0 / 4.5; 4.7 | 6.7 / 30.0 / 1.6 / 0.5; 40.4 |
| gap 1 s (collector pass gap) | 7.1 / 30.0 / 1.5 / 3.2; 4.0 | 9.2 / 30.0 / 4.0 / 6.3; 4.7 | 7.4 / 30.0 / 1.7 / 0.8; 40.6 |
| gap (a) fast recheck-only pass | 12.9 / 30.0 / 1.1 / 3.5; 4.6 | 9.6 / 30.0 / 1.1 / 6.6; 4.7 | 7.4 / 30.0 / 1.6 / 0.7; 39.9 |
| gap (b) cut cadence 2 s | 13.2 / 30.0 / 4.2 / 4.1; 4.2 | 9.4 / 30.0 / 5.7 / 6.7; 7.3 | 7.4 / 30.0 / 3.1 / 0.0; 35.1 |
| P1' + C5 free + gap 1 s | 2.4 / 30.0 / 1.6 / 1.7; 82.7 | 0.8 / 30.0 / 2.2 / 3.3; 21.1 | 6.7 / 30.0 / 16.2 / 0.5; 35.6 |
| P1' + C5 free + (a) fast recheck | 4.7 / 30.0 / 1.0 / 1.2; 16.0 | 1.2 / 30.0 / 1.4 / 2.8; 22.3 | 6.6 / 30.0 / 9.9 / 0.5; 35.4 |

The measured dead time of the first condemn in rep 1 was 33 s (30 s grace plus about 3 s of recheck and delete); the model's base is 15.6 + 30 + 4.8 + 3.8 s for the segments deleted over the whole run (the first batch is shorter; the mean includes
segments that die long before the pass that finds them). The simulated band dwell of the base is 4.6 s (faithful) and 4.7 s (stall regime) against 6.5 s measured: about 30 % short, in the right range. P1' lengthens it to 20-26 s, which is its purpose.

#### 5. The network volume reaching the band (`netvol-band`, all rows)

cells: `stops first-fill/steady; longest median / p90 s; dwell s (throttle on to first stop)`

| row | network volume (band) |
|---|---|
| base (main) | 0 / 3; 11.0 / 12.1; 40.6 |
| P1' alone | 0 / 3; 8.5 / 8.6; 36.0 |
| C5 free | 0 / 3; 7.9 / 12.2; 40.4 |
| gap (a) fast recheck-only pass | 0 / 3; 10.8 / 14.7; 39.9 |
| P1' + C5 free + gap 1 s | 0 / 3; 8.5 / 8.6; 35.6 |
| P1' + C5 free + (a) fast recheck | 0 / 1; 8.5 / 8.6; 35.4 |

The cell enters the band at about 92 s and stops in the third cell (longest completion 11.0 / 12.1 s, throughput 2.0 K / 0.7 K IOPS); no candidate changes the throughput, none makes the stops or the longest wait worse
(P1' alone 8.5 / 8.6 s, C5 7.9 / 12.2 s): the no-regression check holds with a cell that is actually in the band. Its dead-time split shows the long recheck of P1' there (15.8 s: its slow pass cadence at 22 % collector).

### Revised verdict

* **P1' alone** (the real test's first step): it removes the stops on RAM 64K at the median (faithful: stops 0/0, 1 of 32 seeds stops in the first cell; longest 4.3 s against 35.1 s) and halves the stall in the stall regime (19.5 against 31.0 s), at no throughput cost, **but its p90 is
  50.9 s (61.6 s with A under-estimated by 15 %, 13.9 s with A over-estimated): a heavy tail the median hides**. Bias the estimate high, and judge the real test on p90 and the worst run, not the median. It does nothing for a cap-bound volume (M15: 35.4 / 57.6 s) and a
  little on the network-volume band (8.5 against 11.0 s). Confidence in its direction: medium; in its size: low.
* **Next, C5 (condemn-on-cut), in its recheck-only form**: it is the part of the earlier combination that does the work (`P1' + C5` 2.4 / 3.5 s faithful and 5.9 / 11.5 s in the stall regime without any gap; `P1' + C5 + (a) fast recheck` 0.8 / 1.1 s faithful), costed it still
  holds (within 20 %), it raises the stop count (shorter, more frequent) unless P1' is in front, and the 1 s collector gap is not needed once the fast recheck-only pass exists.
* **Short grace behind reader safety** stays the only lever on a cap-bound volume (round 2) and was not re-run here.
* Not done in this round: skew workloads, G6, P7.

#### Tables, RAM 64K, faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 2 | 25 (32/32) | 35.1 / 68.3 | 0.66 | 35.8K / 28.9K | 64.7K | 1.011 | 2.4 | 30.7 / 16.8 | 18 % | 4.6 | 15.6 / 30.0 / 4.8 / 3.8 |
| P1' alone | 0 / 0 | 40 (1/32) | 4.3 / 50.9 | 0.78 | 36.1K / 29.9K | 64.7K | 1.008 | 2.4 | 30.1 / 23.0 | 25 % | 20.1 | 11.7 / 30.0 / 3.8 / 4.0 |
| P1' alone, lagged A, bias +15 % | 0 / 0 | 45 (3/32) | 1.0 / 13.9 | 1.02 | 35.6K / 28.2K | 64.7K | 1.009 | 2.4 | 29.5 / 22.9 | 24 % | 26.2 | 11.4 / 30.0 / 4.3 / 4.3 |
| P1' alone, lagged A, bias -15 % | 0 / 0 | none | 5.7 / 61.6 | 0.67 | 36.6K / 30.0K | 64.7K | 1.008 | 2.4 | 30.8 / 22.7 | 23 % | - | 12.5 / 30.0 / 4.2 / 3.7 |
| C5 free | 2 / 4 | 25 (32/32) | 8.3 / 11.4 | 0.66 | 40.3K / 38.3K | 64.7K | 1.014 | 2.4 | 41.6 / 23.3 | 15 % | 4.3 | 4.7 / 30.0 / 3.6 / 2.7 |
| C5 costed (list sync, flags, skipped while a pass runs) | 2 / 4 | 25 (30/32) | 10.7 / 11.6 | 0.71 | 39.7K / 35.5K | 64.6K | 1.014 | 2.4 | 38.9 / 23.0 | 14 % | 4.2 | 6.5 / 30.0 / 3.6 / 2.7 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 2 / 4 | 25 (30/32) | 10.7 / 11.6 | 0.71 | 39.7K / 35.8K | 64.6K | 1.013 | 2.4 | 38.3 / 23.7 | 15 % | 4.2 | 6.6 / 30.0 / 3.6 / 2.7 |
| C5 costed, 20 % proposals fail | 2 / 4 | 25 (30/32) | 11.0 / 17.6 | 0.70 | 38.9K / 34.8K | 64.6K | 1.012 | 2.4 | 37.8 / 21.3 | 15 % | 4.2 | 7.3 / 30.0 / 3.6 / 3.0 |
| gap 1 s (collector pass gap) | 1 / 4 | 25 (29/32) | 13.8 / 68.3 | 0.65 | 39.0K / 36.3K | 64.7K | 1.014 | 2.4 | 28.3 / 36.0 | 31 % | 4.0 | 7.1 / 30.0 / 1.5 / 3.2 |
| gap (a) fast recheck-only pass | 1 / 2 | 25 (32/32) | 15.5 / 58.8 | 0.67 | 36.3K / 34.0K | 64.7K | 1.011 | 2.4 | 41.9 / 14.4 | 24 % | 4.6 | 12.9 / 30.0 / 1.1 / 3.5 |
| gap (b) cut cadence 1 s | 1 / 2 | 24 (16/32) | 33.2 / 39.9 | 0.67 | 35.0K / 26.8K | 64.7K | 1.016 | 2.4 | 34.3 / 17.2 | 15 % | 31.0 | 11.3 / 30.0 / 4.0 / 4.2 |
| gap (b) cut cadence 2 s | 2 / 4 | 25 (31/32) | 16.7 / 40.3 | 0.66 | 35.3K / 28.9K | 64.7K | 1.011 | 2.4 | 34.3 / 17.8 | 16 % | 4.2 | 13.2 / 30.0 / 4.2 / 4.1 |
| gap (b) cut cadence 5 s | 1 / 2 | 25 (32/32) | 37.7 / 62.7 | 0.66 | 35.8K / 26.8K | 64.7K | 1.009 | 2.4 | 31.8 / 16.2 | 17 % | 4.6 | 15.6 / 30.0 / 4.9 / 3.8 |
| P1' + C5 free (no gap) | 0 / 0 | 37 (1/32) | 2.4 / 3.5 | 0.67 | 38.9K / 36.5K | 64.7K | 1.006 | 2.4 | 33.7 / 29.6 | 20 % | 16.0 | 4.3 / 30.0 / 4.0 / 2.5 |
| P1' + gap 1 s (no C5) | 0 / 0 | 31 (9/32) | 6.9 / 16.6 | 0.94 | 37.0K / 32.2K | 64.7K | 1.013 | 2.4 | 27.1 / 39.2 | 36 % | 62.6 | 6.3 / 30.0 / 1.9 / 3.4 |
| P1' + C5 free + gap 1 s | 0 / 0 | 39 (2/32) | 1.0 / 4.8 | 0.65 | 41.2K / 35.0K | 64.7K | 1.012 | 2.4 | 21.6 / 54.6 | 34 % | 82.7 | 2.4 / 30.0 / 1.6 / 1.7 |
| P1' + C5 costed + gap 1 s | 0 / 0 | 33 (4/32) | 1.3 / 8.2 | 0.67 | 38.5K / 35.2K | 64.6K | 1.014 | 2.4 | 22.6 / 50.5 | 30 % | 81.5 | 3.5 / 30.0 / 1.6 / 2.0 |
| P1' + C5 free + (a) fast recheck | 0 / 0 | 37 (1/32) | 0.8 / 1.1 | 0.66 | 43.5K / 39.0K | 64.7K | 1.009 | 2.4 | 59.4 / 9.6 | 27 % | 16.0 | 4.7 / 30.0 / 1.0 / 1.2 |
| P1' + C5 free + (b) cadence 2 s | 1 / 0 | 38 (16/32) | 5.3 / 11.2 | 1.02 | 35.5K / 28.9K | 64.7K | 1.014 | 2.4 | 36.9 / 26.2 | 22 % | 17.2 | 3.5 / 30.0 / 5.4 / 3.4 |
| P1' + (b) cadence 2 s (no C5) | 1 / 2 | 36 (23/32) | 12.5 / 30.5 | 1.02 | 35.4K / 21.6K | 64.7K | 1.014 | 2.4 | 31.4 / 19.3 | 24 % | 15.6 | 11.2 / 30.0 / 5.0 / 6.1 |

#### Tables, RAM 64K, stall regime

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 4 / 5 | 13 (32/32) | 31.0 / 35.9 | 1.02 | 20.7K / 15.2K | 94.3K | 1.031 | 4.1 | 38.5 / 16.3 | 13 % | 4.7 | 12.6 / 30.0 / 3.5 / 8.2 |
| P1' alone | 2 / 0 | 30 (25/32) | 19.5 / 23.6 | 1.02 | 19.8K / 13.3K | 87.9K | 1.013 | 4.2 | 24.3 / 32.7 | 24 % | 22.4 | 10.3 / 30.0 / 3.8 / 5.9 |
| P1' alone, lagged A, bias +15 % | 2 / 0 | 35 (25/32) | 12.1 / 20.1 | 1.02 | 19.5K / 13.0K | 86.7K | 1.011 | 4.2 | 22.5 / 35.1 | 24 % | 27.3 | 9.5 / 30.0 / 3.7 / 5.6 |
| P1' alone, lagged A, bias -15 % | 2 / 2 | 32 (31/32) | 19.8 / 23.7 | 1.02 | 20.3K / 14.0K | 88.1K | 1.015 | 4.1 | 28.8 / 30.8 | 22 % | 23.8 | 10.1 / 30.0 / 3.4 / 6.2 |
| C5 free | 6 / 10 | 13 (32/32) | 19.7 / 28.8 | 1.06 | 25.4K / 17.2K | 94.2K | 1.021 | 4.1 | 50.7 / 22.6 | 12 % | 4.7 | 2.1 / 30.0 / 4.0 / 4.5 |
| C5 costed (list sync, flags, skipped while a pass runs) | 5 / 8 | 17 (29/32) | 20.4 / 36.6 | 2.56 | 24.2K / 17.5K | 90.9K | 1.035 | 3.7 | 37.9 / 27.1 | 10 % | 8.2 | 5.1 / 30.0 / 3.7 / 4.4 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 6 / 9 | 17 (27/32) | 20.2 / 32.3 | 2.79 | 24.6K / 17.9K | 90.8K | 1.034 | 3.6 | 42.2 / 23.2 | 10 % | 8.7 | 4.9 / 30.0 / 3.6 / 4.4 |
| C5 costed, 20 % proposals fail | 5 / 8 | 16 (26/32) | 20.8 / 34.6 | 1.41 | 23.7K / 18.1K | 89.7K | 1.037 | 3.8 | 39.7 / 24.4 | 10 % | 19.0 | 6.1 / 30.0 / 3.6 / 4.3 |
| gap 1 s (collector pass gap) | 4 / 5 | 13 (32/32) | 31.0 / 32.9 | 1.02 | 20.1K / 15.8K | 94.3K | 1.025 | 4.1 | 36.1 / 22.4 | 13 % | 4.7 | 9.2 / 30.0 / 4.0 / 6.3 |
| gap (a) fast recheck-only pass | 4 / 7 | 13 (32/32) | 30.4 / 32.0 | 1.02 | 20.9K / 19.3K | 94.3K | 1.043 | 4.1 | 45.2 / 16.4 | 17 % | 4.7 | 9.6 / 30.0 / 1.1 / 6.6 |
| gap (b) cut cadence 1 s | 4 / 4 | 17 (32/32) | 31.4 / 35.5 | 1.04 | 20.7K / 13.4K | 94.3K | 1.025 | 4.1 | 36.0 / 19.2 | 10 % | 9.0 | 9.0 / 30.0 / 5.9 / 6.1 |
| gap (b) cut cadence 2 s | 5 / 4 | 15 (32/32) | 32.4 / 35.6 | 1.02 | 21.0K / 13.2K | 94.3K | 1.024 | 4.1 | 36.4 / 16.8 | 11 % | 7.3 | 9.4 / 30.0 / 5.7 / 6.7 |
| gap (b) cut cadence 5 s | 4 / 4 | 13 (32/32) | 31.5 / 35.4 | 1.02 | 21.0K / 13.5K | 94.3K | 1.017 | 4.1 | 37.5 / 17.2 | 11 % | 4.7 | 10.3 / 30.0 / 5.5 / 7.5 |
| P1' + C5 free (no gap) | 2 / 0 | 29 (24/32) | 5.9 / 11.5 | 1.02 | 21.9K / 14.6K | 87.9K | 1.011 | 4.2 | 38.2 / 30.8 | 18 % | 21.5 | 1.2 / 30.0 / 4.6 / 4.0 |
| P1' + gap 1 s (no C5) | 2 / 0 | 25 (28/32) | 23.5 / 27.4 | 1.02 | 20.5K / 15.7K | 87.9K | 1.012 | 4.2 | 26.7 / 43.9 | 34 % | 17.0 | 6.0 / 30.0 / 1.7 / 5.4 |
| P1' + C5 free + gap 1 s | 1 / 0 | 29 (19/32) | 5.4 / 12.4 | 1.02 | 24.8K / 15.3K | 87.9K | 1.008 | 4.2 | 38.9 / 42.8 | 28 % | 21.1 | 0.8 / 30.0 / 2.2 / 3.3 |
| P1' + C5 costed + gap 1 s | 1 / 0 | 34 (16/32) | 4.1 / 12.3 | 1.02 | 21.9K / 17.8K | 90.5K | 1.012 | 3.7 | 25.7 / 46.6 | 26 % | 24.8 | 3.9 / 30.0 / 2.0 / 4.1 |
| P1' + C5 free + (a) fast recheck | 2 / 0 | 30 (26/32) | 7.0 / 12.4 | 1.02 | 23.1K / 14.6K | 87.9K | 1.012 | 4.2 | 54.7 / 22.6 | 23 % | 22.3 | 1.2 / 30.0 / 1.4 / 2.8 |
| P1' + C5 free + (b) cadence 2 s | 2 / 0 | 26 (31/32) | 11.1 / 11.7 | 1.02 | 22.1K / 14.0K | 87.9K | 1.007 | 4.2 | 37.9 / 31.8 | 18 % | 17.8 | 1.0 / 30.0 / 4.8 / 4.1 |
| P1' + (b) cadence 2 s (no C5) | 2 / 2 | 28 (32/32) | 20.4 / 30.2 | 1.02 | 19.9K / 13.0K | 87.9K | 1.011 | 4.2 | 25.2 / 34.6 | 23 % | 19.9 | 8.8 / 30.0 / 3.7 / 5.8 |

#### Tables, network volume (band)

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 3 | none | 11.0 / 12.1 | 7.56 | 2.0K / 0.7K | 11.3K | 1.041 | 7.5 | 0.0 / 2.5 | 21 % | 40.6 | 7.4 / 30.0 / 3.2 / 0.8 |
| P1' alone | 0 / 3 | none | 8.5 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 29 % | 36.0 | 7.4 / 30.0 / 15.8 / 0.8 |
| P1' alone, lagged A, bias +15 % | 0 / 3 | none | 8.6 / 12.7 | 7.96 | 1.9K / 0.7K | 11.3K | 1.028 | 7.5 | 0.0 / 3.1 | 29 % | 37.2 | 7.3 / 30.0 / 15.6 / 0.7 |
| P1' alone, lagged A, bias -15 % | 0 / 3 | none | 8.5 / 8.6 | 7.58 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 29 % | 35.8 | 7.4 / 30.0 / 16.6 / 0.8 |
| C5 free | 0 / 3 | none | 7.9 / 12.2 | 7.56 | 2.0K / 0.8K | 11.3K | 1.026 | 7.5 | 0.0 / 2.8 | 21 % | 40.4 | 6.7 / 30.0 / 1.6 / 0.5 |
| C5 costed (list sync, flags, skipped while a pass runs) | 0 / 3 | none | 7.9 / 11.4 | 7.56 | 2.0K / 0.8K | 11.3K | 1.041 | 7.5 | 0.0 / 2.8 | 21 % | 41.9 | 6.8 / 30.0 / 2.4 / 0.5 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 0 / 3 | none | 7.9 / 11.4 | 7.56 | 2.0K / 0.8K | 11.3K | 1.041 | 7.5 | 0.0 / 2.8 | 21 % | 41.9 | 6.8 / 30.0 / 2.4 / 0.5 |
| C5 costed, 20 % proposals fail | 0 / 3 | none | 8.6 / 11.4 | 7.56 | 2.0K / 0.8K | 11.3K | 1.041 | 7.5 | 0.0 / 2.5 | 21 % | 41.9 | 6.8 / 30.0 / 2.4 / 0.5 |
| gap 1 s (collector pass gap) | 0 / 3 | none | 10.8 / 12.1 | 7.56 | 2.0K / 0.7K | 11.3K | 1.041 | 7.5 | 0.0 / 2.5 | 22 % | 40.6 | 7.4 / 30.0 / 1.7 / 0.8 |
| gap (a) fast recheck-only pass | 0 / 3 | none | 10.8 / 14.7 | 7.56 | 2.0K / 0.7K | 11.3K | 1.022 | 7.5 | 0.0 / 2.8 | 22 % | 39.9 | 7.4 / 30.0 / 1.6 / 0.7 |
| gap (b) cut cadence 1 s | 0 / 5 | none | 8.2 / 14.7 | 7.56 | 2.0K / 0.7K | 11.3K | 1.046 | 7.5 | 0.0 / 1.4 | 18 % | 34.0 | 7.4 / 30.0 / 4.5 / 0.0 |
| gap (b) cut cadence 2 s | 0 / 2 | none | 8.4 / 9.6 | 7.56 | 2.0K / 0.7K | 11.3K | 1.018 | 7.5 | 0.0 / 1.6 | 22 % | 35.1 | 7.4 / 30.0 / 3.1 / 0.0 |
| gap (b) cut cadence 5 s | 0 / 3 | none | 10.6 / 12.3 | 7.56 | 2.0K / 0.7K | 11.3K | 1.038 | 7.5 | 0.0 / 2.5 | 21 % | 40.6 | 7.4 / 30.0 / 3.2 / 0.8 |
| P1' + C5 free (no gap) | 0 / 3 | none | 8.5 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 29 % | 35.6 | 6.7 / 30.0 / 16.2 / 0.5 |
| P1' + gap 1 s (no C5) | 0 / 3 | none | 8.5 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 29 % | 36.0 | 7.4 / 30.0 / 15.8 / 0.8 |
| P1' + C5 free + gap 1 s | 0 / 3 | none | 8.5 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 29 % | 35.6 | 6.7 / 30.0 / 16.2 / 0.5 |
| P1' + C5 costed + gap 1 s | 0 / 3 | none | 8.6 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 3.1 | 28 % | 35.6 | 6.7 / 30.0 / 16.5 / 0.5 |
| P1' + C5 free + (a) fast recheck | 0 / 1 | none | 8.5 / 8.6 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 2.4 | 25 % | 35.4 | 6.6 / 30.0 / 9.9 / 0.5 |
| P1' + C5 free + (b) cadence 2 s | 0 / 3 | none | 8.5 / 15.2 | 7.56 | 1.9K / 0.7K | 11.3K | 1.029 | 7.5 | 0.0 / 2.5 | 20 % | 50.5 | 6.7 / 30.0 / 5.8 / 0.5 |
| P1' + (b) cadence 2 s (no C5) | 0 / 3 | none | 8.5 / 15.0 | 7.56 | 1.9K / 0.7K | 11.3K | 1.037 | 7.5 | 0.0 / 2.3 | 20 % | 51.3 | 7.5 / 30.0 / 5.7 / 0.7 |

#### Tables, M15, faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 1 | 55 (17/24) | 36.8 / 37.5 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % | 44.5 | 20.7 / 30.0 / 2.1 / 0.0 |
| P1' alone | 0 / 2 | 63 (14/24) | 35.4 / 57.6 | 2.57 | 17.2K / 12.9K | 82.2K | 1.026 | 1.4 | 0.0 / 22.5 | 20 % | 52.0 | 15.2 / 30.0 / 2.5 / 2.1 |
| P1' alone, lagged A, bias +15 % | 0 / 5 | 68 (15/24) | 31.2 / 37.6 | 1.58 | 17.8K / 15.5K | 82.2K | 1.031 | 1.4 | 0.0 / 24.3 | 20 % | 56.9 | 11.9 / 30.0 / 3.2 / 2.0 |
| P1' alone, lagged A, bias -15 % | 1 / 2 | 60 (17/24) | 36.5 / 70.3 | 5.81 | 16.4K / 14.2K | 82.2K | 1.030 | 1.4 | 0.0 / 20.2 | 22 % | 49.2 | 16.3 / 30.0 / 2.1 / 2.3 |
| C5 free | 1 / 2 | 54 (17/24) | 35.4 / 36.1 | 12.38 | 19.1K / 13.4K | 82.2K | 1.029 | 1.4 | 0.0 / 23.0 | 23 % | 43.5 | 10.2 / 30.0 / 4.7 / 2.3 |
| C5 costed (list sync, flags, skipped while a pass runs) | 1 / 1 | 57 (17/24) | 35.3 / 36.3 | 16.66 | 17.8K / 14.2K | 82.2K | 1.029 | 1.4 | 0.0 / 22.6 | 22 % | 46.0 | 14.9 / 30.0 / 4.1 / 2.1 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 1 / 1 | 57 (17/24) | 35.3 / 36.3 | 16.66 | 17.8K / 14.2K | 82.2K | 1.029 | 1.4 | 0.0 / 22.6 | 22 % | 46.0 | 14.9 / 30.0 / 4.1 / 2.1 |
| C5 costed, 20 % proposals fail | 1 / 0 | 57 (17/24) | 35.5 / 37.2 | 16.71 | 17.7K / 13.9K | 82.2K | 1.029 | 1.4 | 0.0 / 22.0 | 23 % | 46.0 | 15.6 / 30.0 / 4.0 / 2.1 |
| gap 1 s (collector pass gap) | 0 / 3 | 59 (15/24) | 36.5 / 66.3 | 6.55 | 15.3K / 15.3K | 82.2K | 1.028 | 1.4 | 0.0 / 23.8 | 19 % | 47.9 | 14.0 / 30.0 / 2.2 / 2.1 |
| gap (a) fast recheck-only pass | 1 / 1 | 55 (17/24) | 36.8 / 37.6 | 28.81 | 16.5K / 12.7K | 82.2K | 1.029 | 1.4 | 0.0 / 20.0 | 25 % | 44.5 | 20.7 / 30.0 / 2.1 / 2.1 |
| gap (b) cut cadence 1 s | 5 / 9 | 14 (24/24) | 33.0 / 35.3 | 5.46 | 24.2K / 17.8K | 82.2K | 1.035 | 1.4 | 0.1 / 30.2 | 21 % | 2.7 | 6.6 / 30.0 / 4.2 / 2.3 |
| gap (b) cut cadence 2 s | 2 / 6 | 55 (17/24) | 35.0 / 37.3 | 3.71 | 16.1K / 13.6K | 82.2K | 1.032 | 1.4 | 0.0 / 24.6 | 22 % | 44.2 | 8.7 / 30.0 / 3.1 / 2.4 |
| gap (b) cut cadence 5 s | 1 / 1 | 55 (17/24) | 36.8 / 57.6 | 28.05 | 16.5K / 12.7K | 82.2K | 1.030 | 1.4 | 0.0 / 19.8 | 24 % | 44.5 | 20.2 / 30.0 / 2.2 / 0.7 |
| P1' + C5 free (no gap) | 0 / 3 | 102 (15/24) | 32.7 / 36.2 | 1.02 | 17.5K / 17.5K | 82.2K | 1.028 | 1.4 | 0.0 / 28.8 | 20 % | 90.8 | 4.5 / 30.0 / 4.2 / 2.2 |
| P1' + gap 1 s (no C5) | 0 / 5 | 103 (16/24) | 33.2 / 37.1 | 1.02 | 15.3K / 18.0K | 82.2K | 1.032 | 1.4 | 0.0 / 26.9 | 22 % | 92.3 | 11.2 / 30.0 / 2.5 / 1.9 |
| P1' + C5 free + gap 1 s | 1 / 6 | 62 (16/24) | 29.6 / 35.3 | 1.02 | 18.9K / 19.6K | 82.2K | 1.031 | 1.4 | 0.0 / 31.2 | 21 % | 51.7 | 3.5 / 30.0 / 3.0 / 1.6 |
| P1' + C5 costed + gap 1 s | 1 / 6 | 62 (16/24) | 29.9 / 35.2 | 1.63 | 17.2K / 17.6K | 82.2K | 1.030 | 1.4 | 0.0 / 31.4 | 21 % | 51.5 | 6.2 / 30.0 / 2.8 / 1.7 |
| P1' + C5 free + (a) fast recheck | 0 / 1 | 100 (14/24) | 34.9 / 36.6 | 1.02 | 17.5K / 17.5K | 82.2K | 1.015 | 1.4 | 0.0 / 20.1 | 18 % | 89.3 | 5.9 / 30.0 / 3.0 / 2.6 |
| P1' + C5 free + (b) cadence 2 s | 4 / 10 | 15 (24/24) | 24.7 / 29.3 | 1.02 | 24.2K / 18.9K | 82.2K | 1.028 | 1.4 | 0.1 / 35.1 | 23 % | 3.8 | 0.6 / 30.0 / 4.2 / 2.0 |
| P1' + (b) cadence 2 s (no C5) | 3 / 6 | 15 (24/24) | 27.5 / 32.5 | 2.26 | 24.0K / 16.3K | 82.2K | 1.030 | 1.4 | 0.1 / 30.8 | 25 % | 3.8 | 6.7 / 30.0 / 4.2 / 2.4 |

#### Tables, M15, stall regime

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 2 / 3 | 18 (24/24) | 35.3 / 36.1 | 14.63 | 18.4K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 29.8 | 20 % | 11.5 | 6.9 / 30.0 / 4.7 / 2.9 |
| P1' alone | 2 / 3 | 22 (24/24) | 32.2 / 36.8 | 14.41 | 18.2K / 15.0K | 81.2K | 1.028 | 2.0 | 0.0 / 30.7 | 24 % | 15.8 | 7.0 / 30.0 / 4.5 / 3.2 |
| P1' alone, lagged A, bias +15 % | 2 / 3 | 22 (24/24) | 30.3 / 36.7 | 14.13 | 18.2K / 14.3K | 80.6K | 1.027 | 2.0 | 0.0 / 30.1 | 24 % | 16.0 | 6.9 / 30.0 / 4.6 / 2.9 |
| P1' alone, lagged A, bias -15 % | 2 / 2 | 22 (24/24) | 32.2 / 36.8 | 14.41 | 18.2K / 15.0K | 81.6K | 1.033 | 2.0 | 0.0 / 30.7 | 25 % | 15.6 | 6.9 / 30.0 / 4.5 / 3.2 |
| C5 free | 2 / 4 | 18 (24/24) | 31.7 / 32.1 | 14.53 | 18.2K / 16.3K | 83.4K | 1.035 | 2.0 | 0.0 / 34.4 | 18 % | 11.7 | 0.2 / 30.0 / 5.3 / 2.9 |
| C5 costed (list sync, flags, skipped while a pass runs) | 3 / 4 | 16 (24/24) | 32.2 / 33.2 | 23.88 | 18.5K / 16.0K | 83.4K | 1.035 | 2.0 | 0.0 / 34.9 | 18 % | 9.7 | 1.8 / 30.0 / 4.7 / 3.1 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 3 / 5 | 16 (24/24) | 32.2 / 33.2 | 23.76 | 18.5K / 15.9K | 83.4K | 1.035 | 2.0 | 0.0 / 34.8 | 17 % | 9.7 | 1.7 / 30.0 / 4.7 / 3.1 |
| C5 costed, 20 % proposals fail | 2 / 4 | 16 (24/24) | 32.2 / 33.3 | 15.24 | 18.4K / 15.6K | 83.4K | 1.035 | 2.0 | 0.0 / 34.6 | 18 % | 9.7 | 2.1 / 30.0 / 4.6 / 3.6 |
| gap 1 s (collector pass gap) | 2 / 5 | 17 (24/24) | 32.3 / 35.9 | 21.57 | 18.2K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 34.5 | 21 % | 10.4 | 5.0 / 30.0 / 3.9 / 3.1 |
| gap (a) fast recheck-only pass | 3 / 4 | 18 (24/24) | 38.5 / 45.0 | 31.35 | 18.6K / 14.6K | 83.4K | 1.035 | 2.0 | 0.0 / 27.1 | 18 % | 11.5 | 6.5 / 30.0 / 1.7 / 4.8 |
| gap (b) cut cadence 1 s | 2 / 3 | 18 (24/24) | 35.5 / 36.6 | 14.60 | 18.5K / 15.2K | 83.4K | 1.035 | 2.0 | 0.0 / 30.0 | 20 % | 11.6 | 6.8 / 30.0 / 4.4 / 3.1 |
| gap (b) cut cadence 2 s | 3 / 3 | 18 (24/24) | 35.5 / 36.6 | 14.61 | 18.5K / 15.2K | 83.4K | 1.035 | 2.0 | 0.0 / 31.1 | 20 % | 11.6 | 6.9 / 30.0 / 4.5 / 2.9 |
| gap (b) cut cadence 5 s | 3 / 3 | 18 (24/24) | 35.5 / 36.0 | 23.94 | 18.4K / 15.1K | 83.4K | 1.035 | 2.0 | 0.0 / 28.7 | 18 % | 11.0 | 6.8 / 30.0 / 5.7 / 3.0 |
| P1' + C5 free (no gap) | 2 / 3 | 21 (24/24) | 27.5 / 32.1 | 5.00 | 18.3K / 15.9K | 81.2K | 1.027 | 2.0 | 0.0 / 35.6 | 22 % | 14.4 | 0.2 / 30.0 / 4.4 / 2.6 |
| P1' + gap 1 s (no C5) | 2 / 3 | 20 (24/24) | 31.9 / 36.9 | 1.02 | 18.1K / 14.7K | 81.2K | 1.027 | 2.0 | 0.0 / 35.5 | 32 % | 13.2 | 4.8 / 30.0 / 3.3 / 2.6 |
| P1' + C5 free + gap 1 s | 3 / 4 | 17 (24/24) | 29.0 / 32.4 | 1.02 | 18.9K / 18.2K | 81.2K | 1.023 | 2.0 | 0.0 / 39.4 | 29 % | 10.7 | 0.2 / 30.0 / 3.2 / 2.4 |
| P1' + C5 costed + gap 1 s | 3 / 4 | 17 (24/24) | 30.8 / 35.1 | 1.02 | 18.3K / 18.1K | 81.2K | 1.023 | 2.0 | 0.0 / 36.8 | 28 % | 10.9 | 1.3 / 30.0 / 3.3 / 2.6 |
| P1' + C5 free + (a) fast recheck | 2 / 3 | 21 (24/24) | 32.2 / 34.1 | 24.42 | 18.7K / 15.2K | 81.2K | 1.035 | 2.0 | 0.0 / 34.1 | 20 % | 14.4 | 0.3 / 30.0 / 1.4 / 4.6 |
| P1' + C5 free + (b) cadence 2 s | 3 / 3 | 20 (24/24) | 27.2 / 31.8 | 3.21 | 18.2K / 16.0K | 81.2K | 1.024 | 2.0 | 0.0 / 35.6 | 23 % | 13.7 | 0.3 / 30.0 / 4.1 / 2.7 |
| P1' + (b) cadence 2 s (no C5) | 2 / 2 | 22 (24/24) | 32.5 / 36.9 | 14.54 | 18.3K / 14.8K | 81.2K | 1.028 | 2.0 | 0.0 / 30.7 | 24 % | 15.9 | 7.0 / 30.0 / 4.3 / 3.1 |
Exception in WebSocket on_incoming_frame_payload callback
Traceback (most recent call last):
  File "awscrt/websocket.py", line 501, in _on_incoming_frame_payload
  File "awscli/customizations/ec2instanceconnect/websocket.py", line 299, in _on_incoming_frame_payload_data
  File "awscli/customizations/ec2instanceconnect/websocket.py", line 83, in write
BrokenPipeError: [Errno 32] Broken pipe

aws: [ERROR]: AWS_ERROR_HTTP_CALLBACK_FAILURE: A callback has reported failure.
Exception ignored while flushing sys.stdout:
BrokenPipeError: [Errno 32] Broken pipe

#### Tables, RAM 16K (not evidence), faithful

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 0 / 2 | 48 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.7K | 83.6K | 1.010 | 1.4 | 9.8 / 32.2 | 33 % | 93.3 | 24.3 / 30.0 / 5.5 / 4.9 |
| P1' alone | 0 / 0 | none | 37.3 / 42.6 | 0.40 | 34.9K / 43.4K | 83.6K | 0.999 | 1.4 | 9.6 / 31.6 | 31 % | 101.6 | 28.9 / 30.0 / 6.0 / 4.9 |
| P1' alone, lagged A, bias +15 % | 0 / 0 | none | 14.8 / 28.3 | 0.40 | 34.9K / 43.4K | 83.6K | 0.993 | 1.4 | 10.2 / 29.3 | 32 % | 101.3 | 31.5 / 30.0 / 5.1 / 5.9 |
| P1' alone, lagged A, bias -15 % | 0 / 1 | 54 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.1K | 83.6K | 1.008 | 1.4 | 10.7 / 30.4 | 32 % | 101.8 | 26.3 / 30.0 / 5.7 / 5.0 |
| C5 free | 1 / 0 | 45 (10/16) | 0.9 / 2.4 | 0.84 | 60.5K / 46.7K | 83.6K | 1.004 | 1.4 | 16.8 / 37.5 | 35 % | 16.1 | 1.9 / 30.0 / 5.3 / 3.3 |
| C5 costed (list sync, flags, skipped while a pass runs) | 0 / 2 | 50 (4/16) | 13.2 / 21.2 | 0.40 | 52.8K / 43.7K | 83.6K | 1.008 | 1.4 | 12.1 / 36.0 | 32 % | 69.1 | 7.2 / 30.0 / 5.5 / 4.9 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 0 / 2 | 50 (4/16) | 13.4 / 21.5 | 0.40 | 52.8K / 43.9K | 83.6K | 1.009 | 1.4 | 12.2 / 36.6 | 32 % | 69.6 | 7.5 / 30.0 / 5.6 / 4.9 |
| C5 costed, 20 % proposals fail | 0 / 3 | 50 (3/16) | 14.3 / 21.9 | 0.40 | 52.3K / 42.6K | 83.6K | 1.009 | 1.4 | 12.1 / 36.4 | 31 % | 71.5 | 8.3 / 30.0 / 5.4 / 5.3 |
| gap 1 s (collector pass gap) | 0 / 3 | 47 (3/16) | 42.3 / 42.6 | 0.40 | 34.9K / 49.6K | 83.6K | 1.008 | 1.4 | 8.8 / 34.7 | 33 % | 88.1 | 21.1 / 30.0 / 3.7 / 4.5 |
| gap (a) fast recheck-only pass | 0 / 2 | 59 (2/16) | 42.3 / 42.7 | 0.40 | 34.9K / 45.6K | 83.6K | 1.010 | 1.4 | 13.8 / 29.1 | 34 % | 93.3 | 23.0 / 30.0 / 3.7 / 5.0 |
| gap (b) cut cadence 1 s | 1 / 1 | 54 (2/16) | 32.6 / 32.9 | 0.61 | 34.0K / 37.8K | 83.6K | 1.017 | 1.4 | 9.6 / 27.1 | 29 % | 36.6 | 20.5 / 30.6 / 4.7 / 5.0 |
| gap (b) cut cadence 2 s | 1 / 0 | 52 (3/16) | 30.0 / 30.0 | 0.72 | 34.6K / 51.8K | 83.6K | 1.013 | 1.4 | 10.9 / 33.0 | 32 % | 36.6 | 17.0 / 30.0 / 6.6 / 6.3 |
| gap (b) cut cadence 5 s | 1 / 2 | 47 (6/16) | 30.0 / 32.4 | 0.78 | 34.8K / 50.7K | 83.6K | 1.008 | 1.4 | 11.8 / 26.6 | 32 % | 36.6 | 18.1 / 30.0 / 6.1 / 6.2 |
| P1' + C5 free (no gap) | 0 / 0 | none | 0.9 / 1.0 | 0.75 | 59.9K / 45.5K | 83.6K | 0.998 | 1.4 | 17.7 / 36.2 | 35 % | - | 2.0 / 30.0 / 5.9 / 3.1 |
| P1' + gap 1 s (no C5) | 0 / 0 | none | 37.2 / 42.6 | 0.40 | 34.9K / 43.8K | 83.6K | 0.996 | 1.4 | 9.0 / 34.0 | 32 % | 92.1 | 27.2 / 30.0 / 3.4 / 4.9 |
| P1' + C5 free + gap 1 s | 0 / 0 | none | 0.9 / 1.0 | 0.55 | 59.8K / 54.0K | 83.6K | 0.994 | 1.4 | 11.5 / 47.2 | 40 % | - | 1.5 / 30.0 / 4.0 / 2.8 |
| P1' + C5 costed + gap 1 s | 0 / 0 | 57 (1/16) | 9.9 / 12.6 | 0.41 | 52.0K / 45.4K | 83.6K | 0.994 | 1.4 | 10.1 / 38.2 | 37 % | 74.0 | 8.0 / 30.0 / 4.0 / 5.0 |
| P1' + C5 free + (a) fast recheck | 0 / 0 | none | 0.5 / 1.0 | 0.40 | 54.1K / 50.6K | 83.6K | 0.989 | 1.4 | 33.4 / 21.2 | 34 % | - | 3.1 / 30.0 / 3.6 / 2.6 |
| P1' + C5 free + (b) cadence 2 s | 0 / 0 | none | 0.6 / 0.9 | 0.51 | 59.4K / 43.8K | 83.6K | 0.976 | 1.4 | 17.8 / 36.3 | 36 % | - | 1.6 / 30.0 / 5.6 / 3.3 |
| P1' + (b) cadence 2 s (no C5) | 1 / 0 | none | 22.7 / 23.6 | 0.56 | 33.8K / 49.4K | 83.6K | 1.008 | 1.4 | 10.7 / 30.5 | 35 % | 44.1 | 18.1 / 30.0 / 5.9 / 6.3 |

#### Tables, RAM 16K (not evidence), stall regime

| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base (main) | 1 / 1 | 39 (16/16) | 13.0 / 15.1 | 1.02 | 46.1K / 25.8K | 110.0K | 1.007 | 2.0 | 4.4 / 47.3 | 26 % | 23.3 | 8.7 / 30.0 / 6.2 / 6.1 |
| P1' alone | 0 / 0 | 57 (1/16) | 1.0 / 1.0 | 1.02 | 44.1K / 24.0K | 110.0K | 0.991 | 2.0 | 4.2 / 44.7 | 28 % | 40.9 | 8.8 / 30.0 / 6.7 / 6.4 |
| P1' alone, lagged A, bias +15 % | 0 / 0 | none | 1.0 / 1.0 | 1.02 | 43.9K / 23.4K | 110.0K | 0.986 | 2.0 | 4.1 / 44.9 | 28 % | 49.2 | 9.2 / 30.0 / 6.7 / 6.2 |
| P1' alone, lagged A, bias -15 % | 0 / 0 | 51 (1/16) | 1.0 / 1.0 | 1.02 | 44.5K / 27.0K | 110.0K | 0.996 | 2.0 | 4.3 / 45.1 | 28 % | 34.7 | 8.9 / 30.0 / 6.7 / 6.3 |
| C5 free | 0 / 0 | 38 (7/16) | 1.0 / 1.1 | 1.02 | 45.6K / 40.2K | 110.0K | 1.000 | 2.0 | 19.4 / 44.4 | 22 % | 21.7 | 0.3 / 30.0 / 6.3 / 5.2 |
| C5 costed (list sync, flags, skipped while a pass runs) | 0 / 1 | 38 (7/16) | 2.1 / 12.2 | 1.02 | 47.5K / 38.6K | 110.0K | 1.003 | 1.9 | 6.3 / 49.2 | 20 % | 27.0 | 3.0 / 30.0 / 5.8 / 6.2 |
| C5 costed, disk (5.5 ms flags, 3 ms sync) | 0 / 0 | 38 (7/16) | 2.1 / 12.8 | 1.02 | 47.4K / 38.9K | 110.0K | 1.003 | 1.9 | 5.0 / 51.3 | 20 % | 22.6 | 3.0 / 30.0 / 5.7 / 5.9 |
| C5 costed, 20 % proposals fail | 0 / 1 | 38 (6/16) | 2.1 / 13.3 | 1.02 | 47.3K / 38.9K | 110.0K | 1.001 | 1.9 | 4.4 / 51.2 | 21 % | 73.8 | 3.0 / 30.0 / 5.7 / 5.7 |
| gap 1 s (collector pass gap) | 2 / 2 | 37 (16/16) | 11.3 / 14.6 | 1.02 | 46.1K / 36.8K | 110.0K | 1.003 | 2.0 | 2.4 / 52.2 | 34 % | 20.8 | 6.0 / 30.0 / 4.1 / 5.4 |
| gap (a) fast recheck-only pass | 1 / 1 | 39 (16/16) | 7.1 / 15.1 | 1.02 | 46.1K / 30.6K | 110.0K | 1.009 | 2.0 | 14.8 / 37.5 | 27 % | 23.3 | 7.1 / 30.0 / 3.1 / 5.4 |
| gap (b) cut cadence 1 s | 1 / 1 | 39 (16/16) | 13.7 / 15.1 | 1.02 | 46.0K / 25.8K | 110.0K | 1.006 | 2.0 | 4.5 / 47.4 | 26 % | 23.5 | 8.8 / 30.0 / 6.1 / 5.9 |
| gap (b) cut cadence 2 s | 1 / 1 | 39 (16/16) | 13.3 / 14.8 | 1.02 | 46.2K / 25.7K | 110.0K | 1.007 | 2.0 | 4.0 / 47.4 | 26 % | 23.2 | 8.7 / 30.0 / 6.1 / 5.8 |
| gap (b) cut cadence 5 s | 2 / 1 | 39 (16/16) | 13.1 / 15.1 | 1.02 | 46.3K / 25.1K | 110.0K | 1.008 | 2.0 | 4.1 / 47.4 | 26 % | 23.4 | 8.8 / 30.0 / 6.0 / 5.9 |
| P1' + C5 free (no gap) | 0 / 0 | none | 1.0 / 1.0 | 0.89 | 44.5K / 38.9K | 110.0K | 0.982 | 2.0 | 19.0 / 43.0 | 22 % | - | 0.2 / 30.0 / 6.7 / 4.7 |
| P1' + gap 1 s (no C5) | 0 / 0 | 54 (1/16) | 1.0 / 1.0 | 1.01 | 44.8K / 35.8K | 110.0K | 0.989 | 2.0 | 2.6 / 50.7 | 35 % | 37.7 | 6.3 / 30.0 / 3.9 / 5.6 |
| P1' + C5 free + gap 1 s | 0 / 0 | none | 0.8 / 1.0 | 0.71 | 44.4K / 40.4K | 110.0K | 0.980 | 2.0 | 16.0 / 50.4 | 30 % | - | 0.3 / 30.0 / 4.6 / 4.4 |
| P1' + C5 costed + gap 1 s | 0 / 0 | none | 1.8 / 2.1 | 0.74 | 45.7K / 39.6K | 110.0K | 0.983 | 1.9 | 3.6 / 60.6 | 28 % | - | 2.3 / 30.0 / 3.9 / 4.9 |
| P1' + C5 free + (a) fast recheck | 0 / 0 | none | 1.0 / 1.0 | 0.74 | 44.5K / 39.5K | 110.0K | 0.982 | 2.0 | 27.2 / 36.6 | 24 % | - | 0.3 / 30.0 / 3.9 / 4.2 |
| P1' + C5 free + (b) cadence 2 s | 0 / 0 | none | 1.0 / 1.0 | 0.89 | 44.5K / 39.0K | 110.0K | 0.984 | 2.0 | 18.6 / 43.1 | 23 % | - | 0.3 / 30.0 / 6.6 / 4.9 |
| P1' + (b) cadence 2 s (no C5) | 0 / 0 | 55 (1/16) | 1.0 / 1.0 | 1.02 | 44.2K / 23.7K | 110.0K | 0.992 | 2.0 | 4.1 / 45.2 | 28 % | 39.5 | 9.1 / 30.0 / 6.4 / 6.4 |

## Gates and deviations

* `tools/ci.sh` on the remote: `ci: all green` (fmt, clippy `-D warnings`, `cargo test --workspace` including 25 simulator tests, the crashpoint builds, ownership
  `[spacesim]`). The simulator's own tests run in 1.4 s. No kill loops: no product behaviour changed.
* `tools/ownership.toml` has a `[spacesim]` entry with exactly the changed paths (`Cargo.lock` in a separate `deps:` commit).
* Nothing in `lunix-gc` or `lunix-index` was touched. The only product change is the visibility change in `lunix-engine/src/{cut,lib}.rs` above.
* The simulator's G1 cost constants (`condemn_ms` 5.5, `delete_ms` 13) are the gcrate figures; a pass log of the p1/m16 RAM run shows the real phase times
  (condemn about 4 ms and delete about 5 ms per segment unit, mark 12 ms at that size), to be replaced by p1/gcbatch's measurements.
* The last coordinator message (remote CPUs 0-7 only) arrived while the full CI run (pinned 0-15 when it started) was finishing; nothing was run on 8-15 afterwards.
