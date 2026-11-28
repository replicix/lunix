# p1-shipcheck: the shipped GC defaults in a T6-style run on main 19b31c20 (2026-11-28)

Branch `p1/shipcheck` from `origin/main` 19b31c20 (the merge of `p1/c5def`). Nothing in the daemon changed; the branch carries the measurement harness (the c5def harness with the arms renamed), this report and the results. Not merged.

## Result

| clause | verdict |
|---|---|
| A safety (every block, all of it) | **PASS** (11 of 11 blocks) |
| B rep 1 (64K: all five rep-1 checks; 16K: no stop, first delete <= 3 s) | **PASS** (SHIP b01, b04, b06, b07) |
| D reps 2-5 stops 0 | **PASS** (0 stops in 5 SHIP blocks, 20 reps) |
| F pin cell (corrected check) | **PASS** (zero_waiting never above the segment files; peaks 417 / 462 / 447; below 64 again 4.6 s after the release; queue_len peak 211, bounded) |
| A2 effective configuration (not a clause of the brief; the assertion it asked for) | PASS, see below |
| C, E (not gated here) | E PASS (worst 0.081 % of wall time, mean 392-510 us per call). C is reported only: see the note under D |

Method: the harness of `p1/c5def-t6` (`tests/perf/c5def.sh`, `c5def-root.sh`, `bench/phase1/perf/c5def.py`, force-added), with two arms. **SHIP**: the daemon settings file has no `gc_*` and no `throttle_*` key at all. **OLD**: `gc_pressure_grace_ms = 0`, `gc_fast_recheck = false`, `gc_condemn_on_cut = "off"` (`throttle_pace_store_bytes` is at its default, off, in both). Release build of 19b31c20, one fresh daemon and volume per block (tmpfs store, 16 GiB volume, 2 GiB working set, 8 jobs, queue depth 128, 60 s per rep, 5 reps, `lunix fsck --deep` and a read-back verify at the end, `log_level = debug`, kernel 7.0.0, 16 CPUs, daemon on CPUs 0-7 and fio on 8-15). Blocks in the order run: 64K 01 SHIP, 02 OLD, 03 OLD, 04 SHIP; 16K 05 OLD, 06 SHIP, 07 SHIP, 08 OLD; zipf (64K chunks, `w4kz`) 09 SHIP, 10 OLD; pin 11 SHIP (plain volume, pin held through reps 1-3 and removed before rep 4). The clauses are those of `p1-c5def.md` (A, B, D, F as corrected). Raw results (paths removed) are in `bench/phase1/perf/results/shipcheck/` (`tables.md`, `summary.json`, `throttle-off.md`, one directory per block).

## SHIP against OLD

Medians over the blocks of a cell. First delete is counted from the onset of pressure in rep 1.

| cell | arm | blocks | first delete after onset s | stops, rep 1 | longest wait s, rep 1 | throttled s, rep 1 | rep 1 IOPS | stops, reps 2-5 | longest wait s, reps 2-5 (med / worst) | reps 2-5 IOPS |
|---|---|---|---|---|---|---|---|---|---|---|
| 64K | SHIP | 2 | 0.09 | 0 | 0.8 | 4.1 | 69,142 | 0 | 0.88 / 0.95 | 68,154 |
| 64K | OLD | 2 | 37.00 | 3 | 31.7 | 36.1 | 31,119 | 42 | 16.45 / 23.75 | 28,027 |
| 16K | SHIP | 2 | 1.90 | 0 | 0.9 | 5.1 | 66,083 | 0 | 1.07 / 1.14 | 65,000 |
| 16K | OLD | 2 | 24.50 | 6 | 23.3 | 33.7 | 35,242 | 0 | 1.58 / 10.57 | 44,237 |
| zipf | SHIP | 1 | 0.69 | 0 | 0.7 | 1.8 | 70,386 | 0 | 0.61 / 0.73 | 72,539 |
| zipf | OLD | 1 | 23.24 | 3 | 20.2 | 31.1 | 37,391 | 0 | 2.84 / 5.19 | 44,095 |

OLD is the configuration before the defaults changed (no short grace, no fast recheck, no condemn on cut). It is much worse than the c5def OFF arm, which already had the 2 s grace and fast recheck; the two are not comparable. The 64K OLD blocks stop 42 times in reps 2-5 (the SHIP blocks never).

## Write throttle off

Every `write throttle off` line of every daemon log was matched with the status samples (every 2 s) next to it (`throttle-off.md`); the line carries no headroom itself, the samples give it.

