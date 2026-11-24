# p1-recheck16k: the 16K cell of the fast-recheck A/B, re-run on fixed main

Branch `p1/recheck16k` from `origin/main` b6af157 (merge of `p1/footerfix`: the compactor's footer cache takes `footer_cache_bytes`).
Data and analysis: `bench/phase1/perf/results/t6recheck16k/` (`tables.md`, `summary.json`, one directory per block). Analysis script
`bench/phase1/perf/t6recheck.py`, driver `tests/perf/t6recheck.sh`. Everything ran on the remote test host (kernel 7.0.0, 16 CPUs, nothing
else running); no local build or run. Why: the 16K figures of `p1-fastrecheck-t6.md` were measured on the 8-byte footer-cache bug (about 6x
slower compaction per unit, `bisect-16k-compaction.md`). Both arms have `throttle_pace_store_bytes = true`; only `gc_fast_recheck` differs
(OFF false, ON true, delay 1 s).

## Verdict

**The fix holds and the 16K gates now pass.** Compaction is back at 15 us per unit, a regular pass takes 1.5-1.6 s, and the Recheck lag at 16K
falls from 3.6 s (FAIL) to 1.5 s (PASS, target 1.9 s). 64K w4k repeats the earlier PASS (1.15 s). The GC share is 14.0 % at 16K OFF
(expected 13-16 %) and 19.7 % ON: 5.7 points more than OFF, all of it extra marks, compaction unchanged.

## Setup (as `p1-fastrecheck-t6.md`, with the deviations below)

Release build of `lunixd`, `lunix`, `lunix-ublk-testd` from this tree (one tree, one target directory: no mtime trap). `tests/perf/partA.sh`
and the harness of `p1/fastrecheck-t6` e46f92d, brought over unchanged except `tests/perf/t6recheck.sh` and `t6recheck.py`. 4 queues, daemon on
CPUs 0-7, fio on 8-15, a dedicated 20 GiB tmpfs for `LUNIX_SPIKE_DIR`, `WS_GIB=2`, `VOL_GIB=16`, 1 GiB journal, `destage_workers = 8`, w4k 60 s
(+ 5 s ramp), seqw 30 s, 5 repetitions per cell, `log_level = debug`, `SKIP_READ_CELLS=1 VERIFY_READS=0 SAMPLE=1 STATUS_PROBE=1 NULLBLK=0 PERF=0`.
A fresh daemon and volume per block, `fsck --deep` at the end of each.

| block | arm | chunk | cells | start load 1 / 5 / 15 min |
|---|---|---|---|---|
| b1 | OFF | 16K | w4k | 0.94 / 0.85 / 0.40 |
| b2 | ON | 16K | w4k | 0.00 / 1.99 / 28.9 |
| b3 | ON | 16K | w4k | 0.00 / 1.96 / 32.5 |
| b4 | OFF | 16K | w4k | 0.00 / 1.96 / 33.0 |
| b5 | OFF | 64K | w4k, seqw | 0.01 / 1.97 / 33.3 |
| b6 | ON | 64K | w4k, seqw | 0.00 / 1.99 / 33.2 |

16K: 2 blocks per arm, ABBA; 64K: 1 block per arm. So 16K has rep 1 n = 2 and reps 2-5 n = 8 per arm; 64K has rep 1 n = 1 and reps 2-5 n = 4.
Rep 1 is reported apart from reps 2-5. Wall time of the run 2 h 49 min (04:13 to 07:02Z).

## Deviations from the brief and from the earlier run (read these first)

1. **Milestone id** `LUNIX_M=fastrecheck` with `LUNIX_HOST_SRC/TARGET` on the `recheck16k` trees (as the earlier run); `remote.sh` created
   `<base>/fastrecheck/scratch`, which is removed. No `[recheck16k]` entry in `tools/ownership.toml`; `check-ownership.sh` would not know the paths.
2. **`log_level = debug`**, as the earlier run, both arms: `gc pass done` lines of passes that changed nothing are DEBUG.
3. **Load rule.** The driver now waits before every block for the 1-minute load below 1 and the 5-minute load below 2 (the earlier driver did it
   before the first block only). All six blocks started quiet by the harness rule, b2-b6 only just (5-minute load 1.96-1.99). The wait was about
   25 minutes after every block (the daemon's blocked threads and fio count in the load average), so the blocks are 31 minutes apart.
4. **Detached run.** The ssh session through the EICE gateway has a time limit, so the driver ran through `tools/remote.sh root` with a wrapper that
   starts it under `setsid`. The root session did drop at the end ("Connection exceeded session timeout") after all blocks had finished
   (driver rc 0); remote.sh's own leftovers check after the run therefore did not run. I ran `remote.sh leftovers` myself in the cleanup (clean).
5. **Script fixes** (all in `t6recheck.py`, the defects of `p1-fastrecheck-t6-check.md`):
   - The repetition window now starts at fio's own `job_start` (ms), not at "end minus elapsed". The old rule opened 1.5 s early and pulled the fill's
     cuts into rep 1. The window is `job_start` to the end of the fio json + 0.5 s. Numbers of this report are therefore not recomputed for the old run.
   - Urgent passes are no longer zero: their `phases=` is empty, so their wall time is counted as collector time (own column "urgent"). Every pass
     is clipped to the window by its interval (line time minus `wall_ms`, to line time) instead of counted whole.
   - Cut time is normalised by the cut's journal bytes: pooled (`last_cut_ms`, `last_cut_bytes`) pairs of the 5 s samples (the same cut seen twice
     in a row counts once), least-squares ms = a + b x MiB, and the median ms per MiB.
   - **Onset.** The collector writes no pressure line of its own (`set_pressure` only stores the flag), so the onset is still the engine's
     `write throttle on` line, which is coalesced: it is written only when 5 s have passed since the last throttle line and can be late. The table
     therefore shows "all onsets" and "fresh onsets" (the line's headroom still at least 0.9 x soft, i.e. written at the transition). Errors of up to
     5 s per rep remain in "all"; "fresh" is the cleaner figure but has fewer reps.
   - New: the lag split (waited / not waited behind a regular pass), the regular pass wall, compaction us/unit, and the GC share split into mark /
     compact / delete / urgent / other, in the "Headline" section of `tables.md`. "Waited" means the mean origin of a Recheck line's lag (line time minus
     `lag_mean_ms`) fell inside the interval of a regular pass (any pass that is not `kind=Recheck`, urgent ones included).
6. **No zipf block** in this run (not asked); the gate 5 table (skewed cell) is removed from the script output. Gates 5 and 7 of the old report are not
   re-judged here beyond the abandoned-pass and urgent-stop counts.
7. Passes without a pass line (an aborted pass) are invisible to the script, as before. `gc pass abandoned` and `gc urgent pass stopped early`: 0
   in all six blocks.
8. `hostinfo*` files are not part of the results; the remote scratch path in the logs and status files is rewritten to `<spike-dir>` (the host
   address, name, user and instance details appear nowhere).

## Gate table

IOPS: median / lower decile / worst. Latencies: median / p90 / worst. n = reps.

| # | item | 16K w4k (OFF n = 2 + 8, ON n = 2 + 8) | 64K w4k (OFF n = 1 + 4, ON n = 1 + 4) | verdict |
|---|---|---|---|---|
| 1 | Recheck lag: median over `kind=Recheck` lines (`how=urgent` out) of `lag_mean_ms`, target 1.5 s + median mark | ON reps 2-5: **1,528 ms** (75 lines) against 1,894 (mark 394 ms); rep 1: 1,544 (4 lines) against 1,869; all 79 lines 1,528, excluding the first line after each onset 1,536 (38 lines); median `lag_max` 1,580, worst 3,199. OFF (regular passes, no Recheck lines): 4,744 ms (reps 2-5), 5,415 (rep 1) | ON reps 2-5: **1,150 ms** (46 lines) against 1,584 (mark 82); rep 1: 1,512 (2 lines) against 1,582; all 68 lines 1,134; seqw 288 (reps 2-5) and 1,130 (rep 1). OFF regular passes 4,131 (reps 2-5) | **PASS at 16K** (was FAIL at 3,594 ms) and **PASS at 64K** (was 1,138 ms) |
| 1a | lag split | ON reps 2-5: waited behind a regular pass 22 of 75 lines (29 %, was 79 %), median **2,078 ms** (284 ms above the target); not waited 53 lines, median **1,492 ms**. Rep 1: 0 of 4 waited | ON reps 2-5: waited 2 of 46, not waited 44 at 1,155 ms | the unblocked lines meet the target with 400 ms to spare; the 16K lines that waited do not, and are now a minority |
| 2 | Regular pass wall (passes that ran compaction); compaction us/unit | median **1.55 s** ON, **1.58 s** OFF (p90 3.1 / 2.8, worst 3.8 / 4.6); **15 us per unit in both arms**, rep 1 the same (15) | median 0.31 s ON, 0.75 s OFF; 28 us ON, 27 us OFF per unit (rep 1: 28 / 27) | **PASS**: the 1.5-2 s expectation holds (was 6.4 / 6.6 s and 102 / 110 us) |
| 2a | GC share of the coordinator (reps 2-5, mean over reps): total (mark / compact / delete / urgent / other) | OFF **14.0 %** (5.7 / 7.1 / 1.2 / 0 / 0); ON **19.7 %** (11.7 / 6.7 / 1.3 / 0 / 0), of which Recheck passes 7.3 %. Rep 1: OFF 9.9, ON 11.5 | OFF 5.5 % (1.5 / 2.7 / 1.3 / 0 / 0); ON 6.7 % (3.1 / 2.3 / 1.3 / 0 / 0). seqw: OFF 2.5, ON 3.2. Rep 1: OFF 1.8 (0.6 / 0.4 / 0.0 / 0.8 urgent), ON 3.2 | **PASS** (target 30 %; was 33.8 / 37.0 % at 16K). OFF at 16K is inside the expected 13-16 %; ON is 5.7 points higher, all of it mark time (one mark of about 400 ms per Recheck pass); compaction does not shrink (7.1 to 6.7 %), so no starvation |
| 3 | Pressure onset to first delete (s, median / worst) | reps 2-5: OFF all onsets **7.5** / 7.6 (fresh 7.5 / 7.6, n = 6), ON **2.2** / 2.6 (fresh 2.2 / 2.3, n = 6). Rep 1: OFF 25.6 / 25.7, ON 21.4 / 21.8 | reps 2-5: OFF 5.5 / 6.6 (fresh n = 1: 0.9), ON **0.6** / 2.7 (fresh 1.5 / 2.7, n = 2). Rep 1: OFF 35.5, ON 37.2 | reported. The first Recheck line comes 2.2 s (16K) and 1.8 s (64K) after the onset line; the onset lines are coalesced, so read the 64K "all" medians with up to 5 s of error each. Rep 1 at 64K is unchanged (grace floor) |
| 4 | Stops (reps with a stop), longest completion, IOPS, p99.9, `delay_capped` | reps 2-5: stops 0 / 0; longest completion OFF 2.1 / 2.7 / 2.7 s, ON 2.5 / 3.7 / 4.2 s; IOPS OFF 42,807 / 40,383 / 40,337, ON 46,122 / 44,696 / 44,226; p99.9 OFF 0.76 / 0.80 / 0.81 s, ON 0.80 / 0.82 / 0.82 s; capped max 0 / 0. Rep 1: stops 0 / 0, longest OFF 11.9 (12.7), ON 9.6 (11.0) s, IOPS OFF 41,462, ON 44,425 | reps 2-5: stops OFF 1 (1 of 4), ON 0; longest OFF 10.7 / 11.5 / 11.6, ON 5.6 / 6.2 / 6.2 s; IOPS OFF 20,567 / 16,305 / 15,540, ON 23,746 / 22,097 / 22,065; p99.9 OFF 2.95 / 3.04 / 3.04, ON 2.13 / 2.32 / 2.33 s; capped 0 / 0. Rep 1: stops OFF 3 (1 of 1), ON 2 (1 of 1); longest 36.2 / 30.4 s; IOPS 19,971 / 28,617; capped 2 / 2. 64K seqw: no stops either arm, IOPS 692 / 677 | **PASS**, no regression at 16K (ON IOPS +7.7 %, longest completion tail slightly longer: worst 4.2 against 2.7 s; n is small); 64K better in reps 2-5, rep 1 not changed by the feature |
| 5 | `fsck --deep` clean, no EIO/ENOSPC | 4 of 4 clean; 0 EIO/ENOSPC lines; fio errors 0; `fulls`/`enospc` deltas 0 | 2 of 2 clean, same | **PASS** |

Cut time (not a gate here, reported because the earlier run saw +24-31 % on the mean): normalised by the cut size the cost per byte is the same.
Fit slope in ms per MiB, OFF to ON, reps 2-5: 16K 1.37 to 1.41 (+3 %), 64K w4k 1.75 to 1.37, 64K seqw 0.32 to 0.34. The means differ with the cut
size mix (16K 277 to 291 ms at 156 to 162 MiB, 64K w4k 178 to 255 ms at 69 to 117 MiB). The 64K OFF block had the smaller cuts (n = 43 and 44 samples
in one block each), so its slope has a wide error; the feature adds no visible per-byte cost.

Evidence objects: ON 16K 14.0 and 14.9 per minute over a block (largest 60 s window 19), 64K ON 19.3 (24); OFF 8.1 and 10.6 (9 and 11). History kept at the
largest ON rate: 54 and 43 minutes. Under the 60 per minute bound.

## Before and after (16K w4k, the same cell and harness)

Before = `p1-fastrecheck-t6.md` (e46f92d, faa5b40 plus the footer-cache bug), one block per arm; its figures are from the old script and window rule.
After = this run. The bisect rows (`bisect-16k-compaction.md`, 4 reps per point) are the other two references.

| figure (reps 2-5 unless noted) | before OFF | before ON | after OFF | after ON | bisect: s2 bug / s6 one-line fix |
|---|---|---|---|---|---|
| Recheck lag median ms (lines) | regular 6,008 | 3,594 (24, all reps) | regular 4,744 | **1,528** (75) | - |
| lag of lines that waited behind a regular pass | - | 4,402 (16 of 24 lines, 79 %) | - | 2,078 (22 of 75, 29 %) | - |
| lag of lines that did not wait | - | 1,611 (8 lines) | - | 1,492 (53 lines) | - |
| regular pass wall, median | 6.4 s | 6.6 s | 1.58 s | 1.55 s | 6.5 s / 1.5 s |
| compaction us per unit | 102 | 110 | 15 | 15 | 97 / 15 |
| GC share | 33.8 % | 37.0 % | 14.0 % | 19.7 % | 30.6 % / 13.6 % |
| of which compaction | 28.4 % | 29.3 % | 7.1 % | 6.7 % | 26.6 % / 6.9 % |
| of which mark | 3.1 % | 6.7 % | 5.7 % | 11.7 % | - |
| IOPS median | 37,596 | 36,901 | 42,807 | 46,122 | 39.1K / 43.4K |
| p99.9 s | 0.83 | 0.91 | 0.76 | 0.80 | - |
| onset to first delete, median | 14.3 s | 5.7 s | 7.5 s | 2.2 s | - |
| onset to first delete, rep 1 | 30.5 s | 22.3 s | 25.6 s | 21.4 s | - |
| stops | 0 | 0 | 0 | 0 | - |

At 64K w4k (reps 2-5): Recheck lag 1,138 (before) against 1,150 ms; regular pass wall 0.56-0.75 s against 0.31 s (ON) and 0.75 s (OFF); GC share
OFF 5.8 / ON 8.2 % against 5.5 / 6.7 %; stops OFF 5 / ON 1 (8 reps each) against 1 / 0 (4 reps each); onset to first delete 4.7 / 0.6 s against 5.5 / 0.6 s;
IOPS 21,634 / 23,665 against 20,567 / 23,746. Rep 1 at 64K: first delete 36.2 / 35.8 s before, 35.5 / 37.2 s now: the grace floor decides it, as found.

## Findings

* **The 16K lag gate failed because of the bug, not the feature.** With 1.5 s regular passes the recheck pass no longer waits 2.7 s behind one: 29 %
  of the ON lines waited (was 79 %). The waited lines still have a median 2.1 s, 0.3 s over the target, because a recheck pass starts only 3 x mark
  (1.2 s at 16K) after the regular pass ends. The median over lines meets the target, 1.5 s against 1.9 s.
* **GC share ON at 16K is 19.7 %, not 13-16 %.** The extra 5.7 points over OFF are marks: ON runs 79 Recheck passes against 81 regular ones in 10 reps
  and each marks for about 400 ms. Compaction and delete shares equal OFF's. 11.7 % mark time on the coordinator is the figure to watch if a later change
  makes recheck passes run continuously (the active C5 stage): it would roughly double.
* **Rep 1 is not helped at 64K** (first delete 35-37 s in both arms, longest completion 30-36 s, 2-3 stops). At 16K rep 1 first delete is 25.6 to 21.4 s,
  no stop in either arm.
* **No cost of the feature visible** in IOPS, p99.9, cut time per byte or compaction volume at either chunk size; ON is ahead on IOPS at both.
* Limits: 16K has 2 blocks per arm, 64K only one, and the 16K ON arm has a longer worst completion (4.2 s against 2.7 s) (ON blocks b2 and b3); with n = 8 reps
  this is not tested. The onset figures carry the coalescing error given in deviation 5.

## Reproduce

`tests/perf/t6recheck.sh` through `tools/remote.sh root` with `LUNIXD`, `LUNIX`, `TESTD`, `LUNIX_SPIKE_DIR` (a tmpfs) and `RESDIR`; then
`python3 bench/phase1/perf/t6recheck.py bench/phase1/perf/results/t6recheck16k --json .../summary.json > .../tables.md`. The `BLOCKS` default of the driver
is the six blocks above.

Remote clean-up: `lunix-p1/recheck16k` and the stale `lunix-p1/refs` removed, tmpfs unmounted, no daemon, fio or ublk device left (leftovers check clean).
