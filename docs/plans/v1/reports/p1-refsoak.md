# p1-refsoak: real-daemon shadow soak of the C5 reference table

Branch `p1/refsoak` from `origin/main` fcacaf4. Measurement only: no product code changed (new files: `tests/perf/refsoak.sh`,
`tests/perf/refsoak-cells.sh`, `bench/phase1/perf/refsoak.py`, three fio files, the T6 harness of `p1/recheck16k` brought over unchanged,
results under `bench/phase1/perf/results/refsoak/`: `tables.md`, `summary.json`, one directory per block, no hostinfo files).
Everything ran on the remote test host (kernel 7.0.0, 16 CPUs, nothing else running), one release build, one tree and target directory
(no mtime trap). Every arm: `throttle_pace_store_bytes = true`, `gc_fast_recheck = true`, `gc_condemn_on_cut = "shadow"` (the cost
check also has `"off"`), `log_level = debug`, `destage_workers = 8`, no pins or snapshots, 16 GiB integrity volume, 1 GiB journal,
4 queues, daemon on CPUs 0-7, fio on 8-15, 20 GiB tmpfs for the store, 2 GiB working set, fresh daemon and volume per block, each block
started with the 1-minute load below 1 and the 5-minute load below 2, `lunix fsck --deep` at the end of each. Polling: `lunix gc --status
--json` and `lunix status --json` every 5 s (2 s in the cost blocks), `lunix status` timed once a second.

## Verdict

**GO for milestone 3 (active C5) on the four stated criteria.**

| criterion | result |
|---|---|
| `shadow_unconfirmed_named` == 0 | **0 in all 6 cells and both processes of the restart cell** |
| `drift_segments` == 0 (and `report_mismatches` == 0) | **0 / 0 in all 6 cells**, with 105-147 rebases per cell (108 across the restart); no "reference table" log line anywhere |
| `unconfirmed_live` < 5 % of confirmed | **0 of 2,302 to 8,152 confirmed in every cell (0.000 %)**; `_kept` 0; `_listed` 0 except 74 (16K) and 43 (discard) |
| positive lead on the 64K cells | **yes**: 93-100 % of confirmed segments had a lead; mean lead 2.2-3.9 s, max 13.5-21.9 s |

Caveats the decision should carry (none is a blocker by the criteria, all are findings):

1. **Recall is low outside the dedup cell**: 31 to 54 % by segments and by bytes (98 % in the dedup cell). The loss is almost all
   `early_by_mark`, not `missed`: the mark condemned the segment before the table offered it (the `2 x (retain_last - 1)` cut lag, the
   cap of 64, or a mark in a call without an offer). `missed` (hints cannot see it) is 0.0-1.3 % of bytes. So active C5 will
   free about a third to a half of the segments earlier than the mark, by about 3 s on average, not all of them.
2. **The 3 % cut-time gate is not shown to hold at 16K** (+6.8 % byte-weighted, +3.7 % reps 2-3); at 64K it holds byte-weighted
   (+1.6 %) and misses narrowly over reps 2-3 (+3.3 %). See Cost. n is 6 repetitions per arm and the spread between repetitions is 5-15 %.
3. **The discard cell did not exercise the `ready()` flood**: the volume holds at most 551 segments, `zero_waiting` peaked at 218.
   `ready()` is O(zeros) per step (8.1 ms at 500k waiting in the engine cost test): not seen here, the review's "must change before active C5"
   stands for large volumes. Likewise `rebase` is 360 ms at 1M segments (15.9 ms at 128k).
4. Pins and snapshots were excluded as instructed; the shadow mode ignores pins, so the active stage's pin rule is not covered.

No product bug found.

## Cells (5 repetitions each, fresh volume)

Cell design (all on the 16 GiB volume, working set 2 GiB unless said):

| cell | chunk | workload per repetition |
|---|---|---|
| 1 | 64K | `w4k.fio` (4K random write, QD 128 x 8 jobs, 60 s + 5 s ramp). **One clean daemon restart in rep 3, 30 s into the window** |
| 2 | 16K | `w4k.fio` |
| 3 | 64K | `w4kz.fio` (zipf:1.2) |
| 4 | 64K | `w64d.fio`: 64K random write aligned to the chunk, `dedupe_percentage=50`, QD 32 x 8 jobs (4K blocks cannot duplicate a 64K chunk, so the job writes whole chunks); `dedup_hits` was 50 % of the chunks written |
| 5 | 64K | `comp.fio`, three stonewalled phases over a **4 GiB working set** (a quarter of the volume): (a) 4K random write over all 4 GiB, 30 s; (b) one 1M sequential pass rewriting **the first half**; (c) 4K random write over the second half, 30 s. Phase a spreads each segment's chunks over random places, phase b kills about half of every segment: many half-live segments while the load goes on. 137 rebases, 2,302 confirmed; the collector compacted 6,652 segments (12.2 GiB read, 12.2 GiB written) |
| 6 | 64K | refill [4 GiB, 10 GiB) (1M sequential write), `blkdiscard` of it (6 GiB = 37.5 % of the volume, 5 ms per call), then `w4k.fio` 60 s. `ops_discard` 25, `chunks_zero` 491,520 in the final status: the discards were applied |

