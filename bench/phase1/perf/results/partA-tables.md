# M16 Part A

## chunk 16 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 234,711 (212,125-235,547) | 0.96 | 28,705 (31,326) | 15.8 |  | 793.8 | 793.8 |
| r4k | 8 | 3 | 305,178 (268,610-305,460) | 1.25 | 18,481 (20,578) | 17.7 |  | 1,362 | 1,362 |
| r4k-verifyreads | 4 | 1 | 205,189 (205,189-205,189) | 0.84 | 32,375 (32,375) | 18.8 |  | 792.0 | 1,195 |
| r4k-verifyreads | 8 | 1 | 273,048 (273,048-273,048) | 1.12 | 20,316 (20,316) | 21.6 |  | 1,572 | 1,833 |
| seqr | 4 | 3 | 3,688 (3,343-3,795) | 3.88 | 73,925 (103,285) | 1,018 |  | 976.8 | 1,192 |
| seqr | 8 | 3 | 3,791 (3,424-3,861) | 3.98 | 70,779 (106,430) | 1,025 |  | 1,561 | 1,833 |
| seqr-verifyreads | 4 | 1 | 3,087 (3,087-3,087) | 3.25 | 110,625 (110,625) | 1,234 |  | 913.7 | 1,195 |
| seqr-verifyreads | 8 | 1 | 3,299 (3,299-3,299) | 3.47 | 104,333 (104,333) | 1,221 |  | 1,543 | 1,934 |
| seqw | 4 | 3 | 28.5 (28.2-29.4) | 0.04 | 15,367,930 (15,770,583) | 2,942 |  | 808.5 | 1,026 |
| seqw | 8 | 3 | 28.1 (26.4-28.3) | 0.04 | 17,112,760 (17,112,760) | 3,331 |  | 1,382 | 1,627 |
| w4k | 4 | 3 | 2,445 (1,737-2,539) | 0.01 | 17,112,760 (17,112,760) | 46.9 | NOISY | 723.6 | 810.2 |
| w4k | 8 | 3 | 2,890 (2,596-3,041) | 0.01 | 17,112,760 (17,112,760) | 41.5 |  | 1,293 | 1,385 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1168 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 8 | 305,178 | 18,481 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| w4k | 300,000 IOPS | 8 | 2,890 | 17,112,760 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| seqw | 3.0 GB/s | 4 | 0.04 | 15,367,930 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| seqr | 5.0 GB/s | 8 | 3.98 | 70,779 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |

### verify_reads = true (cost, Spec gaps 10; one run each)

| cell | queues | off | on | on/off |
|---|---|---|---|---|
| r4k | 4 | 234,711 | 205,189 | 0.87 |
| r4k | 8 | 305,178 | 273,048 | 0.89 |
| seqr | 4 | 3.88 | 3.25 | 0.84 |
| seqr | 8 | 3.98 | 3.47 | 0.87 |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 402,653,184 | 134,217,728 |
| seqw | 8 | 536,870,912 | 402,653,184 |
| w4k | 4 | 268,435,456 | 134,217,728 |
| w4k | 8 | 671,088,640 | 402,653,184 |

#### shortfall evidence: r4k at 8 queues

