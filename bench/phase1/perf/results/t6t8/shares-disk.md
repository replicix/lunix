## disk partA_c64k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s |  |
|---|---|

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 18.64 |  |  | 0.00 | 18.64 |  | 0.00 |  |  |  |  |  |  |
| lunix-seal | 12.74 |  |  |  |  |  | 0.00 |  | 0.13 |  | 12.61 |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  | 5.02 |  | 5.02 |  |  |  |  |
| lunix-space-tim | 5.96 |  |  |  |  |  | 5.96 |  |  |  |  |  |  |
| lunix-ring-time | 5.90 |  |  |  |  |  | 5.90 |  |  |  |  |  |  |
| lunix-q3 | 5.88 |  |  |  |  |  | 0.00 | 0.00 | 0.00 |  |  | 5.88 |  |
| lunix-q2 | 5.86 |  |  |  |  |  | 0.00 | 0.00 | 0.00 |  |  | 5.86 |  |
| coordinator | 5.50 | 0.01 | 0.00 |  | 2.94 | 0.04 | 2.50 |  |  | 0.00 | 0.01 | 0.00 | 0.00 |
| lunixd | 5.02 |  |  |  |  |  | 5.02 |  |  |  |  |  |  |
| lunix-q0 | 4.43 |  |  |  |  |  |  |  |  |  |  | 4.43 |  |
| lunix-q1 | 4.43 |  |  |  |  |  |  |  |  |  |  | 4.43 |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  |  | 4.00 |  |  |  |  |

Coordinator thread, share of its blocked time (5.50 s of a 6.0 s window): destage_worker_other 53 %, futex_wait 45 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6t8/shares-disk/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (70.1 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 14 | 48.16 | 68.7 % | 3440 | 5149 |
| finish | 0 | 0 | 0 % | | |
| segseal | 415 | 48.14 | 68.7 % | 116 | 590 |
| cwseal | 473 | 216.90 | 309.4 % | 459 | 862 |
| reclaim | 24 | 0.12 | 0.2 % | 5 | 28 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 15 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 14; in cut_and_commit 48.16 s = 69 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 48.16 s = 100 % of the cuts (68.7 % of the window).
Reclaim (C9) 0.12 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).
## disk partA_c64k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 0.81 | 0.00 | 0.00 | 0.40 | 0.01 |  |  | 0.01 | 0.39 |  |  |  |  |
| coordinator | 0.33 |  | 0.00 | 0.06 | 0.00 | 0.02 | 0.00 |  | 0.23 | 0.01 |  |  | 0.00 |
| lunix-q2 | 0.08 |  |  |  |  |  |  |  | 0.06 |  |  | 0.02 |  |
| lunix-seal | 0.06 |  |  | 0.00 |  |  |  |  | 0.03 |  | 0.03 |  |  |
| lunix-q3 | 0.05 |  |  |  |  |  |  |  | 0.04 |  |  | 0.01 |  |

Coordinator thread, share of its on-CPU time (0.33 s of a 8.0 s window): other 68 %, destage_worker_cpu 19 %, fsync_writeback_wait 7 %, packer_other 3 %, c8_index 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lunix-seal | 10.49 |  |  |  |  |  |  | 0.05 |  | 10.44 |  |  |
| lunixd-ctl | 10.05 |  |  |  |  |  | 5.03 | 5.03 |  |  |  |  |
| lunix-q2 | 5.92 |  |  |  |  |  | 0.00 | 0.94 |  |  | 4.98 |  |
| lunix-q3 | 5.91 |  |  |  |  |  |  | 0.91 |  |  | 5.00 |  |
| lunix-ring-time | 5.90 |  |  |  |  |  | 5.90 |  |  |  |  |  |
| lunix-space-tim | 5.59 |  |  |  |  |  | 5.59 |  |  |  |  |  |
| coordinator | 5.40 | 0.09 | 0.03 |  | 2.88 | 0.08 | 1.71 |  | 0.00 | 0.51 |  | 0.11 |
| lunixd | 5.03 |  |  |  |  |  | 5.03 |  |  |  |  |  |
| lunix-q0 | 4.19 |  |  |  |  |  |  |  |  |  | 4.19 |  |
| lunix-q1 | 4.19 |  |  |  |  |  |  |  |  |  | 4.19 |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  | 4.00 |  |  |  |  |
| destage_workers | 0.20 |  |  | 0.00 | 0.20 |  | 0.00 | 0.00 |  |  |  |  |

Coordinator thread, share of its blocked time (5.40 s of a 6.0 s window): destage_worker_other 53 %, futex_wait 32 %, packer_seal_fsync 9 %, zero_rewrite 2 %, c8_index 2 %, fsync_writeback_wait 1 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6t8/shares-disk/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (35.9 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 21 | 29.36 | 81.7 % | 1398 | 4229 |
| finish | 0 | 0 | 0 % | | |
| segseal | 141 | 12.48 | 34.7 % | 89 | 943 |
| cwseal | 176 | 91.75 | 255.3 % | 521 | 1509 |
| reclaim | 40 | 1.62 | 4.5 % | 41 | 668 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 21 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 21; in cut_and_commit 29.36 s = 82 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 29.36 s = 100 % of the cuts (81.7 % of the window).
Reclaim (C9) 1.62 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).
## disk partA_c16k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 1.59 | 0.00 | 0.00 | 0.10 | 0.00 |  | 0.01 |  | 1.47 |  |  |  |
| coordinator | 0.71 |  |  | 0.04 |  | 0.01 | 0.02 |  | 0.63 | 0.00 |  |  |
| lunix-q2 | 0.31 |  |  |  |  |  | 0.00 |  | 0.28 |  |  | 0.03 |
| lunix-q3 | 0.26 |  |  |  |  |  | 0.00 | 0.01 | 0.22 |  |  | 0.03 |
| lunix-seal | 0.08 |  |  | 0.02 |  |  |  |  | 0.05 |  | 0.01 |  |