### 1 and 2. Table correctness, lag rule (all processes of a block summed)

| cell | procs | drift_segments | report_mismatches | rebases | named | "reference table" log lines | fsck --deep | fio errors / EIO, ENOSPC lines, fulls, enospc |
|---|---|---|---|---|---|---|---|---|
| 1 64K w4k (+restart) | 2 | 0 | 0 | 108 | 0 | 0 | clean | 0 / 0, 0, 0 |
| 2 16K w4k | 1 | 0 | 0 | 105 | 0 | 0 | clean | 0 / 0, 0, 0 |
| 3 64K zipf | 1 | 0 | 0 | 127 | 0 | 0 | clean | 0 / 0, 0, 0 |
| 4 64K dedup | 1 | 0 | 0 | 134 | 0 | 0 | clean | 0 / 0, 0, 0 |
| 5 64K compaction | 1 | 0 | 0 | 137 | 0 | 0 | clean | 0 / 0, 0, 0 |
| 6 64K discard | 1 | 0 | 0 | 147 | 0 | 0 | clean | 0 / 0, 0, 0 |

The restart (stop 2.3 s, start 0.7 s, attached again after 3.1 s, fio kept running, rep 3 IOPS 15.3k against 22-29k in the other reps and
p99.9 10.3 s): the second process rebuilt its table from its first mark: 60 rebases, drift 0, first mark condemned after 0.3 s, table
`ready` after 35 s. This covers deltas before and during the first mark after a start under load, which the kill loops did not.

### 3. Wrong-proposal forecast

| cell | confirmed | unconfirmed_live | share | kept | listed | revived | would_propose |
|---|---|---|---|---|---|---|---|
| 1 | 3,519 | 0 | 0.000 % | 0 | 0 | 0 | 3,616 |
| 2 | 2,652 | 0 | 0.000 % | 0 | 74 | 0 | 2,775 |
| 3 | 2,845 | 0 | 0.000 % | 0 | 0 | 0 | 2,845 |
| 4 | 8,152 | 0 | 0.000 % | 0 | 0 | 0 | 8,158 |
| 5 | 2,302 | 0 | 0.000 % | 0 | 0 | 0 | 2,302 |
| 6 | 4,381 | 0 | 0.000 % | 0 | 43 | 0 | 4,427 |

The class that becomes a wrong proposal (live by id with no hinting entry) never occurred. `_listed` at 16K (2.8 % of confirmed) and in the
discard cell (1.0 %) is not near zero as the review expected; it is segments already listed for condemnation, harmless in shadow, but the
active stage should be checked for it.

### 4. Recall

| cell | confirmed | early by mark | missed | recall (segments) | confirmed bytes | early bytes | missed bytes | recall (bytes) |
|---|---|---|---|---|---|---|---|---|
| 1 | 3,519 | 3,002 | 43 | 53.6 % | 53.8 GiB | 46.6 GiB | 579 MiB | 53.3 % |
| 2 | 2,652 | 3,556 | 83 | 42.2 % | 40.3 GiB | 54.7 GiB | 1.3 GiB | 41.9 % |
| 3 | 2,845 | 3,891 | 1 | 42.2 % | 43.3 GiB | 58.9 GiB | 65 KiB | 42.4 % |
| 4 | 8,152 | 58 | 107 | 98.0 % | 113.8 GiB | 849 MiB | 1.5 GiB | 98.0 % |
| 5 | 2,302 | 5,128 | 26 | 30.9 % | 35.1 GiB | 78.9 GiB | 347 MiB | 30.7 % |
| 6 | 4,381 | 4,338 | 67 | 49.9 % | 65.7 GiB | 67.3 GiB | 956 MiB | 49.1 % |

(Byte totals count every repetition including the many compaction outputs; they are cumulative over a block.)

### 5. Gain

