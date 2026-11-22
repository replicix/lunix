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

## Calibration (policy `main`; 8 seeds, the phase of the collector's idle cadence drawn from the seed; sim = median (min-max))

| cell | measured | simulated | verdict |
|---|---|---|---|
| RAM 64 KiB w4k, reclaim_rate at first band entry | 0 in 12 of 12 volumes (leadzone report) | 5 (0-6) MB/s | match |
| store bytes per admitted byte before the band | about 2.8 (t6t8 rep 1: used +10.2 GB for 3.5 GB admitted in the first 11 s; later 1.7-2.4) | 2.7 (2.7) | match |
| sustained IOPS | 24-25 K (leadzone), 21.5-32.7 K (t6t8) | 26.1 K (18.6-33.8) | match (within 10 %) |
| stops per cell | 1-7 (1-4 in t6t8) | 1 (0-3) | low end only |
| first stop | 12-15 s into the overwrite (brief); about 20-27 s by the t6t8 series | 23 s (seen in 1 of 8 first cells) | not reproduced as a gate `stop` (see below) |
| max completion per cell | about 18-33 s | 7.1 s (0.7-40.5); first cells 8.2 (0.7-32.3) | the long stall exists but is a ring/commit stall in most seeds, not a gate stop |
| first condemn / first delete | the first condemn is the urgent pass at the stop; the first delete a floor grace later (stop 25-30 s) | condemn 13.6 s (7.7-26.7), delete 55.9 s (40.0-58.7); delete minus condemn at least 30 s in every run | consistent |
| RAM 16 KiB w4k | stops 4 in 9 cells, max 5.8 s (1.5-24.5), 43.6 K IOPS | stops 1 (0-3), max 6.6 s (0.4-38.8), 42.9 K IOPS | IOPS and max match; stops fewer |
| network volume 64 / 16 KiB | 0 stops, 0 delayed, 3.3 K / 8.7 K IOPS, max 20 / 5 s | 0 stops, 0 delayed, 1.0 K / 1.7 K IOPS, max 6.2 / 2.6 s | gate not involved: match; throughput 3-5x low (the shared-bandwidth model is crude) |
| M15 8 GiB cap, 16 KiB | stops 0-2 (base), longest completion 14-38 s, reclaim 0 at first entry | stops 3 (0-7), longest 36.9 s, reclaim 0 | longest and reclaim match; stops high; the cell's workload shape is assumed |

Tolerances the regression test enforces (`tests/calibration.rs`, 4 seeds, no wall clock): reclaim at band entry under 50 MB/s; amplification 2.2-3.6;
grace under pressure exactly 30 s and delete at least 30 s after condemn; at least one stop and a longest completion of 15-70 s over the runs; median IOPS
15-40 K; network volume: no band entry, 0 stops, 0 delayed; same seed gives the same trace.

**Null results for `gate-gc` and `leadzone` (calibration data, as measured):** RAM 64 KiB: stops per cell 1 (0-3) and 1 (0-3) against 1 (0-3) for `main`,
longest completion 7.9 and 7.1 s against 7.1 s, IOPS 26.4 K and 26.1 K against 26.1 K, reclaim at entry 5 MB/s for all. M15: stops 5 and 3 against 3. The
mechanism the simulation shows is the one the maintainer diagnosed: the first condemn comes from the first pass that finds wholly dead segments (idle cadence,
0-30 s into the overwrite) or the urgent pass, and nothing becomes deletable until a grace later; `leadzone` changes the grace of objects that were not yet
condemned, `gate-gc` sizes a collector that has nothing deletable to delete.

### What does not reproduce, and why

* **The gate `stop` of 25-30 s.** In the measured cell the gate is in `stop` (`used` above the limit, ingest 0) for a grace. In the simulation the same
  stall appears (ring-parked writes while the cut cannot reserve its bound; `commit failed for space`), but in most seeds `used` stays under the limit and the
  gate stays in `slowdown`, so the stop counter and its episode durations are low. Measured: the limit itself fell from 19.0 to 16.5 GiB between two status
  samples (`used + fs_free - limit` rose from 1.2 to 2.3 GiB), i.e. the host file system held about 1.5 GiB that the ledger does not count at the stop (ring
  files rewritten with zeros on tmpfs, the model's `zero_alloc`, explains part; early in the cell the ledger over-counts instead). The model has the ring
  allocation but not the rest; what is missing is the file system's real accounting (open-but-unlinked segments, page-cache style holds). This is the main open
  calibration item.
* **Network-volume throughput** is 3-5x low: one shared bandwidth and a fixed cut cost; the seal `fdatasync` behaviour (T8 measurements) is not modelled.
* **M15** workload shape and device are assumed.

Constants fitted rather than measured: `engine_iops` 110 K and `ring_park` 384 MiB (so that the ingest before the band is about 300 MB/s and the store grows
2.7-2.9x the admitted bytes); `cut_fixed_ms`, `store_bw` 1.9 GB/s (from the cuts in the t6t8 status: 345 MiB journal in 528 ms), `gc_bw`. Nothing else was tuned,
and the unreproduced items above were left as they are.

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
