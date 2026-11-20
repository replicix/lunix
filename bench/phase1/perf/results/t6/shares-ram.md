
## ram partA_c64k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | destage_worker_cpu | destage_worker_other | futex_wait | gc_mark | gc_other | gc_urgent_mark | journal_flush | other | packer_other | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 1.47 | 0.00 | 0.01 | 1.32 | 0.03 |  |  |  |  | 0.02 | 0.09 |  |  |
| lunix-space-lad | 0.38 |  | 0.04 | 0.13 |  |  | 0.01 | 0.02 | 0.01 |  | 0.16 | 0.00 |  |
| coordinator | 0.34 |  | 0.00 | 0.20 | 0.02 |  |  |  |  | 0.00 | 0.11 | 0.01 | 0.01 |
| lunix-q3 | 0.08 |  |  | 0.00 |  |  |  |  |  | 0.00 | 0.05 |  | 0.03 |
| lunix-q2 | 0.08 |  |  | 0.00 |  |  |  |  |  | 0.01 | 0.06 |  | 0.02 |

Coordinator thread, share of its on-CPU time (0.34 s of a 8.0 s window): destage_worker_cpu 58 %, other 32 %, destage_worker_other 5 %, queue_io 2 %, packer_other 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | futex_wait | other | queue_io |
|---|---|---|---|---|
| lunixd-ctl | 10.04 | 5.02 | 5.02 |  |
| coordinator | 5.84 | 5.84 |  |  |
| lunix-space-tim | 5.16 | 5.16 |  |  |
| lunixd | 5.02 | 5.02 |  |  |
| lunix-space-lad | 5.00 | 5.00 |  |  |
| lunixd-trim | 4.00 |  | 4.00 |  |
| lunix-q2 | 3.65 |  |  | 3.65 |
| lunix-q1 | 3.65 |  |  | 3.65 |
| lunix-q0 | 3.65 |  |  | 3.65 |
| lunix-q3 | 3.65 |  |  | 3.65 |

