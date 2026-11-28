# c5def tables

Arms: SHIP = no GC or throttle override (the shipped defaults); OLD = `gc_pressure_grace_ms = 0`, `gc_fast_recheck = false`, `gc_condemn_on_cut = "off"`. `log_level = debug` in both. Rep 1 is the cold run after the fill, reported apart.

## Blocks

| block | arm | chunk | kind | load 1/5/15 at start | reps | result | fsck |
|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | SHIP | 64K | w4k | 1.98/0.65/0.23 | 5 | 0 | 0 |
| b02-OLD-65536-w4k | OLD | 64K | w4k | 119.29/254.99/139.26 | 5 | 0 | 0 |
| b03-OLD-65536-w4k | OLD | 64K | w4k | 114.39/316.47/229.63 | 5 | 0 | 0 |
| b04-SHIP-65536-w4k | SHIP | 64K | w4k | 114.38/330.62/285.93 | 5 | 0 | 0 |
| b05-OLD-16384-w4k | OLD | 16K | w4k | 116.91/334.84/320.46 | 5 | 0 | 0 |
| b06-SHIP-16384-w4k | SHIP | 16K | w4k | 116.08/333.60/340.90 | 5 | 0 | 0 |
| b07-SHIP-16384-w4k | SHIP | 16K | w4k | 116.06/331.61/352.97 | 5 | 0 | 0 |
| b08-OLD-16384-w4k | OLD | 16K | w4k | 115.60/333.61/361.87 | 5 | 0 | 0 |
| b09-SHIP-65536-w4kz | SHIP | 64K | w4kz | 114.83/327.44/363.70 | 5 | 0 | 0 |
| b10-OLD-65536-w4kz | OLD | 64K | w4kz | 116.70/331.71/368.60 | 5 | 0 | 0 |
| b11-SHIP-65536-pin | SHIP | 64K | pin | 106.80/329.02/370.71 | 5 | 0 | 0 |

## A. Safety, every block

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

## A2. Effective configuration of every block

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

## B. Rep 1 (cold run after the fill)

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

## C. Cut cost per journal byte, ON against OFF

| cell | arm | windows | cuts | mean cut MiB | mean cut ms | us / MiB (plain, +- se) | us / MiB at pooled size | vs OFF at pooled size | OFF se % | map ns / chunk |
|---|---|---|---|---|---|---|---|---|---|---|
| 64K w4k | OLD | 10 | 340 | 226.3 | 345.1 | 1,525 +- 28 | 1,481 (244.8 MiB) | - | 1.86 | 94 |
| 64K w4k | SHIP | 10 | 700 | 253.9 | 389.9 | 1,536 +- 15 | 1,564 (244.8 MiB) | +5.6 % | - | 275 |
| 16K w4k | OLD | 10 | 459 | 245.2 | 395.1 | 1,611 +- 9 | 1,613 (235.7 MiB) | - | 0.54 | 216 |
| 16K w4k | SHIP | 10 | 740 | 229.8 | 395.2 | 1,720 +- 20 | 1,717 (235.7 MiB) | +6.5 % | - | 432 |
| 64K w4kz | OLD | 5 | 302 | 186.3 | 180.9 | 971 +- 20 | 1,029 (165.0 MiB) | - | 2.03 | 284 |
| 64K w4kz | SHIP | 5 | 606 | 154.3 | 170.9 | 1,107 +- 24 | 1,053 (165.0 MiB) | +2.3 % | - | 526 |

## D. Steady state, reps 2-5, SHIP against OLD, pooled over blocks (gate: SHIP stops = 0; the other rows are reported)

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

## E. Proposal cost, ON blocks

| block | arm | reps | max propose_us_sum / wall % | mean us per call | propose_us_max us | orphan deferrals | cuts requested | verdict |
|---|---|---|---|---|---|---|---|---|
| b01-SHIP-65536-w4k | SHIP | 5 | 0.081 | 510 | 1357 | 0 | 0 | PASS |
| b04-SHIP-65536-w4k | SHIP | 5 | 0.076 | 506 | 1266 | 0 | 0 | PASS |
| b06-SHIP-16384-w4k | SHIP | 5 | 0.037 | 392 | 721 | 0 | 0 | PASS |
| b07-SHIP-16384-w4k | SHIP | 5 | 0.034 | 398 | 841 | 0 | 0 | PASS |
| b09-SHIP-65536-w4kz | SHIP | 5 | 0.044 | 466 | 1089 | 0 | 0 | PASS |

## F. Pin cell (pin held through reps 1-3, removed before rep 4; plain volume), corrected check

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


## Shipping defaults and restart cells, reported

| block | arm | kind | rep 1: first delete s | stops | wait s | throttled s | GiB <= 5 s | rep 1 IOPS | reps 2-5 IOPS (med) | p99.9 s (med) | stops 2-5 | all rep-1 clauses |
|---|---|---|---|---|---|---|---|---|---|---|---|---|

## SHIP against OLD, per cell (medians over blocks; rep 1 and reps 2-5 apart)

| cell | arm | blocks | rep 1: first delete after onset s | rep 1 stops (sum) | rep 1 longest wait s | rep 1 throttled s | rep 1 IOPS | reps 2-5 stops (sum) | reps 2-5 longest wait s (med / worst) | reps 2-5 throttled s (med) | reps 2-5 IOPS (med) | reps 2-5 p99.9 s (med) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 64K w4k | SHIP | 2 | 0.09 | 0 | 0.8 | 4.1 | 69,142 | 0 | 0.88 / 0.95 | 4.1 | 68,154 | 0.625 |
| 64K w4k | OLD | 2 | 37.00 | 3 | 31.7 | 36.1 | 31,119 | 42 | 16.45 / 23.75 | 55.4 | 28,027 | 4.454 |
| 16K w4k | SHIP | 2 | 1.90 | 0 | 0.9 | 5.1 | 66,083 | 0 | 1.07 / 1.14 | 5.8 | 65,000 | 0.734 |
| 16K w4k | OLD | 2 | 24.50 | 6 | 23.3 | 33.7 | 35,242 | 0 | 1.58 / 10.57 | 48.0 | 44,237 | 0.772 |
| 64K w4kz | SHIP | 1 | 0.69 | 0 | 0.7 | 1.8 | 70,386 | 0 | 0.61 / 0.73 | 3.1 | 72,539 | 0.365 |
| 64K w4kz | OLD | 1 | 23.24 | 3 | 20.2 | 31.1 | 37,391 | 0 | 2.84 / 5.19 | 51.3 | 44,095 | 0.532 |

## Clause summary

| clause | verdict |
|---|---|
| A | PASS |
| A2_effective_config | PASS |
| B | PASS |
| C | FAIL |
| C_16K | FAIL |
| C_64K | UNDECIDED |
| D | PASS |
| D_comparisons_reported | FAIL |
| E | PASS |
| F | PASS |