SHIP: **the line appears** (114 times in the six SHIP blocks: it is the normal end of each throttled stretch, 13-25 per block), **never with low headroom**: the headroom of the sample before and the sample after is 4.86 GiB or more in every case (up to 12.7 GiB), none below 1.2 GiB, and the throttle never reached `stop` (0 stop lines). The `write throttle on` lines of SHIP opened at 1.49-2.73 GiB headroom (soft mark about 2.5 GiB); the pin block had one at 0.35 GiB, and its off line came at 4.86 GiB or more. OLD: 91 off lines in 5 blocks, **29 with a neighbouring sample below 1.2 GiB** (lowest 0.00), 19 of them in the 64K blocks: the throttle reached `stop` and opened again at the resume mark (about 0.17 GiB) as the deletes freed space, or the headroom moved across 1.2 GiB between two samples. So the earlier observation (off while headroom was 0.3-1.2 GiB) reproduces with OLD and does not occur with SHIP. Limit: the headroom is read from samples 2 s apart.

## Tables

### Write throttle off lines per block

| block | on lines | off lines | stop lines | off with the sample before / after below 1.2 GiB | min headroom at off GiB (before / after) | max | on-line headroom GiB (min / max) |
|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | 24 | 24 | 0 | 0 | 5.13 | 12.27 | 1.50 / 2.61 |
| b02-OLD-65536-w4k | 19 | 19 | 0 | 12 | 0.00 | 10.64 | 0.09 / 2.79 |
| b03-OLD-65536-w4k | 15 | 14 | 0 | 7 | 0.07 | 9.65 | 0.10 / 2.50 |
| b04-SHIP-65536-w4k | 23 | 23 | 0 | 0 | 6.71 | 12.25 | 1.49 / 2.53 |
| b05-OLD-16384-w4k | 21 | 20 | 0 | 3 | 0.06 | 6.61 | 1.01 / 2.51 |
| b06-SHIP-16384-w4k | 13 | 13 | 0 | 0 | 6.17 | 12.12 | 1.84 / 2.53 |
| b07-SHIP-16384-w4k | 13 | 13 | 0 | 0 | 6.71 | 11.92 | 1.91 / 2.56 |
| b08-OLD-16384-w4k | 19 | 18 | 0 | 5 | 0.97 | 7.44 | 1.29 / 2.65 |
| b09-SHIP-65536-w4kz | 16 | 16 | 0 | 0 | 5.94 | 12.71 | 2.02 / 2.56 |
| b10-OLD-65536-w4kz | 21 | 20 | 0 | 2 | 0.06 | 6.81 | 0.56 / 2.64 |
| b11-SHIP-65536-pin | 25 | 25 | 0 | 0 | 4.86 | 11.88 | 0.35 / 2.73 |

### A. Safety, every block

| block | fio errors | verify | fsck | ref-table lines | drift | mismatches | rebases | rejected early | named | propose errors | uncond / condemned (lines) | hold footer errors | exhausted | auth failures | min pgrace wait ms | EIO/ENOSPC | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 134 | 0 | 0 | 0 | 0 / 15179 (0) | 0 | 0 | 0 | 2019 | 0 | PASS |
| b02-OLD-65536-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 0 | 0 | PASS |
| b03-OLD-65536-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 0 | 0 | PASS |
| b04-SHIP-65536-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 133 | 0 | 0 | 0 | 0 / 14897 (0) | 0 | 0 | 0 | 2000 | 0 | PASS |
| b05-OLD-16384-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 0 | 0 | PASS |
| b06-SHIP-16384-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 67 | 0 | 0 | 0 | 0 / 8343 (0) | 0 | 0 | 0 | 2064 | 0 | PASS |
| b07-SHIP-16384-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 67 | 0 | 0 | 0 | 0 / 8470 (0) | 0 | 0 | 0 | 2015 | 0 | PASS |
| b08-OLD-16384-w4k | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 0 | 0 | PASS |
| b09-SHIP-65536-w4kz | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 85 | 0 | 0 | 0 | 0 / 9162 (0) | 0 | 0 | 0 | 2004 | 0 | PASS |
| b10-OLD-65536-w4kz | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 0 | 0 | PASS |
| b11-SHIP-65536-pin | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 136 | 0 | 0 | 0 | 0 / 7020 (0) | 0 | 0 | 0 | 2022 | 0 | PASS |

### A2. Effective configuration of every block