Coordinator thread, share of its blocked time (5.84 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: ram2/out/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (60.3 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 28 | 8.92 | 14.8 % | 319 | 671 |
| finish | 28 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 945 | 0.25 | 0.4 % | 0 | 3 |
| cwseal | 1007 | 0.26 | 0.4 % | 0 | 3 |
| reclaim | 51 | 0.54 | 0.9 % | 11 | 34 |
| zero | 109 | 0.46 | 0.8 % | 4 | 8 |
| gcurgent | 6 | 0.74 | 1.2 % | 124 | 259 |
| runurgent | 37 | 1.13 | 1.9 % | 31 | 428 |
| trueup | 28 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 28; in cut_and_commit 8.92 s = 15 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 7.33 s = 82 % of the cuts, C5-C9 1.59 s = 18 % of the cuts (2.6 % of the window).
Reclaim (C9) 0.54 s, of which the zero rewrite 0.46 s = 5 % of the cuts (0.8 % of the window).

## ram partA_c64k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 13.78 | 0.00 | 0.26 | 0.00 | 3.19 | 0.11 | 0.02 | 0.22 | 9.97 |  |  |  |
| coordinator | 5.31 | 0.00 | 0.02 | 0.00 | 0.58 | 0.05 | 0.05 |  | 4.50 | 0.09 | 0.01 | 0.01 |
| lunix-q2 | 2.61 |  |  |  | 0.12 |  | 0.00 | 0.17 | 1.55 |  |  | 0.77 |
| lunix-q3 | 2.50 |  |  |  | 0.14 |  |  | 0.16 | 1.48 |  |  | 0.73 |
| iou-wrk-737192 | 0.62 |  |  |  |  |  |  |  | 0.62 |  |  |  |
| iou-wrk-737191 | 0.61 |  |  |  |  |  |  |  | 0.61 |  |  |  |

Coordinator thread, share of its on-CPU time (5.31 s of a 8.0 s window): other 85 %, destage_worker_cpu 11 %, packer_other 2 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_other | futex_wait | other | queue_io |
|---|---|---|---|---|---|
| lunixd-ctl | 10.05 |  | 5.02 | 5.02 |  |
| lunix-space-tim | 5.87 |  | 5.87 |  |  |
| lunix-q3 | 5.73 |  | 0.00 |  | 5.73 |
| lunix-q1 | 5.70 |  |  |  | 5.70 |
| lunix-q0 | 5.70 |  |  |  | 5.70 |
| lunix-q2 | 5.69 |  |  |  | 5.69 |
| coordinator | 5.13 | 0.00 | 5.12 |  |  |
| lunixd | 5.02 |  | 5.02 |  |  |
| lunixd-trim | 4.00 |  |  | 4.00 |  |
| destage_workers | 1.08 | 0.33 | 0.75 | 0.00 |  |

Coordinator thread, share of its blocked time (5.13 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: ram2/out/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (28.7 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 75 | 8.43 | 29.3 % | 112 | 351 |
| finish | 75 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 675 | 0.23 | 0.8 % | 0 | 2 |
| cwseal | 766 | 0.23 | 0.8 % | 0 | 2 |
| reclaim | 147 | 1.54 | 5.4 % | 10 | 30 |
| zero | 322 | 1.51 | 5.3 % | 5 | 11 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 75 | 0.03 | 0.1 % | 0 | 1 |

Cuts: 75; in cut_and_commit 8.43 s = 29 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 5.78 s = 69 % of the cuts, C5-C9 2.65 s = 31 % of the cuts (9.2 % of the window).
Reclaim (C9) 1.54 s, of which the zero rewrite 1.51 s = 18 % of the cuts (5.3 % of the window).

## ram partA_c16k_q4_w4k_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c5_map_seal | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | futex_wait | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 12.15 | 0.00 | 0.04 | 0.00 | 0.88 | 0.06 | 0.12 | 0.01 | 11.04 |  |  |  |
| coordinator | 5.95 | 0.00 |  |  | 0.24 | 0.03 | 0.07 | 0.00 | 5.59 | 0.00 | 0.01 | 0.01 |
| lunix-q2 | 2.21 |  |  |  | 0.01 |  | 0.02 | 0.01 | 1.99 |  |  | 0.19 |
| lunix-q3 | 2.20 |  |  |  | 0.01 |  | 0.02 | 0.01 | 1.98 |  |  | 0.18 |
| iou-wrk-821670 | 0.33 |  |  |  |  |  |  |  | 0.33 |  |  |  |
| iou-wrk-821669 | 0.31 |  |  |  |  |  |  |  | 0.31 |  |  |  |

Coordinator thread, share of its on-CPU time (5.95 s of a 8.0 s window): other 94 %, destage_worker_cpu 4 %, futex_wait 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | futex_wait | other | queue_io |
|---|---|---|---|---|
| lunixd-ctl | 10.04 | 5.02 | 5.02 |  |
| coordinator | 5.93 | 5.93 |  |  |
| lunix-space-tim | 5.14 | 5.14 |  |  |
| lunixd | 5.02 | 5.02 |  |  |
| lunix-space-lad | 4.44 | 4.44 |  |  |
| lunixd-trim | 4.00 |  | 4.00 |  |
| lunix-q2 | 3.83 | 0.00 |  | 3.83 |
| lunix-q3 | 3.83 | 0.00 |  | 3.83 |

Coordinator thread, share of its blocked time (5.93 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: ram2/out/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (59.3 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 66 | 21.98 | 37.0 % | 333 | 873 |
| finish | 67 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 1146 | 0.76 | 1.3 % | 1 | 1 |
| cwseal | 1280 | 0.82 | 1.4 % | 1 | 1 |
| reclaim | 128 | 1.52 | 2.6 % | 12 | 38 |
| zero | 289 | 1.28 | 2.2 % | 4 | 9 |
| gcurgent | 3 | 1.56 | 2.6 % | 520 | 737 |
| runurgent | 27 | 1.68 | 2.8 % | 62 | 738 |
| trueup | 67 | 0.01 | 0.0 % | 0 | 0 |

Cuts: 66; in cut_and_commit 21.98 s = 37 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 17.60 s = 80 % of the cuts, C5-C9 4.38 s = 20 % of the cuts (7.4 % of the window).
Reclaim (C9) 1.52 s, of which the zero rewrite 1.28 s = 6 % of the cuts (2.2 % of the window).

## ram partA_c16k_q4_seqw_rep1

### on-CPU time (cpu-clock samples at 499 Hz)

| role | total s | c8_index | c9_reclaim | destage_worker_cpu | destage_worker_other | futex_wait | gc_mark | gc_other | journal_flush | other | packer_other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| destage_workers | 9.11 | 0.63 | 0.00 | 0.99 | 0.11 | 0.06 |  |  | 0.07 | 7.24 |  |  |  |
| coordinator | 3.53 | 0.14 |  | 0.43 | 0.04 | 0.03 | 0.02 | 0.00 | 0.00 | 2.85 | 0.01 | 0.01 | 0.00 |
| lunix-q2 | 0.87 |  |  | 0.04 |  | 0.00 |  |  | 0.06 | 0.56 |  |  | 0.22 |
| lunix-q3 | 0.79 |  |  | 0.04 |  |  |  |  | 0.03 | 0.52 |  |  | 0.20 |
| iou-wrk-821669 | 0.22 |  |  |  |  |  |  |  |  | 0.22 |  |  |  |
| iou-wrk-821670 | 0.21 |  |  |  |  |  |  |  |  | 0.21 |  |  |  |

Coordinator thread, share of its on-CPU time (3.53 s of a 8.0 s window): other 81 %, destage_worker_cpu 12 %, c8_index 4 %, destage_worker_other 1 %

(window 8.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

### blocked time (sched_switch intervals)

| role | total s | destage_worker_cpu | destage_worker_other | futex_wait | other | packer_seal_fsync | queue_io |
|---|---|---|---|---|---|---|---|
| lunixd-ctl | 10.05 |  |  | 5.02 | 5.03 |  |  |
| destage_workers | 8.73 | 0.00 | 0.42 | 8.31 | 0.00 |  |  |
| lunix-q0 | 5.83 |  |  |  |  |  | 5.83 |
| lunix-q1 | 5.83 |  |  |  |  |  | 5.83 |
| lunix-space-tim | 5.56 |  |  | 5.56 |  |  |  |
| lunixd | 5.02 |  |  | 5.02 |  |  |  |
| lunix-q3 | 5.00 |  |  | 0.00 |  |  | 5.00 |
| lunix-q2 | 4.94 |  |  | 0.00 |  |  | 4.94 |
| lunixd-trim | 4.00 |  |  |  | 4.00 |  |  |
| coordinator | 2.57 |  | 0.01 | 2.56 |  | 0.00 | 0.00 |

Coordinator thread, share of its blocked time (2.57 s of a 6.0 s window): futex_wait 100 %

(window 6.0 s; destage_workers = all worker threads summed, so up to 8 s per second of window)

uprobes: ram2/out/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (29.1 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 61 | 11.65 | 40.0 % | 191 | 473 |
| finish | 61 | 0.00 | 0.0 % | 0 | 0 |
| segseal | 591 | 0.43 | 1.5 % | 1 | 1 |
| cwseal | 665 | 0.44 | 1.5 % | 1 | 1 |
| reclaim | 120 | 1.34 | 4.6 % | 11 | 31 |
| zero | 278 | 1.31 | 4.5 % | 5 | 9 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 61 | 0.04 | 0.1 % | 1 | 1 |

Cuts: 61; in cut_and_commit 11.65 s = 40 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 9.23 s = 79 % of the cuts, C5-C9 2.42 s = 21 % of the cuts (8.3 % of the window).
Reclaim (C9) 1.34 s, of which the zero rewrite 1.31 s = 11 % of the cuts (4.5 % of the window).
