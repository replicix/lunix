# M16 Part A

## chunk 16 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 370,991 (368,161-371,383) | 1.52 | 19,005 (19,268) | 5.3 |  | 2,661 | 2,661 |
| r4k | 8 | 3 | 773,248 (770,461-773,292) | 3.17 | 5,341 (5,341) | 4.4 |  | 3,280 | 3,280 |
| seqr | 4 | 3 | 7,683 (7,680-7,712) | 8.07 | 35,914 (35,914) | 261.3 |  | 3,033 | 3,230 |
| seqr | 8 | 3 | 10,024 (9,934-10,473) | 10.52 | 48,497 (48,497) | 344.8 |  | 3,726 | 3,726 |
| seqw | 4 | 3 | 301.1 (293.0-316.1) | 0.32 | 10,536,092 (14,696,841) | 4,160 |  | 956.7 | 2,800 |
| seqw | 8 | 3 | 233.7 (215.2-262.8) | 0.25 | 11,475,616 (14,831,059) | 5,075 | NOISY | 1,848 | 3,398 |
| w4k | 4 | 3 | 12,810 (11,922-16,154) | 0.05 | 968,884 (985,661) | 70.0 | NOISY | 818.8 | 2,800 |
| w4k | 8 | 3 | 12,980 (12,354-14,759) | 0.05 | 1,233,125 (1,283,457) | 77.8 |  | 1,519 | 3,398 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1180 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 8 | 773,248 | 5,341 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| w4k | 300,000 IOPS | 8 | 12,980 | 1,233,125 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| seqw | 3.0 GB/s | 4 | 0.32 | 10,536,092 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| seqr | 5.0 GB/s | 8 | 10.52 | 48,497 | pass | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 402,653,184 | 134,217,728 |
| seqw | 8 | 469,762,048 | 402,653,184 |
| w4k | 4 | 268,435,456 | 134,217,728 |
| w4k | 8 | 671,088,640 | 402,653,184 |

#### shortfall evidence: r4k at 8 queues

- measured median 773,248 against 500,000.00: gap -273,248.45 (-55 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- daemon CPU per I/O: 4.4 us; thread split of the first repetition (seconds over the window): lunix-q6=26.05(1) lunix-q4=26.18(1) lunix-q7=26.07(1) lunix-q5=26.04(1) lunix-read-1=0.10(1) lunix-read-2=0.11(1) lunix-read-0=0.10(1) lunix-read-3=0.11(1) 
- busiest thread group uses 0.84 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: w4k at 8 queues

- measured median 12,980 against 300,000.00: gap 287,019.93 (96 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- daemon CPU per I/O: 77.8 us; thread split of the first repetition (seconds over the window): lunix-q6=1.28(1) lunix-q4=1.27(1) lunix-q7=1.28(1) lunix-q5=1.27(1) lunix-space=38.63(1) lunix-coord=7.06(1) lunixd-trim=0.11(1) 
- busiest thread group uses 0.58 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 4 queues

- measured median 0.32 against 3.00: gap 2.68 (89 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- daemon CPU per I/O: 4,160 us; thread split of the first repetition (seconds over the window): lunix-q2=1.91(1) lunix-q3=1.99(1) lunix-space=11.94(1) lunix-coord=6.26(1) iou-wrk-2714882=0.10(1) iou-wrk-2714883=0.09(1) 
- busiest thread group uses 0.39 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

## chunk 64 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 380,367 (380,209-380,381) | 1.56 | 18,743 (18,743) | 5.3 |  | 2,650 | 2,650 |
| r4k | 8 | 3 | 894,254 (883,388-908,741) | 3.66 | 4,424 (4,555) | 4.1 |  | 3,202 | 3,202 |
| seqr | 4 | 3 | 8,873 (8,849-8,915) | 9.31 | 30,540 (30,802) | 220.7 |  | 2,901 | 2,977 |
| seqr | 8 | 3 | 10,908 (10,739-11,368) | 11.45 | 36,962 (37,487) | 306.6 |  | 3,379 | 3,479 |
| seqw | 4 | 3 | 366.7 (302.2-375.8) | 0.39 | 14,159,970 (17,112,760) | 1,926 | NOISY | 817.5 | 2,698 |
| seqw | 8 | 3 | 326.9 (283.2-331.5) | 0.35 | 17,112,760 (17,112,760) | 2,257 |  | 1,290 | 3,251 |
| w4k | 4 | 3 | 6,063 (5,168-9,526) | 0.02 | 750,780 (1,702,887) | 57.8 | NOISY | 634.6 | 2,698 |
| w4k | 8 | 3 | 5,712 (5,484-6,032) | 0.02 | 692,060 (2,600,468) | 68.3 |  | 1,191 | 3,251 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1121 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 8 | 894,254 | 4,424 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| w4k | 300,000 IOPS | 4 | 6,063 | 750,780 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs | NOISY |
| seqw | 3.0 GB/s | 4 | 0.39 | 14,159,970 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs | NOISY |
| seqr | 5.0 GB/s | 8 | 11.45 | 36,962 | pass | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 134,217,728 | 134,217,728 |
| seqw | 8 | 671,088,640 | 402,653,184 |
| w4k | 4 | 268,435,456 | 134,217,728 |
| w4k | 8 | 671,088,640 | 402,653,184 |

#### shortfall evidence: r4k at 8 queues

- measured median 894,254 against 500,000.00: gap -394,253.72 (-79 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- daemon CPU per I/O: 4.1 us; thread split of the first repetition (seconds over the window): lunix-q6=27.69(1) lunix-q4=27.78(1) lunix-q5=27.70(1) lunix-q7=27.62(1) 
- busiest thread group uses 0.93 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: w4k at 4 queues

- measured median 6,063 against 300,000.00: gap 293,936.55 (98 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- daemon CPU per I/O: 57.8 us; thread split of the first repetition (seconds over the window): lunix-q2=1.20(1) lunix-q3=1.19(1) lunix-space=10.72(1) lunix-coord=3.34(1) lunixd-trim=0.10(1) 
- busiest thread group uses 0.11 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 4 queues

- measured median 0.39 against 3.00: gap 2.61 (87 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- daemon CPU per I/O: 1,926 us; thread split of the first repetition (seconds over the window): lunix-q2=2.62(1) lunix-q3=2.70(1) lunix-space=5.19(1) lunix-coord=4.52(1) iou-wrk-2682846=0.13(1) iou-wrk-2682845=0.12(1) 
- busiest thread group uses 0.13 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/