| cell | confirmed with lead | without lead | mean lead ms | max lead ms | lead byte-ms (GiB x s) | offered idle / busy | idle wait mean / max ms (n) | revived |
|---|---|---|---|---|---|---|---|---|
| 1 | 3,466 | 53 | 3,621 | 21,083 | 191 | 2,848 / 768 | 836 / 3,082 (715) | 0 |
| 2 | 2,473 | 179 | 2,792 | 19,401 | 104 | 1,630 / 1,145 | 1,082 / 3,908 (966) | 0 |
| 3 | 2,845 | 0 | 2,165 | 13,527 | 93 | 2,137 / 708 | 1,446 / 2,950 (708) | 0 |
| 4 | 8,152 | 0 | 2,564 | 21,885 | 291 | 7,964 / 194 | 830 / 2,162 (188) | 0 |
| 5 | 2,302 | 0 | 3,426 | 20,971 | 119 | 2,034 / 268 | 320 / 988 (268) | 0 |
| 6 | 4,381 | 0 | 3,885 | 17,883 | 253 | 4,154 / 273 | 354 / 597 (270) | 0 |

The gain is real but modest: segments freed 2-4 s earlier on average, up to 22 s. Nothing revived (no proposal condemn-on-cut would have had to withdraw).

### 6. Rep 1: first offer against the first mark condemn

| cell | first_ready_ms | first_mark_condemn_ms |
|---|---|---|
| 1 | 9,243 | 30,327 (after the restart: 35,428 / 300) |
| 2 | 11,781 | 31,183 |
| 3 | 16,850 | 30,377 |
| 4 | 8,472 | 30,358 |
| 5 | 9,346 | 30,318 |
| 6 | 12,426 | 30,309 |

The table is ready 8-17 s after attach, the first mark that condemns comes at about 30 s (the grace): the first 13-22 s of a fresh
attach are where an active table could act first.

### 7 and 8. Bounded state and time not Ready

| cell | segments min / median / max / last | zero_waiting max (1st half / 2nd half / last quarter) | idle_waiting max | samples not Ready | passes (step / urgent / requested) | status probe median / p99 / max ms |
|---|---|---|---|---|---|---|
| 1 | 101 / 193 / 257 / 138 | 176 (164 / 176 / 176) | 62 | 0 of 68 | 104 / 4 / 0 | 4 / 6 / 247 (2 failed during the restart) |
| 2 | 117 / 221 / 306 / 148 | 154 (125 / 154 / 154) | 78 | 0 of 67 | 105 / 0 / 0 | 4 / 5 / 6 |
| 3 | 101 / 202 / 514 / 128 | 233 (233 / 188 / 152) | 209 | 0 of 67 | 127 / 0 / 0 | 4 / 7 / 10 |
| 4 | 50 / 62 / 73 / 56 | 30 (30 / 27 / 27) | 37 | 0 of 67 | 134 / 0 / 0 | 4 / 7 / 9 |
| 5 | 182 / 290 / 551 / 280 | 256 (256 / 180 / 102) | 12 | 0 of 81 | 134 / 3 / 0 | 5 / 6 / 8 |
| 6 | 67 / 204 / 324 / 146 | 218 (218 / 216 / 216) | 101 | 0 of 82 | 140 / 4 / 0 | 4 / 6 / 6 |