The daemon has no config dump. Evidence: the block's settings file (paths removed) lists no `gc_*` or `throttle_*` key in SHIP and exactly the three OLD keys in OLD; the GC status shows the behaviour each setting implies (short segment grace `segment_grace_ms` = 2000 under pressure, fast rechecks, per-pressure-grace deletes, offers and condemnations at the cut, `refs` table state); the config unit tests give the compiled defaults (see the report).

| block | arm | gc/throttle keys in settings | segment_grace_ms values seen | fast passes | pgrace deletes | proposals offered | condemned | refs rebases | throttle_pace store fields (store_paced_above_soft ever true) | verdict |
|---|---|---|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | SHIP | none | [0, 2000] | 74 | 17256 | 15179 | 15179 | 134 | False | PASS |
| b02-OLD-65536-w4k | OLD | gc_condemn_on_cut, gc_fast_recheck, gc_pressure_grace_ms | [0] | 0 | 0 | 0 | 0 | None | False | PASS |
| b03-OLD-65536-w4k | OLD | gc_condemn_on_cut, gc_fast_recheck, gc_pressure_grace_ms | [0] | 0 | 0 | 0 | 0 | None | False | PASS |
| b04-SHIP-65536-w4k | SHIP | none | [0, 2000] | 72 | 17184 | 14897 | 14897 | 133 | False | PASS |
| b05-OLD-16384-w4k | OLD | gc_condemn_on_cut, gc_fast_recheck, gc_pressure_grace_ms | [0] | 0 | 0 | 0 | 0 | None | False | PASS |
| b06-SHIP-16384-w4k | SHIP | none | [0, 2000] | 26 | 8865 | 8343 | 8343 | 67 | False | PASS |
| b07-SHIP-16384-w4k | SHIP | none | [0, 2000] | 26 | 8647 | 8470 | 8470 | 67 | False | PASS |
| b08-OLD-16384-w4k | OLD | gc_condemn_on_cut, gc_fast_recheck, gc_pressure_grace_ms | [0] | 0 | 0 | 0 | 0 | None | False | PASS |
| b09-SHIP-65536-w4kz | SHIP | none | [0, 2000] | 34 | 10921 | 9162 | 9162 | 85 | False | PASS |
| b10-OLD-65536-w4kz | OLD | gc_condemn_on_cut, gc_fast_recheck, gc_pressure_grace_ms | [0] | 0 | 0 | 0 | 0 | None | False | PASS |
| b11-SHIP-65536-pin | SHIP | none | [0, 2000] | 74 | 17428 | 7020 | 7020 | 136 | False | PASS |

### B. Rep 1 (cold run after the fill)

| block | arm | chunk | first delete after onset s | stops | longest wait s | throttled s | GiB deleted <= 5 s | first delete <= 3 s | 0 stops | wait <= 5 s | throttled <= 22 s | >= 6 GiB | all |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | SHIP | 64K | 0.08 | 0 | 0.9 | 4.0 | 12.63 | pass | pass | pass | pass | pass | PASS |
| b02-OLD-65536-w4k | OLD | 64K | 37.31 | 1 | 32.3 | 36.8 | 0.00 | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |
| b03-OLD-65536-w4k | OLD | 64K | 36.70 | 2 | 31.1 | 35.4 | 0.00 | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |
| b04-SHIP-65536-w4k | SHIP | 64K | 0.09 | 0 | 0.8 | 4.2 | 12.08 | pass | pass | pass | pass | pass | PASS |
| b05-OLD-16384-w4k | OLD | 16K | 24.62 | 3 | 22.8 | 33.5 | 0.00 | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |
| b06-SHIP-16384-w4k | SHIP | 16K | 1.90 | 0 | 0.9 | 4.1 | 11.55 | pass | pass | pass | pass | pass | PASS |
| b07-SHIP-16384-w4k | SHIP | 16K | 1.90 | 0 | 0.9 | 6.2 | 10.56 | pass | pass | pass | pass | pass | PASS |
| b08-OLD-16384-w4k | OLD | 16K | 24.37 | 3 | 23.8 | 33.8 | 0.00 | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |

### D. Steady state, reps 2-5, SHIP against OLD, pooled over blocks (gate: SHIP stops = 0; the other rows are reported)

