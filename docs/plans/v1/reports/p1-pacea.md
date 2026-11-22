# P1' A/B (T6): store-byte-aware space pacing on the real daemon

Branch `p1/pacea-t6` from `origin/main` 32f686f (P1' behind `throttle_pace_store_bytes`, default false, and `throttle_pace_store_margin_percent`, default 115). Data and
analysis: `bench/phase1/perf/results/t6pacea/` (`tables.md`, `summary.json`, one directory per run); the analysis script is `bench/phase1/perf/t6pacea.py`.

## P1' A/B (T6)

### Setup (as the T6 RAM run of p1/m16, "T6 re-run with T8 and the gate fix")

Release build of `lunixd` / `lunix` from the branch; `tests/perf/partA.sh` and the analysis scripts brought unchanged from `origin/p1/m16`; 4 queues, daemon on CPUs 0-7 (queue threads 0-3), fio on 8-15; a
dedicated 20 GiB tmpfs for `LUNIX_SPIKE_DIR` with `LUNIX_ALLOW_TMPFS_FOR_TESTS=1`; `WS_GIB=2`, `VOL_GIB=16`, 1 GiB journal, `destage_workers = 8`, `W_RUNTIME=60` (w4k), 30 s seqw, 5 repetitions per cell
and arm, `SKIP_READ_CELLS=1 VERIFY_READS=0 SAMPLE=1 STATUS_PROBE=1 NULLBLK=0 PERF=0`. A fresh daemon and a fresh volume per (arm, chunk size) block of five w4k and five seqw repetitions. ON =
`throttle_pace_store_bytes = true` with margin 115; OFF = `throttle_pace_store_bytes = false`. The whole 16-CPU host was used and was quiet at the start (load average 0.08, no daemon, no fio, no ublk device;
only idle system services). Wall time of the measurement: 1 h 41 min.

Order of the blocks: pass 1 OFF 64K, ON 64K, OFF 16K, ON 16K; pass 2 in reverse (ON 16K, OFF 16K, ON 64K, OFF 64K); then OFF 64K again (pass 3, see the deviations). So every arm and chunk size has
**10 repetitions in two blocks** (OFF 64K: pass 2 and pass 3).

### Deviations (read these first)

1. **`destage_workers` is not a daemon key on main.** The T6 setup (8 destage workers) needs it, and `partA.sh` sets it; on 32f686f the daemon refuses the config (`unknown key "destage_workers"`).
   The plumbing was taken over from p1/m16 as a separate commit (config field, bounds 1 to 64, one line in `daemon.rs` handing it to the stack's `destage_workers`; no other change). The
   P1' code is exactly main's. Without this commit main's default would be 4 workers under the 0-7 CPU set.
2. **The first 64K OFF block (pass 1) was lost** (my driver's copy-back directory was missing; the tmpfs was wiped by the next run before I noticed; only the summary lines in the script log remain:
   IOPS 32.1 / 23.5 / 29.8 / 24.5 / 21.4 K, seqw 572-722 IOPS). It was repeated as pass 3 at the end of the session (so OFF 64K is pass 2 + pass 3, not pass 1 + pass 2). The order is therefore not
   strictly interleaved for that arm.
3. No null_blk / journal references (`NULLBLK=0`), no raw baseline, no read cells: this is a daemon-against-daemon A/B.
4. The `hostinfo.md` files are not part of the results (host details).
5. The `store pace above the soft mark` and `store factor of the cut` figures come from the daemon log; the per-cut A error pairs `a_pred_milli` (the A_hat the cut was projected with, unbiased) with
   `a_real_milli` (what the cut measured). The OFF arm computes and logs them too (its `store_factor_milli` is the constant 2000, as stated in the brief).
6. Dwell (throttle on to the first stop) is read from the daemon log inside the cell's time window; it is approximate (a throttle that was already on when the cell started has no "on" line in its window).

### Results (ten repetitions per arm and chunk size; "med / p90 / worst")

Max completion = the largest per-second maximum of the fio completion-latency logs of the cell. The seqw cells are listed for completeness and are not part of the verdict.

### 64 KiB w4k

| arm | reps | stops (reps with a stop) | max completion s (med / p90 / worst) | IOPS (med / p90 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |
|---|---|---|---|---|---|---|---|---|
| OFF | 10 | 41 (10 of 10) | 21.5 / 37.4 / 41.4 | 24,917 / 31,772 / 19,580 | 1.93 / 4.05 / 6.21 | 49,592 | 564,916 | 4 |
| ON | 10 | 6 (3 of 10) | 7.1 / 31.9 / 37.3 | 19,603 / 23,347 / 16,127 | 2.82 / 4.23 / 8.66 | 42,138 | 435,616 | 4 |

### 16 KiB w4k

| arm | reps | stops (reps with a stop) | max completion s (med / p90 / worst) | IOPS (med / p90 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |
|---|---|---|---|---|---|---|---|---|
| OFF | 10 | 5 (2 of 10) | 5.7 / 14.6 / 16.8 | 43,494 / 45,736 / 37,110 | 0.83 / 1.08 / 2.06 | 42,521 | 1,185,422 | 3 |
| ON | 10 | 0 (0 of 10) | 4.2 / 11.5 / 13.1 | 41,264 / 42,801 / 37,468 | 0.82 / 1.56 / 1.74 | 35,498 | 831,923 | 0 |

### 64 KiB seqw

| arm | reps | stops (reps with a stop) | max completion s (med / p90 / worst) | IOPS (med / p90 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |
|---|---|---|---|---|---|---|---|---|
| OFF | 10 | 0 (0 of 10) | 3.2 / 4.6 / 4.7 | 692 / 735 / 566 | 3.09 / 4.34 / 4.40 | 19,266 | 5,475 | 0 |
| ON | 10 | 0 (0 of 10) | 3.8 / 4.8 / 5.1 | 686 / 737 / 615 | 3.62 / 4.62 / 4.80 | 18,226 | 5,496 | 0 |

### 16 KiB seqw

| arm | reps | stops (reps with a stop) | max completion s (med / p90 / worst) | IOPS (med / p90 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |
|---|---|---|---|---|---|---|---|---|
| OFF | 10 | 0 (0 of 10) | 1.8 / 2.0 / 2.1 | 590 / 637 / 544 | 1.74 / 2.02 / 2.04 | 8,808 | 3,004 | 0 |
| ON | 10 | 0 (0 of 10) | 1.9 / 2.2 / 2.6 | 577 / 629 / 556 | 1.90 / 2.06 / 2.57 | 8,245 | 2,643 | 0 |

### per repetition (w4k; seqw in the json)

| run | rep | max s | stops | IOPS | p99.9 s | throttled_ms | delayed_writes | capped | dwell s | above-soft events | above_soft_ms | store_delayed_ms | factor max (milli) | biased max | uncut max MiB | cuts |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| p1-off-16384 | 1 | 14.4 | 4 | 37110 | 2.06 | 33218 | 330108 | 0 | 19.3 | 0 | 0 | 0 | 2000 | 2300 | 435 | 65 |
| p1-off-16384 | 2 | 5.6 | 0 | 46256 | 0.97 | 29704 | 613935 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 449 | 56 |
| p1-off-16384 | 3 | 4.0 | 0 | 42542 | 0.79 | 42637 | 1191370 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 344 | 43 |
| p1-off-16384 | 4 | 6.2 | 0 | 42922 | 0.84 | 45153 | 1303721 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 449 | 51 |
| p1-off-16384 | 5 | 3.4 | 0 | 44067 | 0.76 | 42405 | 1224469 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 444 | 43 |
| p1-on-16384 | 1 | 11.3 | 0 | 40948 | 1.54 | 28024 | 246214 | 0 | - | 1 | 147 | 28667 | 1776 | 2042 | 286 | 68 |
| p1-on-16384 | 2 | 4.7 | 0 | 37468 | 0.87 | 42220 | 949680 | 0 | - | 3 | 1477 | 263931 | 1918 | 2205 | 307 | 46 |
| p1-on-16384 | 3 | 4.4 | 0 | 41250 | 0.83 | 35603 | 816046 | 0 | - | 3 | 1278 | 222612 | 2007 | 2308 | 411 | 48 |
| p1-on-16384 | 4 | 2.5 | 0 | 43360 | 0.81 | 35393 | 878379 | 0 | - | 3 | 1803 | 361535 | 1767 | 2032 | 249 | 49 |
| p1-on-16384 | 5 | 1.8 | 0 | 39668 | 0.78 | 41687 | 1120136 | 0 | - | 4 | 1986 | 343700 | 1566 | 1800 | 442 | 41 |
| p1-on-65536 | 1 | 37.3 | 4 | 18417 | 8.66 | 47150 | 167643 | 3 | 21.2 | 1 | 687 | 131573 | 3222 | 3705 | 302 | 44 |
| p1-on-65536 | 2 | 17.0 | 0 | 22739 | 3.74 | 39377 | 283250 | 0 | - | 3 | 7267 | 1497134 | 5795 | 6664 | 327 | 41 |
| p1-on-65536 | 3 | 6.0 | 0 | 16127 | 3.04 | 42008 | 471743 | 0 | - | 5 | 12992 | 3065389 | 6937 | 7977 | 353 | 30 |
| p1-on-65536 | 4 | 6.4 | 0 | 18073 | 2.77 | 47233 | 473296 | 0 | - | 6 | 10497 | 2407201 | 7223 | 8306 | 321 | 27 |
| p1-on-65536 | 5 | 9.7 | 0 | 21353 | 2.50 | 42476 | 319098 | 0 | - | 5 | 11020 | 2630305 | 5739 | 6599 | 340 | 35 |
| p2-off-16384 | 1 | 16.8 | 1 | 44280 | 0.86 | 26768 | 330581 | 3 | 13.9 | 0 | 0 | 0 | 2000 | 2300 | 461 | 63 |
| p2-off-16384 | 2 | 5.9 | 0 | 39987 | 0.82 | 44106 | 1179473 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 346 | 46 |
| p2-off-16384 | 3 | 6.3 | 0 | 45678 | 0.89 | 41062 | 1092044 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 438 | 51 |
| p2-off-16384 | 4 | 2.8 | 0 | 44876 | 0.81 | 43798 | 1355210 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 466 | 43 |
| p2-off-16384 | 5 | 1.5 | 0 | 41144 | 0.76 | 48294 | 1424391 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 457 | 42 |
| p2-off-65536 | 1 | 41.4 | 4 | 19580 | 3.81 | 46602 | 142400 | 3 | 5.4 | 0 | 0 | 0 | 2000 | 2300 | 432 | 44 |
| p2-off-65536 | 2 | 36.9 | 6 | 23309 | 3.51 | 46639 | 354467 | 4 | 11.1 | 0 | 0 | 0 | 2000 | 2300 | 429 | 29 |
| p2-off-65536 | 3 | 25.0 | 5 | 31834 | 1.95 | 44173 | 498723 | 2 | 8.5 | 0 | 0 | 0 | 2000 | 2300 | 429 | 41 |
| p2-off-65536 | 4 | 22.6 | 6 | 23456 | 6.21 | 53164 | 408510 | 4 | 26.5 | 0 | 0 | 0 | 2000 | 2300 | 360 | 26 |
| p2-off-65536 | 5 | 20.4 | 4 | 31765 | 1.90 | 46400 | 631110 | 3 | 15.3 | 0 | 0 | 0 | 2000 | 2300 | 429 | 42 |
| p2-on-16384 | 1 | 13.1 | 0 | 40059 | 1.74 | 27679 | 183220 | 0 | - | 2 | 342 | 49337 | 1976 | 2272 | 439 | 72 |
| p2-on-16384 | 2 | 5.1 | 0 | 42739 | 0.80 | 35730 | 847800 | 0 | - | 4 | 1000 | 179780 | 1884 | 2166 | 411 | 51 |
| p2-on-16384 | 3 | 3.5 | 0 | 41500 | 0.87 | 31536 | 761066 | 0 | - | 4 | 1933 | 371365 | 1873 | 2153 | 339 | 45 |
| p2-on-16384 | 4 | 4.0 | 0 | 42710 | 0.80 | 35164 | 777024 | 0 | - | 4 | 1449 | 236668 | 1903 | 2188 | 329 | 50 |
| p2-on-16384 | 5 | 3.4 | 0 | 41277 | 0.81 | 40148 | 983965 | 0 | - | 5 | 2289 | 410608 | 1939 | 2229 | 359 | 51 |
| p2-on-65536 | 1 | 31.3 | 1 | 28817 | 2.87 | 37761 | 152130 | 4 | 16.8 | 1 | 959 | 175977 | 3552 | 4084 | 270 | 55 |
| p2-on-65536 | 2 | 6.1 | 0 | 21197 | 2.40 | 41395 | 500507 | 0 | - | 6 | 15397 | 3659414 | 5753 | 6615 | 403 | 31 |
| p2-on-65536 | 3 | 5.2 | 0 | 19566 | 1.87 | 44904 | 503828 | 0 | - | 4 | 12796 | 2922717 | 5644 | 6490 | 284 | 33 |
| p2-on-65536 | 4 | 7.2 | 1 | 19640 | 2.26 | 42268 | 529153 | 0 | - | 4 | 13411 | 3154138 | 6566 | 7550 | 395 | 31 |
| p2-on-65536 | 5 | 7.1 | 0 | 17460 | 3.27 | 35610 | 399489 | 0 | - | 6 | 21450 | 4855625 | 6414 | 7376 | 385 | 32 |
| p3-off-65536 | 1 | 31.9 | 2 | 31222 | 1.30 | 36375 | 84094 | 3 | 5.4 | 0 | 0 | 0 | 2000 | 2300 | 424 | 54 |
| p3-off-65536 | 2 | 9.9 | 2 | 24154 | 1.74 | 57633 | 999859 | 0 | 16.2 | 0 | 0 | 0 | 2000 | 2300 | 379 | 33 |
| p3-off-65536 | 3 | 3.8 | 3 | 23876 | 1.18 | 59749 | 1103569 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 330 | 32 |
| p3-off-65536 | 4 | 11.1 | 6 | 27478 | 2.47 | 52544 | 798143 | 0 | 17.9 | 0 | 0 | 0 | 2000 | 2300 | 473 | 35 |
| p3-off-65536 | 5 | 4.8 | 3 | 25680 | 1.35 | 58823 | 1187559 | 0 | - | 0 | 0 | 0 | 2000 | 2300 | 375 | 32 |

### A_hat error per cut (real - pred, milli; pooled over the w4k cells of all runs of the arm and chunk)

| arm | chunk | cuts | pred median | real median | error p10 / p50 / p90 / p99 | under-predicted (real > pred) | under by more than 20 % |
|---|---|---|---|---|---|---|---|
| OFF | 64 | 368 | 2865.0 | 3133.5 | -1,624 / -30 / 4,155 / 4,939 | 182 (49 %) | 159 (43 %) |
| OFF | 16 | 503 | 1505 | 1537 | -436 / -13 / 471 / 736 | 229 (46 %) | 99 (20 %) |
| ON | 64 | 359 | 3533 | 4316 | -1,888 / 241 / 3,907 / 4,603 | 194 (54 %) | 159 (44 %) |
| ON | 16 | 521 | 1500 | 1554 | -375 / -7 / 517 / 668 | 251 (48 %) | 114 (22 %) |

### A_hat error, cuts of 128 MiB of journal or more (real - pred, milli)

| arm | chunk | cuts | pred median | real median | error p10 / p50 / p90 | relative error p10 / p50 / p90 (%) |
|---|---|---|---|---|---|---|
| OFF | 64 | 205 | 2913 | 2461 | -2,989 / -493 / 559 | -123 / -21 / 17 |
| OFF | 16 | 370 | 1509.0 | 1436.5 | -436 / -66 / 161 | -32 / -4 / 10 |
| ON | 64 | 173 | 3570 | 2987 | -2,598 / -502 / 559 | -97 / -31 / 14 |
| ON | 16 | 349 | 1511 | 1440 | -381 / -76 / 153 | -29 / -6 / 9 |

### per block of five repetitions (one daemon, one fresh volume)

| run | w4k reps with a stop | stops | max completion median s | worst s | IOPS median |
|---|---|---|---|---|---|
| p1-off-16384 | 1 of 5 | 4 | 5.6 | 14.4 | 42,922 |
| p1-on-16384 | 0 of 5 | 0 | 4.4 | 11.3 | 40,948 |
| p1-on-65536 | 1 of 5 | 4 | 9.7 | 37.3 | 18,417 |
| p2-off-16384 | 1 of 5 | 1 | 5.9 | 16.8 | 44,280 |
| p2-off-65536 | 5 of 5 | 25 | 25.0 | 41.4 | 23,456 |
| p2-on-16384 | 0 of 5 | 0 | 4.0 | 13.1 | 41,500 |
| p2-on-65536 | 2 of 5 | 2 | 7.1 | 31.3 | 19,640 |
| p3-off-65536 | 5 of 5 | 16 | 9.9 | 31.9 | 25,680 |

### integrity

| run | result.txt (0 = pass) | fsck json present | EIO/ENOSPC lines in daemon log | fio errors (all cells) | enospc+fulls deltas |
|---|---|---|---|---|---|
| p1-off-16384 | 0 | True | 0 | 0 | 0 |
| p1-on-16384 | 0 | True | 0 | 0 | 0 |
| p1-on-65536 | 0 | True | 0 | 0 | 0 |
| p2-off-16384 | 0 | True | 0 | 0 | 0 |
| p2-off-65536 | 0 | True | 0 | 0 | 0 |
| p2-on-16384 | 0 | True | 0 | 0 | 0 |
| p2-on-65536 | 0 | True | 0 | 0 | 0 |
| p3-off-65536 | 0 | True | 0 | 0 | 0 |


## Verdict against the thresholds set in advance

| threshold | OFF (reference) | ON | verdict |
|---|---|---|---|
| 64K ON: stops in at most 2 of 5 reps | 10 of 10 reps stop (41 stops, 4-6 per rep in pass 2) | block 1: 1 of 5 (4 stops); block 2: 2 of 5 (1 + 1 stops) | **pass** (the literal threshold) |
| 64K ON: median max completion <= 10 s | 21.5 s | 9.7 s (block 1), 7.1 s (block 2); 7.1 s pooled | **pass**, block 1 only just |
| 64K ON refuted if >= 4 of 5 reps stop with max >= 18 s | | no: 1 and 2 reps stop, 2 reps reach 18 s or more (37.3 s, 31.3 s) | **not refuted** |
| 64K ON, judged on p90 and worst run | p90 37.4 s, worst 41.4 s | p90 31.9 s, worst 37.3 s | **fail on the tail**: the worst run is the first repetition of each block, 37.3 s and 31.3 s; repetitions 2-5 have a median of 6.7 s and a maximum of 17.0 s |
| 64K any arm: IOPS >= 30K | median 24.9 K (p90 31.8 K, worst 19.6 K) | median 19.6 K (p90 23.3 K, worst 16.1 K) | **fail in both arms** (the OFF arm too: the 30 K bar is not met by this setup's median) |
| 64K ON vs OFF, relative IOPS | | -21 % at the median (19,603 vs 24,917), -27 % at p90, -18 % at the worst run | the pacing costs throughput |
| 64K any arm: p99.9 <= 2 s | median 1.93 s (p90 4.05 s, worst 6.21 s) | median 2.82 s (p90 4.23 s, worst 8.66 s) | **fail for ON** at the median (OFF passes at the median, fails at p90 and the worst) |
| 16K ON (control): 0 stops | 5 stops in 2 of 10 reps (OFF) | **0 stops in 10 of 10 reps** | **pass** |
| 16K ON: max <= 10 s | median 5.7 s, p90 14.6 s, worst 16.8 s | median 4.2 s, p90 11.5 s, worst 13.1 s | median **pass**; p90 and worst **fail**; both reps over 10 s are repetition 1 of a block (11.3 s, 13.1 s); repetitions 2-5: median 3.8 s, max 5.1 s |
| 16K ON: >= 40K IOPS | median 43.5 K | median 41.3 K (p90 42.8 K, worst 37.5 K); 3 of 10 reps below 40 K | median **pass**, worst **fail** (-5 % against OFF at the median) |
| w4k cells: delay_capped 0 | 64K up to 4, 16K up to 3 (in the reps with stops) | **64K up to 4 (the reps that stop: block 1 rep 1: 3; block 2 rep 1: 4), 16K 0** | 64K ON **fail** (same reps as the stops), 16K ON **pass** |
| fsck --deep clean, no EIO/ENOSPC | | clean on all 8 final volumes; no fio error, no EIO/ENOSPC line, `fulls` and `enospc` deltas 0 | **pass** |

Overall: **P1' does what it was built for in the bulk of the runs and not in the tail.** Stops at 64K fall from 41 (10 of 10 reps) to 6 (3 of 10 reps), the median maximum completion from 21.5 s to 7.1 s, and
the 16K control stays at 0 stops in ten repetitions (OFF stopped in 2 of 10). But the first repetition after a fresh volume still stops and reaches 31-37 s at 64K (and 11-13 s at 16K), the worst
run is not better than OFF's by much (37.3 s against 41.4 s), the 64K IOPS and p99.9 targets are missed by both arms and ON is 21 % slower than OFF at the median. By the thresholds set in advance:
the stop-count and median-completion thresholds pass, the refutation condition is not met, the p90 / worst-run judgement fails, the IOPS and p99.9 thresholds fail.

## What the daemon did

- **Pacing above the soft mark happened.** In all 20 ON w4k repetitions (16K and 64K) `store pace above the soft mark` was logged, 1-6 times per repetition;
  `store_above_soft_ms` 0.1-2.3 s per 60 s at 16K and 0.7-21.5 s at 64K, `store_delayed_ms` (the delays of the paced writes summed over all writes) up to 4.9 million ms per repetition at 64K. The OFF arm has none by construction.
  The throttle state stays `open` above the raw soft mark (as designed): the `throttled_ms` of the ON arm is similar to OFF's (42 s against 50 s median at 64K, 35 s against 43 s at 16K).
- **The factor.** At 64K the measured store factor of the cut reached 3.2-7.2 (`store_factor_milli` max per repetition), at 16K 1.6-2.0; the constant 2000 of the OFF arm is below the 64K
  reality in most cuts. The bias of 115 % is visible (`store_factor_biased_milli`).
- **A_hat error** (real - predicted, milli; pooled over the w4k cuts of the arm; `tables.md` has the full set): at 64K ON 359 cuts, median error +241, p10 -1,888, p90 +3,907, p99 +4,603, the real factor
  higher than predicted in 54 % of cuts and by more than 20 % in 44 %; OFF 64K: 368 cuts, median -30, p90 +4,155. For the cuts that matter (journal >= 128 MiB, 173 at 64K ON) the error is
  tighter and biased high: median -502, relative error -97 / -31 / +14 % (p10 / p50 / p90), i.e. A_hat over-predicts, as the margin intends, but p10 shows cuts whose real factor was half of the
  prediction. At 16K ON (349 large cuts) relative error -29 / -6 / +9 %. The large errors at 64K come from the first cuts of a cell (initial 2.0 against a first cut of 0.5, a small journal) and from the factor
  moving between 3 and 7 with the garbage-collection state; A_hat follows the previous cut, so a swing is paid for once. Under-prediction by more than 20 % in 44 % of all 64K cuts is the exposure that remains.
- **Where the stops are.** All stops of the ON arm at 64K come in repetition 1 or 4 of a block (4, 1, 1 stops; repetition 1 of each block has dwell 16.8-21.2 s from throttle on to the stop); repetitions 2-5 of both
  blocks had maxima of 17.0, 6.0, 6.4, 9.7 s (block 1) and 6.1, 5.2, 7.2, 7.1 s (block 2), and one stop (block 2, repetition 4, max 7.2 s). Repetition 1 follows the 2 GiB fill on a fresh volume with A_hat still at its initial value and an empty store.

## Surprising / worth a look

- The ON arm at 64K is **slower, not just calmer**: -21 % IOPS at the median and a worse median p99.9 (2.8 s against 1.9 s) while the stop count drops by 85 %. The pacing starts earlier (0.7-21 s above the soft
  mark) and the writes that wait are charged to the p99.9 instead of a few very long ones to the maximum. The per-repetition tail is the first repetition (cold A_hat) in both chunk sizes.
- The OFF arm at 16K stopped in 2 of 10 repetitions (4 and 1 stops), so the 16K "0 stops" bar is one the plain curve nearly meets as well; ON's gain there is in the maximum (median 5.7 to 4.2 s) rather than in a regime change.
- `delay_capped` rises exactly in the repetitions that stop (64K, both arms), so it signals the stop episodes rather than the pacing.
- The daemon refuses a config with `destage_workers` on main (deviation 1); anyone repeating this on main without the plumbing commit gets 4 workers.