- measured median 305,178 against 500,000.00: gap 194,822.22 (39 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- null_blk in this session: 6,858,214 IOPS, 28.09 GB/s
- daemon CPU per I/O: 17.7 us; thread split of the first repetition (seconds over the window): lunix-read-2=20.34(1) lunix-read-3=20.31(1) lunix-read-1=20.28(1) lunix-read-0=20.27(1) lunix-q5=19.32(1) lunix-q6=19.44(1) lunix-q7=19.42(1) lunix-q4=19.32(1) lunix-coord=0.22(1) 
- busiest thread group uses 0.68 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: w4k at 8 queues

- measured median 2,890 against 300,000.00: gap 297,110.35 (99 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- null_blk in this session: 1,439,767 IOPS, 5.90 GB/s
- journal file system in this session: iops4k 2,732 IOPS 11 MB/s, commit64k 263 IOPS 17 MB/s, commit4k 254 IOPS 1 MB/s
- daemon CPU per I/O: 41.5 us; thread split of the first repetition (seconds over the window): lunix-q5=0.25(1) lunix-q6=0.25(1) lunix-q7=0.24(1) lunix-q4=0.24(1) lunix-coord=1.94(1) 
- busiest thread group uses 0.02 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 4 queues

- measured median 0.04 against 3.00: gap 2.96 (99 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- null_blk in this session: 11,267 IOPS, 11.81 GB/s
- journal file system in this session: iops4k 2,732 IOPS 11 MB/s, commit64k 263 IOPS 17 MB/s, commit4k 254 IOPS 1 MB/s
- daemon CPU per I/O: 2,942 us; thread split of the first repetition (seconds over the window): lunix-q2=0.15(1) lunix-q3=0.16(1) lunix-coord=0.90(1) 
- busiest thread group uses 0.02 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqr at 8 queues

- measured median 3.98 against 5.00: gap 1.02 (20 % of the target)
- BASELINE ceiling: 64k random read touching every byte: 48.8 GB/s (copy), 81 GB/s (USER_COPY); 1M reads through the copy path 25 K IOPS (158 us per I/O) in the spike
- null_blk in this session: 9,643 IOPS, 10.11 GB/s
- daemon CPU per I/O: 1,025 us; thread split of the first repetition (seconds over the window): lunix-read-2=20.29(1) lunix-read-0=20.27(1) lunix-read-1=20.24(1) lunix-read-3=20.20(1) lunix-q4=12.28(1) lunix-q5=9.12(1) lunix-q6=7.06(1) lunix-q7=5.95(1) lunix-coord=0.88(1) 
- busiest thread group uses 0.65 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

## chunk 64 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| r4k | 4 | 3 | 262,617 (241,899-263,296) | 1.08 | 28,443 (29,491) | 22.0 |  | 765.3 | 791.2 |
| r4k | 8 | 3 | 259,806 (236,318-260,130) | 1.06 | 20,578 (21,103) | 25.4 |  | 1,345 | 1,352 |
| r4k-verifyreads | 4 | 1 | 184,704 (184,704-184,704) | 0.76 | 41,157 (41,157) | 28.8 |  | 770.5 | 1,146 |
| r4k-verifyreads | 8 | 1 | 178,539 (178,539-178,539) | 0.73 | 27,656 (27,656) | 33.4 |  | 1,423 | 1,735 |
| seqr | 4 | 3 | 8,022 (7,780-8,124) | 8.42 | 45,351 (50,070) | 586.2 |  | 878.8 | 1,146 |
| seqr | 8 | 3 | 7,192 (6,461-7,571) | 7.55 | 79,167 (96,993) | 916.4 |  | 1,463 | 1,735 |
| seqr-verifyreads | 4 | 1 | 7,696 (7,696-7,696) | 8.08 | 54,788 (54,788) | 699.8 |  | 811.3 | 1,146 |
| seqr-verifyreads | 8 | 1 | 6,265 (6,265-6,265) | 6.58 | 91,750 (91,750) | 1,118 |  | 1,602 | 1,735 |
| seqw | 4 | 3 | 29.9 (27.5-33.3) | 0.04 | 14,294,188 (14,294,188) | 2,151 | NOISY | 726.7 | 955.9 |
| seqw | 8 | 3 | 30.5 (30.2-30.5) | 0.04 | 17,112,760 (17,112,760) | 2,165 |  | 1,321 | 1,573 |
| w4k | 4 | 3 | 3,762 (3,762-3,762) | 0.02 | 17,112,760 (17,112,760) | 40.2 |  | 690.9 | 791.2 |
| w4k | 8 | 3 | 2,073 (1,701-2,426) | 0.01 | 17,112,760 (17,112,760) | 84.1 | NOISY | 1,230 | 1,358 |

Idle resident set after attach: 4 queues 556 MiB, 8 queues 1127 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | 500,000 IOPS, p99 <= 150 us | 4 | 262,617 | 28,443 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| w4k | 300,000 IOPS | 4 | 3,762 | 17,112,760 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| seqw | 3.0 GB/s | 8 | 0.04 | 17,112,760 | SHORTFALL | 8 queue threads + 8 fio jobs = 16 on 16 CPUs |  |
| seqr | 5.0 GB/s | 4 | 8.42 | 45,351 | pass | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |

### verify_reads = true (cost, Spec gaps 10; one run each)

| cell | queues | off | on | on/off |
|---|---|---|---|---|
| r4k | 4 | 262,617 | 184,704 | 0.70 |
| r4k | 8 | 259,806 | 178,539 | 0.69 |
| seqr | 4 | 8.42 | 8.08 | 0.96 |
| seqr | 8 | 7.55 | 6.58 | 0.87 |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 402,653,184 | 134,217,728 |
| seqw | 8 | 603,979,776 | 402,653,184 |
| w4k | 4 | 402,653,184 | 134,217,728 |
| w4k | 8 | 671,088,640 | 402,653,184 |

#### shortfall evidence: r4k at 4 queues

- measured median 262,617 against 500,000.00: gap 237,383.38 (47 % of the target)
- BASELINE ceiling: ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)
- null_blk in this session: 6,858,214 IOPS, 28.09 GB/s
- daemon CPU per I/O: 22.0 us; thread split of the first repetition (seconds over the window): lunix-read-1=27.84(1) lunix-read-3=27.81(1) lunix-read-2=27.82(1) lunix-read-0=27.75(1) lunix-q3=26.62(1) lunix-q2=26.69(1) lunix-coord=0.07(1) 
- busiest thread group uses 0.90 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: w4k at 4 queues

- measured median 3,762 against 300,000.00: gap 296,238.00 (99 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- null_blk in this session: 1,439,767 IOPS, 5.90 GB/s
- journal file system in this session: iops4k 2,732 IOPS 11 MB/s, commit64k 263 IOPS 17 MB/s, commit4k 254 IOPS 1 MB/s
- daemon CPU per I/O: 40.2 us; thread split of the first repetition (seconds over the window): lunix-q3=0.57(1) lunix-q2=0.59(1) lunix-coord=2.99(1) 
- busiest thread group uses 0.02 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 8 queues

- measured median 0.04 against 3.00: gap 2.96 (99 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- null_blk in this session: 11,267 IOPS, 11.81 GB/s
- journal file system in this session: iops4k 2,732 IOPS 11 MB/s, commit64k 263 IOPS 17 MB/s, commit4k 254 IOPS 1 MB/s
- daemon CPU per I/O: 2,165 us; thread split of the first repetition (seconds over the window): lunix-q4=0.09(1) lunix-q5=0.09(1) lunix-q6=0.10(1) lunix-q7=0.09(1) lunix-coord=0.48(1) 
- busiest thread group uses 0.01 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/
