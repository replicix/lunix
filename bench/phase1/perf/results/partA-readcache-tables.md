# M16 Part A

## chunk 16 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 353,272 (154,876-355,429) | 1.45 | 20,316 (80,216) | 5.7 | NOISY | 4,811 | 4,811 |
| r4k | 8 | 3 | 771,691 (323,952-774,155) | 3.16 | 5,472 (42,729) | 4.7 | NOISY | 5,354 | 5,354 |
| seqr | 4 | 3 | 7,568 (7,562-7,673) | 7.94 | 34,865 (34,865) | 260.9 |  | 4,811 | 4,811 |
| seqr | 8 | 3 | 9,159 (9,111-9,184) | 9.61 | 53,215 (58,982) | 361.6 |  | 5,356 | 5,356 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1128 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 8 | 771,691 | 5,472 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs | NOISY |
| w4k | | | not run | | | | |
| seqw | | | not run | | | | |
| seqr | 5.0 GB/s | 8 | 9.61 | 53,215 | pass | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |

#### shortfall evidence: r4k at 8 queues

- measured median 771,691 against 500,000.00: gap -271,690.52 (-54 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- daemon CPU per I/O: 4.7 us; thread split of the first repetition (seconds over the window): lunix-q4=12.30(1) lunix-q5=12.35(1) lunix-q7=12.67(1) lunix-q6=12.29(1) lunix-read-2=4.82(1) lunix-read-0=4.74(1) lunix-read-1=4.62(1) lunix-read-3=4.60(1) lunix-coord=0.26(1) 
- busiest thread group uses 0.42 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

## chunk 64 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 373,047 (318,087-373,179) | 1.53 | 18,743 (26,345) | 5.4 |  | 4,735 | 4,735 |
| r4k | 8 | 3 | 831,952 (583,372-834,622) | 3.41 | 4,948 (16,712) | 4.3 | NOISY | 5,279 | 5,279 |
| seqr | 4 | 3 | 9,142 (9,118-9,203) | 9.59 | 28,967 (29,491) | 218.4 |  | 4,736 | 4,736 |
| seqr | 8 | 3 | 11,051 (10,748-11,390) | 11.60 | 44,302 (44,827) | 318.3 |  | 5,280 | 5,280 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1122 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 8 | 831,952 | 4,948 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs | NOISY |
| w4k | | | not run | | | | |
| seqw | | | not run | | | | |
| seqr | 5.0 GB/s | 8 | 11.60 | 44,302 | pass | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |

#### shortfall evidence: r4k at 8 queues

- measured median 831,952 against 500,000.00: gap -331,952.17 (-66 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- daemon CPU per I/O: 4.3 us; thread split of the first repetition (seconds over the window): lunix-q4=22.56(1) lunix-q5=22.50(1) lunix-q6=22.43(1) lunix-q7=20.58(1) lunix-coord=0.06(1) 
- busiest thread group uses 0.75 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/
