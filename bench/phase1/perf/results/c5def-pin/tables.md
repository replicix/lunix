# c5def tables

Arms: ON / OFF = `gc_condemn_on_cut` on / off with `throttle_pace_store_bytes = true`; ONS / OFFS = the same with that key at its default. Every arm: grace 2000 ms and fast recheck on (the defaults), `log_level = debug`. Rep 1 is the cold run after the fill, reported apart.

## Blocks

| block | arm | chunk | kind | load 1/5/15 at start | reps | result | fsck |
|---|---|---|---|---|---|---|---|
| b01-ON-65536-pin | ON | 64K | pin | 1.39/0.71/0.42 | 5 | 0 | 0 |
| b02-OFF-65536-pin | OFF | 64K | pin | 116.06/253.11/138.66 | 5 | 0 | 0 |

## A. Safety, every block

| block | fio errors | verify | fsck | ref-table lines | drift | mismatches | rebases | rejected early | named | propose errors | uncond / condemned (lines) | hold footer errors | exhausted | auth failures | min pgrace wait ms | EIO/ENOSPC | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-ON-65536-pin | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 129 | 0 | 0 | 0 | 0 / 6491 (0) | 0 | 0 | 0 | 2036 | 0 | PASS |
| b02-OFF-65536-pin | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 / 0 (0) | 0 | 0 | 0 | 2009 | 0 | PASS |

## B. Rep 1 (cold run after the fill)

| block | arm | chunk | first delete after onset s | stops | longest wait s | throttled s | GiB deleted <= 5 s | first delete <= 3 s | 0 stops | wait <= 5 s | throttled <= 22 s | >= 6 GiB | all |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|

## C. Cut cost per journal byte, ON against OFF

| cell | arm | windows | cuts | mean cut MiB | mean cut ms | us / MiB (plain, +- se) | us / MiB at pooled size | vs OFF at pooled size | OFF se % | map ns / chunk |
|---|---|---|---|---|---|---|---|---|---|---|

## D. Steady state, reps 2-5, ON against OFF, pooled over blocks

| cell | metric | OFF | ON | limit | verdict |
|---|---|---|---|---|---|

## E. Proposal cost, ON blocks

| block | arm | reps | max propose_us_sum / wall % | mean us per call | propose_us_max us | orphan deferrals | cuts requested | verdict |
|---|---|---|---|---|---|---|---|---|

## F. Pin cell (pin held through reps 1-3, removed before rep 4; plain volume), corrected check

While pinned: pin_blocked > 0, nothing offered or condemned, `zero_waiting` <= the segment files in the store at that sample (`nseg` of series.jsonl: a listing of the volume's segment directory every 2 s), and the per-rep peak of `zero_waiting` levels off over reps 1-3 (fails if every pinned rep's peak is more than 10 % above the one before). After the release: condemned > 0 in rep 5, `zero_waiting` below 64 within 30 s.

Block b01-ON-65536-pin (ON): pin released at 1795853199140.

| rep | pin_blocked delta | proposals_offered | proposal_condemned | zero_waiting peak | segment files (max) | samples with zero_waiting above the file count | refs.segments (end) | IOPS | stops |
|---|---|---|---|---|---|---|---|---|---|
| 1 (pinned) | 365 | 0 | 0 | 425 | 1035 | 0 | 237 | 64,715 | 0 |
| 2 (pinned) | 342 | 0 | 0 | 484 | 1050 | 0 | 193 | 61,531 | 0 |
| 3 (pinned) | 345 | 0 | 0 | 524 | 981 | 0 | 239 | 60,706 | 0 |
| 4 | 5 | 2690 | 2690 | 256 | 958 | 0 | 179 | 60,171 | 0 |
| 5 | 0 | 2634 | 2634 | 242 | 875 | 0 | 172 | 60,968 | 0 |

zero_waiting peaks over pinned reps 1-3: [425, 484, 524]; levels off
zero_waiting below 64 again 9.2 s after the release (limit 30 s); orphan_deferrals 0, hold_marks 0

Block b02-OFF-65536-pin (OFF): pin released at 1795853637839.

| rep | pin_blocked delta | proposals_offered | proposal_condemned | zero_waiting peak | segment files (max) | samples with zero_waiting above the file count | refs.segments (end) | IOPS | stops |
|---|---|---|---|---|---|---|---|---|---|
| 1 (pinned) | 0 | 0 | 0 | 0 | None | 0 | 0 | 61,196 | 0 |
| 2 (pinned) | 0 | 0 | 0 | 0 | None | 0 | 0 | 64,273 | 0 |
| 3 (pinned) | 0 | 0 | 0 | 0 | None | 0 | 0 | 57,499 | 0 |
| 4 | 0 | 0 | 0 | 0 | None | 0 | 0 | 59,467 | 0 |
| 5 | 0 | 0 | 0 | 0 | None | 0 | 0 | 64,317 | 0 |


## Shipping defaults and restart cells, reported

| block | arm | kind | rep 1: first delete s | stops | wait s | throttled s | GiB <= 5 s | rep 1 IOPS | reps 2-5 IOPS (med) | p99.9 s (med) | stops 2-5 | all rep-1 clauses |
|---|---|---|---|---|---|---|---|---|---|---|---|---|

## Clause summary

| clause | verdict |
|---|---|
| A | PASS |
| B | NOT RUN |
| C | NOT RUN |
| D | NOT RUN |
| E | NOT RUN |
| F | PASS |