Coordinator thread, share of its on-CPU time (0.71 s of a 8.0 s window): other 89 %, destage_worker_cpu 6 %, futex_wait 3 %, fsync_writeback_wait 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|
| lunixd-ctl | 10.04 |  |  |  | 5.02 |  | 5.02 |  |  |
| destage_workers | 8.70 | 0.00 | 0.01 |  | 8.69 |  |  |  |  |
| lunix-seal | 7.42 |  |  |  | 0.00 |  | 7.42 |  |  |
| lunix-space-tim | 5.97 |  |  |  | 5.97 |  |  |  |  |
| lunix-ring-time | 5.90 |  |  |  | 5.90 |  |  |  |  |
| lunix-q3 | 5.70 |  |  |  | 0.00 | 0.00 | 0.00 | 5.70 |  |
| lunix-q2 | 5.69 |  |  |  | 0.00 |  | 0.00 | 5.69 |  |
| coordinator | 5.32 |  | 0.00 | 0.38 | 4.94 |  | 0.00 | 0.00 | 0.01 |
| lunixd | 5.02 |  |  |  | 5.02 |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  | 4.00 |  |  |
| lunix-q0 | 3.54 |  |  |  |  |  |  | 3.54 |  |
| lunix-q1 | 3.54 |  |  |  |  |  |  | 3.54 |  |

Coordinator thread, share of its blocked time (5.32 s of a 6.0 s window): futex_wait 93 %, fsync_writeback_wait 7 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6t8/shares-disk/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (64.0 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 29 | 34.71 | 54.3 % | 1197 | 2098 |
| finish | 0 | 0 | 0 % | | |
| segseal | 288 | 21.50 | 33.6 % | 75 | 644 |
| cwseal | 354 | 121.17 | 189.5 % | 342 | 839 |
| reclaim | 50 | 0.48 | 0.7 % | 10 | 99 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 30 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 29; in cut_and_commit 34.71 s = 54 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 34.71 s = 100 % of the cuts (54.3 % of the window).
Reclaim (C9) 0.48 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).
## disk partA_c16k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | destage_worker_cpu | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|
| coordinator | 0.68 | 0.00 | 0.04 | 0.03 | 0.01 | 0.59 | 0.00 |  |
| destage_workers | 0.60 |  |  |  | 0.01 | 0.59 |  |  |
| lunix-q2 | 0.09 |  |  |  |  | 0.06 |  | 0.03 |
| lunix-q3 | 0.08 |  |  |  |  | 0.06 |  | 0.01 |

Coordinator thread, share of its on-CPU time (0.68 s of a 8.0 s window): other 87 %, destage_worker_cpu 6 %, fsync_writeback_wait 4 %, futex_wait 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| lunix-seal | 13.72 |  |  |  |  |  | 0.00 | 5.13 |  | 8.59 |  |  |
| lunixd-ctl | 10.05 |  |  |  |  |  | 5.03 | 5.03 |  |  |  |  |
| lunix-space-tim | 5.99 |  |  |  |  |  | 5.99 |  |  |  |  |  |
| lunix-q3 | 5.93 |  |  |  |  |  | 0.00 | 2.21 |  |  | 3.71 |  |
| lunix-q2 | 5.91 |  |  |  |  |  | 0.00 | 2.19 |  |  | 3.72 |  |
| lunix-ring-time | 5.90 |  |  |  |  |  | 5.90 |  |  |  |  |  |
| coordinator | 5.51 | 0.01 | 0.02 |  | 2.25 | 0.17 | 2.05 |  | 0.00 | 0.93 | 0.00 | 0.09 |
| lunixd | 5.03 |  |  |  |  |  | 5.03 |  |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  | 4.00 |  |  |  |  |
| lunix-q0 | 3.51 |  |  |  |  |  |  |  |  |  | 3.51 |  |
| lunix-q1 | 3.51 |  |  |  |  |  |  |  |  |  | 3.51 |  |
| destage_workers | 0.85 |  |  | 0.00 | 0.61 |  | 0.25 |  |  |  |  |  |

Coordinator thread, share of its blocked time (5.51 s of a 6.0 s window): destage_worker_other 41 %, futex_wait 37 %, packer_seal_fsync 17 %, fsync_writeback_wait 3 %, zero_rewrite 2 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6t8/shares-disk/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (34.5 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 16 | 19.18 | 55.5 % | 1199 | 2007 |
| finish | 0 | 0 | 0 % | | |
| segseal | 85 | 8.66 | 25.1 % | 102 | 1225 |
| cwseal | 125 | 61.75 | 178.7 % | 494 | 1476 |
| reclaim | 29 | 0.60 | 1.7 % | 21 | 113 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 16 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 16; in cut_and_commit 19.18 s = 56 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 19.18 s = 100 % of the cuts (55.5 % of the window).
Reclaim (C9) 0.60 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).