All three are bounded (second half never above first half by more than the run-to-run spread; last quarter at or below the maximum), the
table was `ready` in every sample of every cell (0 s not Ready, apart from the restart's own rebuild), no requested passes, 0 to 4 urgent
passes per block. Per-phase maxima and fio IOPS per repetition are in `tables.md`.

**`ready()` during the discard cell**: its coordinator time is not exported. Proxies in the 60 s after each of the five 6 GiB discards:
timed `lunix status` median 4 ms, max 6 ms; `lunix gc --status` call max 4-6 ms; `zero_waiting` max 145-218. No stall visible, which at
this size (hundreds of segments) is what the engine cost test predicts (`ready()` 0.2 ms at 64k waiting, 8.1 ms at 500k).

### 9. Errors

`fsck --deep` clean on all 6 cell volumes and all 8 cost volumes; 0 fio errors; 0 EIO/ENOSPC log lines; throttle `fulls` and `enospc`
0 in the final status of every cell.

## Cost

### Cut time per journal byte, shadow against off (quiet host, 3 repetitions per block, 2 blocks per arm in ABBA order, w4k)

Cut ms per MiB of journal: `last_cut_ms / last_cut_bytes` of the daemon's status, sampled every 2 s (about 70 distinct cuts per block).

| chunk | off (blocks) | shadow (blocks) | shadow vs off, byte-weighted | median per cut | reps 2-3 only | gate |
|---|---|---|---|---|---|---|
| 16K | 1.683 (1.699, 1.668) | 1.798 (1.812, 1.785) | **+6.8 %** | +16.8 % | +3.7 % | **not met** |
| 64K | 2.124 (2.172, 2.080) | 2.157 (2.162, 2.152) | **+1.6 %** | +9.0 % | +3.3 % | met byte-weighted, reps 2-3 marginal |

Both arms agree between their two blocks to 1-4 %, so +6.8 % at 16K is probably not noise, but the first repetition of shadow block 1
(2.243) pulls it up and the per-repetition spread within one arm reaches 15 % (64K off: 1.72 to 2.94). The engine in-process measurement
agrees with the sign: `map_ns` per chunk goes from 59 to 178 ns at 16K and from 49 to 148 ns at 64K (+200 %, +120 and +100 ns per chunk,
warm leaves): about 1.0 ms of a 16K cut with 8,192 chunks and 0.3 ms of a 64K cut with 2,048, which is under 1 % of a 150-300 ms cut; the
+6.8 % is therefore not explained by `map_ns` alone and needs more blocks (the review asked for at least 4 per arm) before being called
real. Cold leaves (a volume above the leaf cache) were not measured.

### mark_ms with and without counting (the `gc pass done` lines of the same blocks; `tip_refs` on only in shadow)

| chunk | off median / mean (n) | shadow median / mean (n) | mark phase ms per unit off / shadow |
|---|---|---|---|
| 16K | 394 / 391 (101) | 398 / 392 (98) | 11.62 / 11.69 (+0.6 %) |
| 64K | 83 / 79.6 (129) | 94 / 90.3 (133) | 5.33 / 6.04 (**+13 %**) |

Counting adds nothing visible at 16K and about 11 ms (+13 %) per mark at 64K, where the mark is short.

### Table costs (engine cost tests on the host, release, quiet)

| segments (waiting) | mark-start snapshot | `ready()` | `rebase` |
|---|---|---|---|
| 16k (0) | 0.02 ms | 0.000 ms | 1.7 ms |
| 128k (0 / 64k) | 0.22 ms | 0.000 / 0.225 ms | 15.9 / 7.8 ms |
| 1M (0 / 500k) | 10.6 ms | 0.000 / 8.1 ms | 360 / 166 ms |

The daemon cells never held more than 551 segments, so none of these costs was reachable in the real runs.

## Deviations and remarks

1. **Restart**: the harness's clean stop is `stop_lunixd` (`kill -TERM` of the daemon from the script, which makes the daemon quiesce every
   device and exit through its normal shutdown path: "quiesced; exiting without a stop command"), followed by a start that recovers the
   attached device. The daemon has no ctl shutdown. Nothing was signalled by hand; no gdb. Only cell 1 has it.
2. **Dedup cell uses 64K blocks** (not the 4K of the others), because 4K writes cannot produce equal 64K chunks; its IOPS are not comparable.
3. **Cut time is sampled** (every 2 s, last cut only), so it is a sample of cuts, not all of them; `map_ns` per chunk and `ready()`
   time are not exported by the daemon and come from the engine's ignored cost tests (`stack_refs_cost`, two of the three). The third,
   the in-process cut-cost test, was started first and stopped by hand after 55 min with no output (its default knobs do not fit a slot);
   it is replaced by the real-daemon comparison.
4. **Cost check size**: 3 repetitions per block, 2 blocks per arm and chunk (6 per arm), not the 4 blocks the review asked for.
5. The 5-minute load wait took about 25-30 minutes after each block (blocked daemon threads and fio count in the load average), so blocks
   are 31 minutes apart; the 8 cost blocks started with 5-minute load 1.96-1.99, the 6 cell blocks 1.9-2.0 (rule: below 2).
6. The first launch of the detached driver through `remote.sh root` left a driver running although its log was empty; a second was
   started by hand and the first (still in its load wait, no block begun) was stopped, so one driver ran all blocks. No block was affected.
7. `LUNIX_M=refs2` was reused as told; its scratch directory on the host is removed. Short smoke runs (15 s cells) of cells 1, 4, 5, 6 on
   the same build preceded the soak and are not part of the results (their output was removed).
8. Result files were scrubbed of the remote home and user name (paths replaced by `<spike-dir>`, `<tree>`, `<home>`); no hostinfo files.
9. The discard cell's volume is small (at most 551 segments); a discard cell that stresses `ready()` needs a much larger volume.

## Clean-up

Remote tree, results copy, scratch directory removed, tmpfs unmounted, no daemon, fio or ublk device left (`/dev/ublk-control` and the
`ublk_drv` module were there before). No rust-skills conflicts (no Rust written).