| cell | metric | OLD | SHIP | limit | verdict |
|---|---|---|---|---|---|
| 64K w4k | IOPS (median) | 28,027 | 68,154 | ON >= 0.97 OFF | PASS |
| 64K w4k | p99.9 s (median) | 4.454 | 0.625 | ON <= 1.10 OFF | PASS |
| 64K w4k | stops (total) | 42 | 0 | SHIP = 0 (gate) | PASS |
| 64K w4k | longest wait s (median) | 16.45 | 0.88 | ON <= OFF + 1 s | PASS |
| 64K w4k | mark share of the coordinator % (mean) | 1.27 | 3.33 | ON - OFF <= 1.0 point | FAIL |
| 64K w4k | evidence objects per minute (max rep) | - | 25 | <= 60 | PASS |
| 16K w4k | IOPS (median) | 44,237 | 65,000 | ON >= 0.97 OFF | PASS |
| 16K w4k | p99.9 s (median) | 0.772 | 0.734 | ON <= 1.10 OFF | PASS |
| 16K w4k | stops (total) | 0 | 0 | SHIP = 0 (gate) | PASS |
| 16K w4k | longest wait s (median) | 1.58 | 1.07 | ON <= OFF + 1 s | PASS |
| 16K w4k | mark share of the coordinator % (mean) | 5.87 | 8.22 | ON - OFF <= 2.0 point | FAIL |
| 16K w4k | evidence objects per minute (max rep) | - | 14 | <= 60 | PASS |
| 64K w4kz | IOPS (median) | 44,095 | 72,539 | ON >= 0.97 OFF | PASS |
| 64K w4kz | p99.9 s (median) | 0.532 | 0.365 | ON <= 1.10 OFF | PASS |
| 64K w4kz | stops (total) | 0 | 0 | SHIP = 0 (gate) | PASS |
| 64K w4kz | longest wait s (median) | 2.84 | 0.61 | ON <= OFF + 1 s | PASS |
| 64K w4kz | mark share of the coordinator % (mean) | 1.60 | 2.52 | ON - OFF <= 1.0 point | PASS |
| 64K w4kz | compaction written per rep MiB | 2,916 | 2,391 | ON >= 0.9 OFF | FAIL |
| 64K w4kz | evidence objects per minute (max rep) | - | 15 | <= 60 | PASS |

### E. Proposal cost, ON blocks

| block | arm | reps | max propose_us_sum / wall % | mean us per call | propose_us_max us | orphan deferrals | cuts requested | verdict |
|---|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | SHIP | 5 | 0.081 | 510 | 1357 | 0 | 0 | PASS |
| b04-SHIP-65536-w4k | SHIP | 5 | 0.076 | 506 | 1266 | 0 | 0 | PASS |
| b06-SHIP-16384-w4k | SHIP | 5 | 0.037 | 392 | 721 | 0 | 0 | PASS |
| b07-SHIP-16384-w4k | SHIP | 5 | 0.034 | 398 | 841 | 0 | 0 | PASS |
| b09-SHIP-65536-w4kz | SHIP | 5 | 0.044 | 466 | 1089 | 0 | 0 | PASS |

### F. Pin cell (pin held through reps 1-3, removed before rep 4; plain volume), corrected check

While pinned: pin_blocked > 0, nothing offered or condemned, `zero_waiting` <= the segment files in the store at that sample (`nseg` of series.jsonl: a listing of the volume's segment directory every 2 s), and the per-rep peak of `zero_waiting` levels off over reps 1-3 (fails if every pinned rep's peak is more than 10 % above the one before). After the release: condemned > 0 in rep 5, `zero_waiting` below 64 within 30 s.

Block b11-SHIP-65536-pin (SHIP): pin released at 1795874160941.

| rep | pin_blocked delta | proposals_offered | proposal_condemned | zero_waiting peak | segment files (max) | samples with zero_waiting above the file count | refs.segments (end) | IOPS | stops |
|---|---|---|---|---|---|---|---|---|---|
| 1 (pinned) | 313 | 0 | 0 | 417 | 1039 | 0 | 218 | 68,056 | 0 |
| 2 (pinned) | 327 | 0 | 0 | 462 | 912 | 0 | 183 | 68,769 | 0 |
| 3 (pinned) | 329 | 0 | 0 | 447 | 937 | 0 | 146 | 68,228 | 0 |
| 4 | 9 | 2732 | 2732 | 174 | 904 | 0 | 181 | 69,170 | 0 |
| 5 | 0 | 2790 | 2790 | 243 | 889 | 0 | 208 | 69,423 | 0 |

