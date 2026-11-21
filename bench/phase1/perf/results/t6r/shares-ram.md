## ram partA_c64k_q4_w4k_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | destage_worker_cpu | futex_wait | journal_flush | other | queue_io |
|---|---|---|---|---|---|---|
| destage_workers | 7.85 |  | 0.02 |  | 7.82 |  |
| coordinator | 1.84 |  | 0.02 |  | 1.82 |  |
| lunix-q3 | 0.96 |  | 0.01 |  | 0.86 | 0.09 |
| lunix-q2 | 0.96 | 0.00 | 0.01 | 0.00 | 0.85 | 0.09 |
| iou-wrk-241888 | 0.08 |  |  |  | 0.08 |  |
| lunix-space-tim | 0.08 |  | 0.01 |  | 0.07 |  |
| iou-wrk-241889 | 0.07 |  |  |  | 0.07 |  |

Coordinator thread, share of its on-CPU time (1.84 s of a 8.0 s window): other 99 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_cpu | destage_worker_other | futex_wait | journal_flush | other | queue_io |
|---|---|---|---|---|---|---|---|
| lunixd-ctl | 10.04 |  |  | 5.02 |  | 5.02 |  |
| lunix-ring-time | 5.90 |  |  | 5.90 |  |  |  |
| lunix-space-tim | 5.68 |  |  | 5.68 |  |  |  |
| lunix-q2 | 5.63 |  |  | 0.00 |  |  | 5.62 |
| lunix-q3 | 5.62 |  |  | 0.00 |  |  | 5.62 |
| coordinator | 5.56 |  | 0.00 | 5.56 | 0.00 |  | 0.00 |
| lunix-space-lad | 5.15 |  |  | 5.15 |  |  |  |
| lunixd | 5.02 |  |  | 5.02 |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  | 4.00 |  |
| lunix-q1 | 1.94 |  |  |  |  |  | 1.94 |
| lunix-q0 | 1.94 |  |  |  |  |  | 1.94 |
| destage_workers | 0.35 | 0.00 | 0.35 | 0.00 | 0.00 | 0.00 |  |

