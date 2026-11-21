## disk partA_c64k_q4_w4k_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 1.66 | 0.00 | 0.00 | 0.00 | 0.45 | 0.02 |  | 0.01 | 0.00 | 1.17 |  |  |  |
| coordinator | 0.50 |  |  |  | 0.11 | 0.00 | 0.09 |  |  | 0.23 | 0.01 | 0.05 |  |
| lunix-q2 | 0.17 |  |  |  | 0.00 |  |  |  | 0.00 | 0.13 |  |  | 0.03 |
| lunix-q3 | 0.11 |  |  |  |  |  |  |  | 0.00 | 0.10 |  |  | 0.00 |

Coordinator thread, share of its on-CPU time (0.50 s of a 8.0 s window): other 46 %, destage_worker_cpu 23 %, fsync_writeback_wait 18 %, packer_seal_fsync 10 %, packer_other 3 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c5_map_seal | c6_full_c7_checkpoint | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 33.10 | 0.00 |  |  |  | 0.00 | 33.10 |  | 0.00 | 0.00 |  |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  |  |  | 5.02 |  | 5.02 |  |  |  |
| lunix-ring-time | 5.90 |  |  |  |  |  |  |  | 5.90 |  |  |  |  |  |
| lunix-space-tim | 5.88 |  |  |  |  |  |  |  | 5.88 |  |  |  |  |  |
| lunix-q3 | 5.88 |  |  |  |  |  |  |  | 0.00 |  | 0.00 |  | 5.87 |  |
| lunix-q2 | 5.82 |  | 0.00 |  |  |  |  |  | 0.00 |  |  |  | 5.82 |  |
| coordinator | 5.57 |  |  | 0.02 | 0.00 |  | 0.00 | 0.01 | 1.40 |  | 0.00 | 4.12 | 0.00 | 0.01 |
| lunixd | 5.02 |  |  |  |  |  |  |  | 5.02 |  |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  |  |  |  | 4.00 |  |  |  |
| lunix-q1 | 2.90 |  |  |  |  |  |  |  |  |  |  |  | 2.90 |  |
| lunix-q0 | 2.90 |  |  |  |  |  |  |  |  |  |  |  | 2.90 |  |

Coordinator thread, share of its blocked time (5.57 s of a 6.0 s window): packer_seal_fsync 74 %, futex_wait 25 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/shares-disk/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (66.4 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 13 | 50.62 | 76.2 % | 3894 | 6504 |
| finish | 14 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 403 | 53.39 | 80.4 % | 132 | 195 |
| cwseal | 456 | 58.14 | 87.5 % | 127 | 253 |
| reclaim | 23 | 0.12 | 0.2 % | 5 | 39 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 14 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 13; in cut_and_commit 50.62 s = 76 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 49.07 s = 97 % of the cuts, C5-C9 1.55 s = 3 % of the cuts (2.3 % of the window).
Reclaim (C9) 0.12 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).

## disk partA_c64k_q4_seqw_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | destage_worker_cpu | fsync_writeback_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 0.56 |  | 0.04 |  | 0.00 | 0.52 |  |  |  |
| coordinator | 0.42 | 0.00 | 0.06 | 0.08 |  | 0.25 | 0.00 | 0.02 |  |
| lunix-q2 | 0.09 |  | 0.00 |  |  | 0.06 |  |  | 0.03 |
| lunix-q3 | 0.07 |  | 0.01 |  |  | 0.05 |  |  | 0.02 |

Coordinator thread, share of its on-CPU time (0.42 s of a 8.0 s window): other 60 %, fsync_writeback_wait 19 %, destage_worker_cpu 14 %, packer_seal_fsync 6 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 19.42 |  |  | 0.00 | 19.42 |  |  |  |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  | 5.02 | 5.02 |  |  |  |
| lunix-space-tim | 5.99 |  |  |  |  |  | 5.99 |  |  |  |  |
| lunix-q3 | 5.92 |  |  |  |  |  |  | 0.08 |  | 5.84 |  |
| lunix-ring-time | 5.90 |  |  |  |  |  | 5.90 |  |  |  |  |
| lunix-q2 | 5.88 |  |  |  |  |  |  | 0.15 |  | 5.73 |  |
| coordinator | 5.64 | 0.83 | 0.04 |  | 0.01 | 0.09 | 0.13 | 0.00 | 4.36 |  | 0.20 |
| lunixd | 5.02 |  |  |  |  |  | 5.02 |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  | 4.00 |  |  |  |
| lunix-q0 | 3.28 |  |  |  |  |  |  |  |  | 3.28 |  |
| lunix-q1 | 3.28 |  |  |  |  |  |  |  |  | 3.28 |  |

Coordinator thread, share of its blocked time (5.64 s of a 6.0 s window): packer_seal_fsync 77 %, c8_index 15 %, zero_rewrite 4 %, futex_wait 2 %, fsync_writeback_wait 2 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/shares-disk/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (36.0 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 20 | 24.93 | 69.3 % | 1246 | 3284 |
| finish | 21 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 115 | 19.42 | 54.0 % | 169 | 632 |
| cwseal | 168 | 28.56 | 79.4 % | 170 | 632 |
| reclaim | 34 | 1.44 | 4.0 % | 42 | 443 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 19 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 20; in cut_and_commit 24.93 s = 69 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 18.42 s = 74 % of the cuts, C5-C9 6.51 s = 26 % of the cuts (18.1 % of the window).
Reclaim (C9) 1.44 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).

