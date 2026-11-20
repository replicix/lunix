
## disk partA_c64k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c6_full_c7_checkpoint | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 1.30 | 0.00 |  | 0.00 | 0.00 | 1.04 | 0.03 |  | 0.01 | 0.21 |  |  |  |  |
| coordinator | 0.43 |  |  |  |  | 0.20 | 0.01 | 0.01 |  | 0.07 | 0.01 | 0.13 | 0.01 | 0.00 |
| lunix-q2 | 0.08 |  | 0.00 |  |  | 0.00 |  |  | 0.00 | 0.05 |  |  | 0.02 |  |

Coordinator thread, share of its on-CPU time (0.43 s of a 8.0 s window): destage_worker_cpu 45 %, packer_seal_fsync 30 %, other 15 %, packer_other 3 %, fsync_writeback_wait 2 %, destage_worker_other 2 %, queue_io 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 33.42 |  |  | 0.00 | 10.19 |  | 21.09 | 2.15 |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  | 5.02 | 5.02 |  |  |  |
| coordinator | 5.61 | 0.01 | 0.00 |  | 0.00 | 2.75 | 0.95 |  | 1.40 | 0.00 | 0.50 |
| lunixd | 5.02 |  |  |  |  |  | 5.02 |  |  |  |  |
| lunix-space-tim | 5.00 |  |  |  |  |  | 5.00 |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  | 4.00 |  |  |  |
| lunix-q3 | 2.85 |  |  |  |  |  |  |  |  | 2.85 |  |
| lunix-q2 | 1.00 |  |  |  |  |  | 0.00 |  |  | 1.00 |  |

