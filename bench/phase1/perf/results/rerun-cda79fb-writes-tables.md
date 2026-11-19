# M16 Part A

## chunk 16 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| seqw | 4 | 3 | 28.1 (25.3-28.3) | 0.04 | 15,904,801 (17,112,760) | 2,843 |  | 878.4 | 1,071 |
| w4k | 4 | 3 | 3,208 (2,441-3,976) | 0.01 | 12,834,570 (17,112,760) | 39.9 | NOISY | 777.9 | 789.5 |

Idle resident set after attach: 4 queues 556 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | | | not run | | | | |
| w4k | 300,000 IOPS | 4 | 3,208 | 12,834,570 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs | NOISY |
| seqw | 3.0 GB/s | 4 | 0.04 | 15,904,801 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| seqr | | | not run | | | | |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 536,870,912 | 134,217,728 |
| w4k | 4 | 268,435,456 | 134,217,728 |

#### shortfall evidence: w4k at 4 queues

- measured median 3,208 against 300,000.00: gap 296,791.51 (99 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- daemon CPU per I/O: 39.9 us; thread split of the first repetition (seconds over the window): lunix-coord=3.71(1) lunix-q3=0.58(1) lunix-q2=0.59(1) 
- busiest thread group uses 0.03 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 4 queues

- measured median 0.04 against 3.00: gap 2.96 (99 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- daemon CPU per I/O: 2,843 us; thread split of the first repetition (seconds over the window): lunix-coord=1.65(1) lunix-q3=0.16(1) lunix-q2=0.16(1) 
- busiest thread group uses 0.03 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

## chunk 64 KiB

| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |
|---|---|---|---|---|---|---|---|---|---|
| seqw | 4 | 3 | 32.5 (32.5-32.7) | 0.04 | 12,146,704 (12,280,922) | 1,956 |  | 734.7 | 939.7 |
| w4k | 4 | 3 | 2,420 (2,284-2,867) | 0.01 | 17,112,760 (17,112,760) | 61.4 | NOISY | 723.8 | 733.9 |

Idle resident set after attach: 4 queues 556 MiB

### verdicts (DESIGN §20), judged at the better queue count

| row | target | queues judged | median | p99 us | verdict | oversubscription | note |
|---|---|---|---|---|---|---|---|
| r4k | | | not run | | | | |
| w4k | 300,000 IOPS | 4 | 2,420 | 17,112,760 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs | NOISY |
| seqw | 3.0 GB/s | 4 | 0.04 | 12,146,704 | SHORTFALL | 4 queue threads + 8 fio jobs = 12 on 16 CPUs |  |
| seqr | | | not run | | | | |

### destage backlog at the end of the write cells (journal bytes in use)

| cell | queues | at the end of the cell (median) | 10 s later (median) |
|---|---|---|---|
| seqw | 4 | 536,870,912 | 134,217,728 |
| w4k | 4 | 268,435,456 | 134,217,728 |

#### shortfall evidence: w4k at 4 queues

- measured median 2,420 against 300,000.00: gap 297,580.31 (99 % of the target)
- BASELINE ceiling: ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)
- daemon CPU per I/O: 61.4 us; thread split of the first repetition (seconds over the window): lunix-coord=2.64(1) lunix-q2=0.45(1) lunix-q3=0.47(1) 
- busiest thread group uses 0.03 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/

#### shortfall evidence: seqw at 4 queues

- measured median 0.04 against 3.00: gap 2.96 (99 % of the target)
- BASELINE ceiling: hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike
- daemon CPU per I/O: 1,956 us; thread split of the first repetition (seconds over the window): lunix-coord=0.51(1) lunix-q2=0.17(1) lunix-q3=0.17(1) 
- busiest thread group uses 0.01 cores per thread (>= 0.9 means a saturated thread)
- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/
