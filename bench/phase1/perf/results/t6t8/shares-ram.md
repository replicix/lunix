## RAM partA_c64k_q4_w4k_rep1

uprobes: results/t6t8/writes-ram/raw/partA_c64k_q4_w4k_rep1/uprobes.txt (60.8 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 37 | 11.97 | 19.7 % | 323 | 597 |
| finish | 0 | 0 | 0 % | | |
| segseal | 1374 | 0.07 | 0.1 % | 0 | 1 |
| cwseal | 1438 | 0.46 | 0.8 % | 0 | 2 |
| reclaim | 69 | 0.62 | 1.0 % | 9 | 35 |
| zero | 142 | 0.57 | 0.9 % | 4 | 8 |
| gcurgent | 4 | 0.49 | 0.8 % | 121 | 206 |
| runurgent | 24 | 0.86 | 1.4 % | 36 | 366 |
| trueup | 37 | 0.00 | 0.0 % | 0 | 0 |

Cuts: 37; in cut_and_commit 11.97 s = 20 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 11.97 s = 100 % of the cuts (19.7 % of the window).
Reclaim (C9) 0.62 s, of which the zero rewrite 0.57 s = 5 % of the cuts (0.9 % of the window).
## RAM partA_c64k_q4_seqw_rep1

uprobes: results/t6t8/writes-ram/raw/partA_c64k_q4_seqw_rep1/uprobes.txt (27.5 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 68 | 6.88 | 25.0 % | 101 | 304 |
| finish | 0 | 0 | 0 % | | |
| segseal | 650 | 0.07 | 0.3 % | 0 | 0 |
| cwseal | 738 | 0.22 | 0.8 % | 0 | 2 |
| reclaim | 133 | 1.49 | 5.4 % | 11 | 37 |
| zero | 307 | 1.40 | 5.1 % | 5 | 9 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 67 | 0.02 | 0.1 % | 0 | 0 |

Cuts: 68; in cut_and_commit 6.88 s = 25 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 6.88 s = 100 % of the cuts (25.0 % of the window).
Reclaim (C9) 1.49 s, of which the zero rewrite 1.40 s = 20 % of the cuts (5.1 % of the window).
## RAM partA_c16k_q4_w4k_rep1

uprobes: results/t6t8/writes-ram/raw/partA_c16k_q4_w4k_rep1/uprobes.txt (60.8 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 53 | 18.56 | 30.5 % | 350 | 811 |
| finish | 0 | 0 | 0 % | | |
| segseal | 978 | 0.07 | 0.1 % | 0 | 0 |
| cwseal | 1142 | 0.75 | 1.2 % | 1 | 2 |
| reclaim | 89 | 0.86 | 1.4 % | 10 | 33 |
| zero | 192 | 0.78 | 1.3 % | 4 | 8 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 53 | 0.01 | 0.0 % | 0 | 0 |

Cuts: 53; in cut_and_commit 18.56 s = 31 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 18.56 s = 100 % of the cuts (30.5 % of the window).
Reclaim (C9) 0.86 s, of which the zero rewrite 0.78 s = 4 % of the cuts (1.3 % of the window).
## RAM partA_c16k_q4_seqw_rep1

uprobes: results/t6t8/writes-ram/raw/partA_c16k_q4_seqw_rep1/uprobes.txt (30.5 s of events)

| function | calls | total s | share of the window | mean ms | max ms |
|---|---|---|---|---|---|
| cut | 64 | 10.26 | 33.6 % | 160 | 449 |
| finish | 0 | 0 | 0 % | | |
| segseal | 575 | 0.04 | 0.1 % | 0 | 1 |
| cwseal | 645 | 0.42 | 1.4 % | 1 | 4 |
| reclaim | 126 | 1.28 | 4.2 % | 10 | 33 |
| zero | 274 | 1.20 | 3.9 % | 4 | 7 |
| gcurgent | 0 | 0 | 0 % | | |
| runurgent | 0 | 0 | 0 % | | |
| trueup | 65 | 0.04 | 0.1 % | 1 | 1 |

Cuts: 64; in cut_and_commit 10.26 s = 34 % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);
C1-C4 (to the return of Packer::finish) 0.00 s = 0 % of the cuts, C5-C9 10.26 s = 100 % of the cuts (33.6 % of the window).
Reclaim (C9) 1.28 s, of which the zero rewrite 1.20 s = 12 % of the cuts (3.9 % of the window).