## disk partA_c16k_q4_w4k_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | destage_worker_cpu | fsync_writeback_wait | futex_wait | other | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|
| destage_workers | 1.71 |  |  | 0.01 | 1.70 |  |  |
| coordinator | 0.87 | 0.03 | 0.12 | 0.02 | 0.69 |  | 0.00 |
| lunix-q2 | 0.29 | 0.00 |  |  | 0.26 | 0.03 |  |
| lunix-q3 | 0.26 |  |  | 0.00 | 0.24 | 0.01 |  |

Coordinator thread, share of its on-CPU time (0.87 s of a 8.0 s window): other 80 %, fsync_writeback_wait 14 %, destage_worker_cpu 4 %, futex_wait 3 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | journal_flush | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 19.33 | 0.00 |  |  | 0.00 | 1.68 |  | 17.65 | 0.00 |  |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  |  | 5.02 |  | 5.02 |  |  |  |
| lunix-space-tim | 5.98 |  |  |  |  |  |  | 5.98 |  |  |  |  |  |
| lunix-ring-time | 5.90 |  |  |  |  |  |  | 5.90 |  |  |  |  |  |
| lunix-q3 | 5.79 |  |  |  |  |  |  | 0.00 |  | 0.00 |  | 5.79 |  |
| lunix-q2 | 5.75 |  |  |  |  |  |  | 0.00 |  | 0.00 |  | 5.75 |  |
| coordinator | 5.18 |  | 0.01 | 0.00 |  | 0.00 | 2.60 | 2.41 |  |  | 0.15 | 0.00 | 0.01 |
| lunixd | 5.02 |  |  |  |  |  |  | 5.02 |  |  |  |  |  |
| lunix-q1 | 4.26 |  |  |  |  |  |  |  |  |  |  | 4.26 |  |
| lunix-q0 | 4.26 |  |  |  |  |  |  |  |  |  |  | 4.26 |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  |  |  | 4.00 |  |  |  |

Coordinator thread, share of its blocked time (5.18 s of a 6.0 s window): fsync_writeback_wait 50 %, futex_wait 47 %, packer_seal_fsync 3 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/shares-disk/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (59.4 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 29 | 33.55 | 56.5 % | 1157 | 2049 |
| finish | 30 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 261 | 25.94 | 43.7 % | 99 | 218 |
| cwseal | 324 | 31.52 | 53.1 % | 97 | 218 |
| reclaim | 54 | 0.26 | 0.4 % | 5 | 30 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 30 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 29; in cut_and_commit 33.55 s = 56 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 28.95 s = 86 % of the cuts, C5-C9 4.60 s = 14 % of the cuts (7.8 % of the window).
Reclaim (C9) 0.26 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).

## disk partA_c16k_q4_seqw_rep1


### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | destage_worker_cpu | fsync_writeback_wait | futex_wait | gc_mark | gc_other | journal_flush | other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|
| coordinator | 0.85 | 0.02 | 0.06 | 0.08 | 0.01 | 0.01 | 0.00 |  | 0.66 | 0.00 |  |
| destage_workers | 0.60 |  |  |  | 0.01 |  |  |  | 0.59 |  |  |
| lunix-q2 | 0.09 |  |  |  |  |  |  | 0.00 | 0.08 |  | 0.01 |
| lunix-q3 | 0.09 |  | 0.00 |  |  |  |  |  | 0.08 |  | 0.01 |

Coordinator thread, share of its on-CPU time (0.85 s of a 8.0 s window): other 77 %, fsync_writeback_wait 10 %, destage_worker_cpu 8 %, c8_index 3 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | fsync_writeback_wait | futex_wait | other | packer_seal_fsync | queue_io | zero_rewrite |
|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 26.95 |  |  | 0.00 | 26.95 |  |  |  |  |  |  |
| lunixd-ctl | 10.04 |  |  |  |  |  | 5.02 | 5.02 |  |  |  |
| lunix-space-tim | 5.99 |  |  |  |  |  | 5.99 |  |  |  |  |
| lunix-q2 | 5.93 |  |  |  |  |  |  | 0.60 |  | 5.33 |  |
| lunix-q3 | 5.91 |  |  |  |  |  | 0.00 | 0.47 |  | 5.44 |  |
| lunix-ring-time | 5.90 |  |  |  |  |  | 5.90 |  |  |  |  |
| coordinator | 5.42 | 0.03 | 0.03 |  | 0.01 | 0.04 | 0.13 |  | 5.08 |  | 0.10 |
| lunix-q0 | 5.36 |  |  |  |  |  |  |  |  | 5.36 |  |
| lunix-q1 | 5.36 |  |  |  |  |  |  |  |  | 5.36 |  |
| lunixd | 5.02 |  |  |  |  |  | 5.02 |  |  |  |  |
| lunixd-trim | 4.00 |  |  |  |  |  |  | 4.00 |  |  |  |

Coordinator thread, share of its blocked time (5.42 s of a 6.0 s window): packer_seal_fsync 94 %, futex_wait 2 %, zero_rewrite 2 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: results/t6r/shares-disk/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (34.1 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 18 | 20.85 | 61.2 % | 1158 | 2771 |
| finish | 18 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 84 | 13.98 | 41.1 % | 166 | 1046 |
| cwseal | 135 | 22.80 | 67.0 % | 169 | 1046 |
| reclaim | 30 | 0.78 | 2.3 % | 26 | 222 |
| zero | 0 | 0 | 0 % | | |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 17 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 18; in cut_and_commit 20.85 s = 61 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 15.05 s = 72 % of the cuts, C5-C9 5.81 s = 28 % of the cuts (17.1 % of the window).
Reclaim (C9) 0.78 s, of which the zero rewrite 0.00 s = 0 % of the cuts (0.0 % of the window).