zero_waiting peaks over pinned reps 1-3: [417, 462, 447]; levels off
zero_waiting below 64 again 4.6 s after the release (limit 30 s); orphan_deferrals 0, hold_marks 0
refs.queue_len peak 211; largest queue_len - 2 x zero_waiting over all samples 7 (documented bound: <= 64, the queue slack): bounded

### C. Cut cost per journal byte, ON against OFF

| cell | arm | windows | cuts | mean cut MiB | mean cut ms | us / MiB (plain, +- se) | us / MiB at pooled size | vs OFF at pooled size | OFF se % | map ns / chunk |
|---|---|---|---|---|---|---|---|---|---|---|
| 64K w4k | OLD | 10 | 340 | 226.3 | 345.1 | 1,525 +- 28 | 1,481 (244.8 MiB) | - | 1.86 | 94 |
| 64K w4k | SHIP | 10 | 700 | 253.9 | 389.9 | 1,536 +- 15 | 1,564 (244.8 MiB) | +5.6 % | - | 275 |
| 16K w4k | OLD | 10 | 459 | 245.2 | 395.1 | 1,611 +- 9 | 1,613 (235.7 MiB) | - | 0.54 | 216 |
| 16K w4k | SHIP | 10 | 740 | 229.8 | 395.2 | 1,720 +- 20 | 1,717 (235.7 MiB) | +6.5 % | - | 432 |
| 64K w4kz | OLD | 5 | 302 | 186.3 | 180.9 | 971 +- 20 | 1,029 (165.0 MiB) | - | 2.03 | 284 |
| 64K w4kz | SHIP | 5 | 606 | 154.3 | 170.9 | 1,107 +- 24 | 1,053 (165.0 MiB) | +2.3 % | - | 526 |

Notes on the reported rows. **C** (cut cost per journal byte) compares SHIP with OLD at a pooled cut size: 64K +5.6 %, 16K +6.5 %, zipf +2.3 %. It is not a gate of this run; the c5def run, against an OFF arm that had the same grace and fast recheck, measured -0.8 % and +2.0 %, so the difference here mixes the condemn-on-cut work with the other two settings and with OLD's long stops. **D** rows other than stops: the mark share of the coordinator is 2.1 points higher in SHIP at 64K and 2.4 points at 16K (the c5def limits were 1 and 2 points against an OFF arm that already had grace and fast recheck), and compaction written per rep at zipf is 82 % of OLD. Both are reported, not gated, and not explained here.

## Effective configuration

The daemon has no configuration dump. The assertion is made of three parts, all in the table A2: (1) the block's settings file has no `gc_*` or `throttle_*` key in SHIP (and exactly the three OLD keys in OLD); (2) the GC status shows what the defaults imply: the short segment grace of 2000 ms in force under pressure, fast rechecks, deletes under the short grace (`pgrace_min_wait_ms` 2,000-2,064, never below), offers and condemnations at the cut (condemn on cut on, `refs` table `ready`), and `store_paced_above_soft` never true (`throttle_pace_store_bytes` off); OLD shows none of these; (3) the compiled defaults, from the `lunixd` config unit tests (release build, 10 passed): `gc_pressure_grace_ms` 2000 (`gc_pressure_grace_ms_defaults_to_2000_and_is_validated`), `gc_condemn_on_cut` on (`gc_condemn_on_cut_parses_off_shadow_and_on`), `throttle_pace_store_bytes` off (`the_throttle_keys_parse_and_are_validated`), the fast recheck keys (`the_fast_recheck_keys_parse_and_are_validated`).

## Deviations and limits

* The harness is c5def's with the arm set replaced (ON/OFF/ONS/OFFS -> SHIP/OLD); the OLD arm no longer sets `throttle_pace_store_bytes = true`. Clause B no longer requires the OLD arm to fail; D's gate is SHIP stops = 0 (the c5def comparison rows are still printed and reported); F gained a check of `refs.queue_len` against its documented bound (`queue_len <= 2 x zero_waiting + 64`; the largest `queue_len - 2 x zero_waiting` over all samples is 7). The per-block settings file is a new output.
* The 16K blocks gate stops and first delete only, as in c5def. One block per cell for zipf and the pin.
* The host was shared: the system load average read 114-120 at the start of the blocks after the first (other work on the machine); every block started only after the quiet check (CPU and IO pressure below 1 % for 10 s, no fio or lunixd alive), as in c5def.
* No pricing. Commits signed, no trailers. rust-skills / digest: no Rust written; no conflict.

## Clean-up

Remote tree, scratch results copy, tmpfs and build target removed; no daemon, fio or ublk device left (leftovers check clean).