Coordinator thread, share of its blocked time (5.61 s of a 6.0 s window): fsync_writeback_wait 49 %, packer_seal_fsync 25 %, futex_wait 17 %, zero_rewrite 9 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: shares/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (63.8 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 12 | 59.11 | 92.7 % | 4926 | 7174 |
| finish | 12 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 310 | 44.30 | 69.5 % | 143 | 368 |
| cwseal | 335 | 46.48 | 72.9 % | 139 | 400 |
| reclaim | 24 | 6.70 | 10.5 % | 279 | 587 |
| zero | 13 | 6.63 | 10.4 % | 510 | 581 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 13 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 12; in cut_and_commit 59.11 s = 93 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 51.77 s = 88 % of the cuts, C5-C9 7.34 s = 12 % of the cuts (11.5 % of the window).
Reclaim (C9) 6.70 s, of which the zero rewrite 6.63 s = 11 % of the cuts (10.4 % of the window).

## disk partA_c64k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 1.06 | 0.02 | 0.85 | 0.04 |  | 0.01 | 0.14 |  |  |  |  |
| coordinator | 0.42 | 0.01 | 0.15 | 0.01 | 0.01 |  | 0.08 | 0.02 | 0.12 |  | 0.01 |

Coordinator thread, share of its on-CPU time (0.42 s of a 8.0 s window): destage_worker_cpu 35 %, packer_seal_fsync 29 %, other 19 %, packer_other 5 %, destage_worker_other 3 %, c8_index 3 %, fsync_writeback_wait 3 %, zero_rewrite 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 15.65 |  |  |  | 15.49 | 0.16 |  |  |  |
| lunixd-ctl | 10.05 |  |  |  | 5.02 | 5.02 |  |  |  |
| coordinator | 5.87 | 0.01 | 0.19 | 2.45 | 0.37 | 0.28 | 0.00 |  | 2.58 |
| lunixd | 5.02 |  |  |  | 5.02 |  |  |  |  |
| lunix-space-tim | 5.00 |  |  |  | 5.00 |  |  |  |  |
| lunix-q3 | 4.79 |  |  |  |  | 0.07 |  | 4.72 |  |
| lunix-q2 | 4.50 |  |  |  |  |  |  | 4.50 |  |
| lunix-q0 | 4.44 |  |  |  |  |  |  | 4.44 |  |
| lunix-q1 | 4.44 |  |  |  |  |  |  | 4.44 |  |
| lunixd-trim | 4.00 |  |  |  |  | 4.00 |  |  |  |

Coordinator thread, share of its blocked time (5.87 s of a 6.0 s window): zero_rewrite 44 %, fsync_writeback_wait 42 %, futex_wait 6 %, other 5 %, c9_reclaim 3 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: shares/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (41.6 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 19 | 36.83 | 88.5 % | 1938 | 4963 |
| finish | 21 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 107 | 22.17 | 53.3 % | 207 | 750 |
| cwseal | 126 | 22.32 | 53.6 % | 177 | 750 |
| reclaim | 40 | 12.73 | 30.6 % | 318 | 1150 |
| zero | 20 | 12.23 | 29.4 % | 611 | 869 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 20 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 19; in cut_and_commit 36.83 s = 89 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 21.96 s = 60 % of the cuts, C5-C9 14.87 s = 40 % of the cuts (35.7 % of the window).
Reclaim (C9) 12.73 s, of which the zero rewrite 12.23 s = 33 % of the cuts (29.4 % of the window).

## disk partA_c16k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 0.80 |  |  |  |  |  |  |  |  | 0.80 |  |  |  |  |
| coordinator | 0.68 |  |  |  | 0.13 | 0.01 | 0.07 | 0.00 |  | 0.41 | 0.00 | 0.04 |  | 0.00 |
| lunix-q3 | 0.19 |  |  |  |  |  |  |  |  | 0.17 |  |  | 0.02 |  |
| lunix-q2 | 0.19 |  |  |  |  |  |  | 0.00 |  | 0.17 |  |  | 0.01 |  |
| :510472 | 0.06 |  |  |  | 0.06 | 0.00 |  |  |  |  |  |  |  |  |
| :510466 | 0.06 |  | 0.00 | 0.00 | 0.05 |  |  |  | 0.00 | 0.01 |  |  |  |  |
| :510470 | 0.06 | 0.00 | 0.00 |  | 0.05 | 0.00 |  |  | 0.00 | 0.00 |  |  |  |  |
| :510465 | 0.06 |  |  |  | 0.05 | 0.01 |  |  |  | 0.01 |  |  |  |  |
| :510468 | 0.06 |  | 0.00 |  | 0.04 | 0.00 |  |  | 0.00 | 0.01 |  |  |  |  |
| :510471 | 0.06 |  | 0.00 | 0.00 | 0.04 | 0.01 |  |  |  | 0.01 |  |  |  |  |
| :510469 | 0.06 |  | 0.00 |  | 0.05 | 0.00 |  |  |  | 0.01 |  |  |  |  |
| :510467 | 0.06 | 0.00 | 0.01 |  | 0.05 |  |  |  |  | 0.00 |  |  |  |  |

Coordinator thread, share of its on-CPU time (0.68 s of a 8.0 s window): other 61 %, destage_worker_cpu 20 %, fsync_writeback_wait 10 %, packer_seal_fsync 7 %, destage_worker_other 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 26.13 | 0.00 |  |  | 0.00 | 12.69 |  | 13.44 |  |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  |  | 5.02 | 5.02 |  |  |  |
| lunixd | 5.02 |  |  |  |  |  |  | 5.02 |  |  |  |  |
| lunix-space-tim | 5.00 |  |  |  |  |  |  | 5.00 |  |  |  |  |
| lunix-q2 | 4.99 |  |  |  |  |  |  |  |  |  | 4.99 |  |
| coordinator | 4.70 |  | 0.02 | 0.00 |  | 0.00 | 1.69 | 0.31 |  | 1.51 | 0.00 | 1.18 |
| lunixd-trim | 4.00 |  |  |  |  |  |  |  | 4.00 |  |  |  |
| lunix-q3 | 2.69 |  |  |  |  |  |  |  |  |  | 2.69 |  |
| lunix-q1 | 2.07 |  |  |  |  |  |  |  |  |  | 2.07 |  |
| lunix-q0 | 2.07 |  |  |  |  |  |  |  |  |  | 2.07 |  |

Coordinator thread, share of its blocked time (4.70 s of a 6.0 s window): fsync_writeback_wait 36 %, packer_seal_fsync 32 %, zero_rewrite 25 %, futex_wait 7 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: shares/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (62.4 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 27 | 48.49 | 77.7 % | 1796 | 2419 |
| finish | 28 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 216 | 27.00 | 43.3 % | 125 | 294 |
| cwseal | 278 | 32.68 | 52.4 % | 118 | 294 |
| reclaim | 54 | 15.62 | 25.0 % | 289 | 773 |
| zero | 28 | 15.44 | 24.8 % | 552 | 766 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 28 | 0.01 | 0.0 % | 0 | 0 |

Cuts: 27; in cut_and_commit 48.49 s = 78 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 29.65 s = 61 % of the cuts, C5-C9 18.85 s = 39 % of the cuts (30.2 % of the window).
Reclaim (C9) 15.62 s, of which the zero rewrite 15.44 s = 32 % of the cuts (24.8 % of the window).

## disk partA_c16k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | gc_mark | gc_other | journal_flush | other | packer_other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| coordinator | 0.68 | 0.00 | 0.11 | 0.19 | 0.00 | 0.02 | 0.00 | 0.03 | 0.04 | 0.00 | 0.24 | 0.01 | 0.03 | 0.00 | 0.01 |
| destage_workers | 0.24 |  | 0.01 | 0.11 | 0.01 |  |  |  |  | 0.01 | 0.10 |  |  |  |  |
| lunix-q2 | 0.06 |  |  | 0.01 |  |  |  |  |  | 0.02 | 0.01 |  |  | 0.03 |  |
| lunix-q3 | 0.05 |  |  | 0.00 |  |  |  |  |  | 0.01 | 0.02 |  |  | 0.02 |  |

Coordinator thread, share of its on-CPU time (0.68 s of a 8.0 s window): other 36 %, destage_worker_cpu 28 %, c8_index 16 %, gc_other 5 %, gc_mark 5 %, packer_seal_fsync 4 %, fsync_writeback_wait 3 %, packer_other 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | fsync_writeback_wait | futex_wait | other | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|
| lunixd-ctl | 10.04 |  |  | 5.02 | 5.02 |  |  |
| destage_workers | 6.61 |  |  | 6.61 |  |  |  |
| coordinator | 5.71 | 0.00 | 2.90 | 0.01 | 0.00 |  | 2.80 |
| lunix-q3 | 5.32 |  |  |  |  | 5.32 |  |
| lunix-q2 | 5.32 |  |  |  |  | 5.32 |  |
| lunixd | 5.02 |  |  | 5.02 |  |  |  |
| lunix-space-tim | 5.00 |  |  | 5.00 |  |  |  |
| lunixd-trim | 4.00 |  |  |  | 4.00 |  |  |
| lunix-q1 | 3.88 |  |  |  |  | 3.88 |  |
| lunix-q0 | 3.88 |  |  |  |  | 3.88 |  |

Coordinator thread, share of its blocked time (5.71 s of a 6.0 s window): fsync_writeback_wait 51 %, zero_rewrite 49 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: shares/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (37.2 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 23 | 31.11 | 83.6 % | 1353 | 2886 |
| finish | 24 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 69 | 10.98 | 29.5 % | 159 | 638 |
| cwseal | 111 | 16.40 | 44.1 % | 148 | 638 |
| reclaim | 47 | 13.50 | 36.3 % | 287 | 1543 |
| zero | 25 | 13.38 | 36.0 % | 535 | 1011 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 24 | 0.01 | 0.0 % | 0 | 0 |

Cuts: 23; in cut_and_commit 31.11 s = 84 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 11.32 s = 36 % of the cuts, C5-C9 19.79 s = 64 % of the cuts (53.2 % of the window).
Reclaim (C9) 13.50 s, of which the zero rewrite 13.38 s = 43 % of the cuts (36.0 % of the window).
