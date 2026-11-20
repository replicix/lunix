# T6 tables disk: page cache dropped before every read cell, default 128 MiB read cache

## raw baseline (fio on the file system that holds the journal and the store)

| cell | IOPS median (min-max) | MB/s median | p50 ms | p99 ms |
|---|---|---|---|---|
| w4k | 2,230 (2,212-2,339) | 9.2 | 413.1 | 708.8 |
| r4k | 2,987 (2,983-2,993) | 12.4 | 329.3 | 700.4 |
| seqw | 60 (59-116) | 70.2 | 3,774.9 | 11,073.0 |
| seqr | 130 (120-134) | 143.5 | 476.1 | 17,112.8 |
| commit64k | 179 (165-262) | 11.8 | 1.3 | 2.9 |
| commit4k | 258 (256-259) | 1.1 | 0.8 | 1.0 |
| commit4k-x8 | 507 (487-517) | 2.1 | 0.8 | 6.1 |
| r4k-buffered | 8,535,095 (8,466,302-8,605,169) | 34,959.9 | 0.1 | 0.1 |
| seqr-buffered | 23,761 (23,588-23,991) | 24,923.8 | 10.4 | 10.6 |

## Lunix against raw

| chunk | queues | cell | working set | ring | Lunix median (min-max) | counterpart | ratio | p99 ms | max completion s | 1 s windows over 1 s | status probe max ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 16 KiB | 4 | r4k | 4 GiB | 1024 MiB | 3,693 IOPS (3,608 IOPS-3,885 IOPS) | 2,987 IOPS | 123.7 % | 2,139.1 | 0.0 | 0/0/0 | 5 |
| 16 KiB | 4 | seqr | 4 GiB | 1024 MiB | 2,627.1 MB/s (2,612.8 MB/s-2,655.1 MB/s) | 143.5 MB/s | 1,830.7 % | 1,069.5 | 0.0 | 0/0/0 | 119 |
| 64 KiB | 4 | r4k | 4 GiB | 1024 MiB | 148,992 IOPS (134,327 IOPS-166,834 IOPS) | 2,987 IOPS | 4,988.4 % | 49.5 | 0.0 | 0/0/0 | 5 |
| 64 KiB | 4 | seqr | 4 GiB | 1024 MiB | 5,671.5 MB/s (5,480.2 MB/s-5,769.5 MB/s) | 143.5 MB/s | 3,952.0 % | 1,027.6 | 0.0 | 0/0/0 | 94 |

## sustained writes against the model

| chunk | cell | B (raw seqw MB/s) | n writes per cut | cr | model | measured | measured / model |
|---|---|---|---|---|---|---|---|

## burst writes (ring not yet full), from the ops_write counter

| chunk | cell | ops before | ops at first sample | seconds since fio start | burst IOPS | sustained IOPS (fio) | journal used at first sample |
|---|---|---|---|---|---|---|---|

## per-cell detail

| cell | rep | IOPS | MB/s | commits | stage ms (hash; dedup; compress; seal; append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end | throttled ms / stops / delayed writes (over the cell) | gc pressure end |
|---|---|---|---|---|---|---|---|---|---|---|---|
| partA/c16k/q4/r4k/rep1 | 1 | 3,885 | 16.1 | 3 | 22; 39; 186; 27; 371 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep2 | 2 | 3,693 | 15.3 | 0 | 0; 0; 0; 0; 0 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep3 | 3 | 3,608 | 14.9 | 0 | 0; 0; 0; 0; 0 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep1 | 1 | 2,524 | 2,655.1 | 0 | 0; 0; 0; 0; 0 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep2 | 2 | 2,497 | 2,627.1 | 0 | 0; 0; 0; 0; 0 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep3 | 3 | 2,483 | 2,612.8 | 0 | 0; 0; 0; 0; 0 | 0 / 872 | 263847 / 0 | 66064320 / 549 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep1 | 1 | 148,992 | 610.4 | 3 | 14; 3; 109; 13; 242 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep2 | 2 | 134,327 | 550.3 | 0 | 0; 0; 0; 0; 0 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep3 | 3 | 166,834 | 683.5 | 0 | 0; 0; 0; 0; 0 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep1 | 1 | 5,494 | 5,769.5 | 0 | 0; 0; 0; 0; 0 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep2 | 2 | 5,400 | 5,671.5 | 0 | 0; 0; 0; 0; 0 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep3 | 3 | 5,218 | 5,480.2 | 0 | 0; 0; 0; 0; 0 | 0 / 3058 | 66452 / 0 | 9437760 / 219 | 134217728 | 0 / 0 / 0 | normal |

## counters over time (first repetition of the write cells, sampler)