Coordinator thread, share of its blocked time (5.56 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/writes-ram/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (58.5 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 24 | 6.42 | 11.0 % | 268 | 533 |
| finish | 24 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 702 | 0.19 | 0.3 % | 0 | 1 |
| cwseal | 759 | 0.20 | 0.3 % | 0 | 1 |
| reclaim | 40 | 0.32 | 0.5 % | 8 | 31 |
| zero | 74 | 0.29 | 0.5 % | 4 | 7 |
| gcurgent | 11 | 1.11 | 1.9 % | 101 | 172 |
| runurgent | 29 | 1.46 | 2.5 % | 50 | 429 |
| trueup | 24 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 24; in cut_and_commit 6.42 s = 11 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 5.42 s = 84 % of the cuts, C5-C9 1.01 s = 16 % of the cuts (1.7 % of the window).
Reclaim (C9) 0.32 s, of which the zero rewrite 0.29 s = 5 % of the cuts (0.5 % of the window).

## ram partA_c64k_q4_seqw_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 7.19 | 0.01 | 0.10 | 0.00 |  | 0.03 | 0.01 | 7.04 |  |  |  |
| coordinator | 2.72 | 0.01 | 0.03 | 0.00 | 0.00 | 0.01 |  | 2.67 | 0.00 | 0.00 |  |
| lunix-q2 | 1.28 |  | 0.01 |  |  | 0.01 | 0.01 | 0.91 |  |  | 0.34 |
| lunix-q3 | 1.17 |  | 0.01 |  |  | 0.00 | 0.01 | 0.89 |  |  | 0.24 |
| iou-wrk-241888 | 0.29 |  |  |  |  |  |  | 0.29 |  |  |  |
| iou-wrk-241889 | 0.24 |  |  |  |  |  |  | 0.24 |  |  |  |

Coordinator thread, share of its on-CPU time (2.72 s of a 8.0 s window): other 98 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_cpu | destage_worker_other | futex_wait | other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|
| lunixd-ctl | 10.04 |  |  | 5.02 | 5.02 |  |  |
| lunix-space-tim | 5.98 |  |  | 5.98 |  |  |  |
| lunix-ring-time | 5.90 |  |  | 5.90 |  |  |  |
| lunix-q2 | 5.86 |  |  |  |  |  | 5.86 |
| lunix-q3 | 5.85 |  |  |  |  |  | 5.85 |
| coordinator | 5.52 |  | 0.02 | 5.50 |  | 0.00 |  |
| lunixd | 5.02 |  |  | 5.02 |  |  |  |
| lunixd-trim | 4.00 |  |  |  | 4.00 |  |  |
| lunix-q0 | 2.75 |  |  |  |  |  | 2.75 |
| lunix-q1 | 2.75 |  |  |  |  |  | 2.75 |
| destage_workers | 0.75 | 0.00 | 0.75 |  | 0.00 |  |  |

Coordinator thread, share of its blocked time (5.52 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/writes-ram/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (31.5 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 67 | 6.69 | 21.2 % | 100 | 332 |
| finish | 67 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 637 | 0.20 | 0.6 % | 0 | 1 |
| cwseal | 713 | 0.20 | 0.6 % | 0 | 1 |
| reclaim | 131 | 1.41 | 4.5 % | 11 | 33 |
| zero | 303 | 1.35 | 4.3 % | 4 | 8 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 67 | 0.02 | 0.1 % | 0 | 1 |

Cuts: 67; in cut_and_commit 6.69 s = 21 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 5.05 s = 76 % of the cuts, C5-C9 1.64 s = 24 % of the cuts (5.2 % of the window).
Reclaim (C9) 1.41 s, of which the zero rewrite 1.35 s = 20 % of the cuts (4.3 % of the window).

## ram partA_c16k_q4_w4k_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c6_full_c7_checkpoint | destage_worker_cpu | futex_wait | journal_flush | other | queue_io |
|---|---|---|---|---|---|---|---|
| destage_workers | 7.96 |  |  | 0.07 |  | 7.90 |  |
| coordinator | 3.43 |  |  | 0.06 |  | 3.37 |  |
| lunix-q2 | 1.60 |  | 0.01 | 0.03 |  | 1.39 | 0.17 |
| lunix-q3 | 1.59 | 0.00 | 0.00 | 0.02 | 0.00 | 1.39 | 0.17 |
| iou-wrk-306086 | 0.12 |  |  |  |  | 0.12 |  |
| iou-wrk-306087 | 0.11 |  |  |  |  | 0.11 |  |
| lunix-space-tim | 0.08 |  |  | 0.01 |  | 0.07 |  |

Coordinator thread, share of its on-CPU time (3.43 s of a 8.0 s window): other 98 %, futex_wait 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_other | futex_wait | other | queue_io |
|---|---|---|---|---|---|
| lunixd-ctl | 10.04 |  | 5.02 | 5.02 |  |
| destage_workers | 6.04 | 0.02 | 6.02 |  |  |
| lunix-space-tim | 5.91 |  | 5.91 |  |  |
| lunix-ring-time | 5.90 |  | 5.90 |  |  |
| lunixd | 5.02 |  | 5.02 |  |  |
| lunix-q3 | 4.75 |  | 0.03 |  | 4.72 |
| lunix-q2 | 4.74 |  | 0.04 |  | 4.69 |
| coordinator | 4.02 | 0.00 | 4.02 |  |  |
| lunixd-trim | 4.00 |  |  | 4.00 |  |
| lunix-q1 | 2.86 |  |  |  | 2.86 |
| lunix-q0 | 2.86 |  |  |  | 2.86 |

Coordinator thread, share of its blocked time (4.02 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/writes-ram/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (60.8 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 45 | 18.76 | 30.9 % | 417 | 881 |
| finish | 46 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 944 | 0.63 | 1.0 % | 1 | 1 |
| cwseal | 1106 | 0.71 | 1.2 % | 1 | 1 |
| reclaim | 76 | 0.85 | 1.4 % | 11 | 32 |
| zero | 190 | 0.76 | 1.3 % | 4 | 8 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 45 | 0.01 | 0.0 % | 0 | 0 |

Cuts: 45; in cut_and_commit 18.76 s = 31 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 14.62 s = 78 % of the cuts, C5-C9 4.14 s = 22 % of the cuts (6.8 % of the window).
Reclaim (C9) 0.85 s, of which the zero rewrite 0.76 s = 4 % of the cuts (1.3 % of the window).

## ram partA_c16k_q4_seqw_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 8.63 |  | 0.30 | 0.01 | 0.61 | 0.04 | 0.07 | 0.06 | 7.54 |  |  |  |
| coordinator | 3.50 | 0.00 | 0.02 |  | 0.22 | 0.02 | 0.05 | 0.01 | 3.18 | 0.00 | 0.01 | 0.00 |
| lunix-q3 | 0.82 |  |  |  | 0.02 |  | 0.01 | 0.06 | 0.52 |  |  | 0.21 |
| lunix-q2 | 0.72 |  |  |  | 0.02 |  | 0.01 | 0.01 | 0.45 |  |  | 0.23 |
| iou-wrk-306086 | 0.18 |  |  |  |  |  |  |  | 0.18 |  |  |  |
| iou-wrk-306087 | 0.17 |  |  |  |  |  |  |  | 0.17 |  |  |  |

Coordinator thread, share of its on-CPU time (3.50 s of a 8.0 s window): other 91 %, destage_worker_cpu 6 %, futex_wait 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | futex_wait | other | queue_io |
|---|---|---|---|---|
| lunixd-ctl | 10.04 | 5.02 | 5.02 |  |
| lunix-space-tim | 5.98 | 5.98 |  |  |
| lunix-ring-time | 5.90 | 5.90 |  |  |
| lunix-q1 | 5.72 |  |  | 5.72 |
| lunix-q0 | 5.72 |  |  | 5.72 |
| destage_workers | 5.69 | 5.68 | 0.00 |  |
| lunix-q2 | 5.55 | 0.00 |  | 5.55 |
| lunix-q3 | 5.51 | 0.00 | 0.00 | 5.51 |
| lunixd | 5.02 | 5.02 |  |  |
| lunixd-trim | 4.00 |  | 4.00 |  |
| coordinator | 3.74 | 3.74 | 0.00 | 0.00 |

Coordinator thread, share of its blocked time (3.74 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/writes-ram/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (29.6 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 62 | 10.62 | 35.8 % | 171 | 486 |
| finish | 62 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 564 | 0.40 | 1.3 % | 1 | 2 |
| cwseal | 637 | 0.41 | 1.4 % | 1 | 2 |
| reclaim | 121 | 1.22 | 4.1 % | 10 | 34 |
| zero | 265 | 1.14 | 3.9 % | 4 | 9 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 62 | 0.04 | 0.1 % | 1 | 1 |

Cuts: 62; in cut_and_commit 10.62 s = 36 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 8.61 s = 81 % of the cuts, C5-C9 2.01 s = 19 % of the cuts (6.8 % of the window).
Reclaim (C9) 1.22 s, of which the zero rewrite 1.14 s = 11 % of the cuts (3.9 % of the window).

