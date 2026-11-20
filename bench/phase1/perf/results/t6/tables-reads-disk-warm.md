# T6 tables disk: warm page cache, default read cache

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
| 16 KiB | 4 | r4k | 4 GiB | 1024 MiB | 229,203 IOPS (228,662 IOPS-229,744 IOPS) | 8,535,095 IOPS | 2.7 % | 30.0 | 0.0 | 0/0 | 9 |
| 16 KiB | 4 | seqr | 4 GiB | 1024 MiB | 4,086.9 MB/s (4,060.9 MB/s-4,112.8 MB/s) | 24,923.8 MB/s | 16.4 % | 70.8 | 0.0 | 0/0 | 7 |
| 64 KiB | 4 | r4k | 4 GiB | 1024 MiB | 271,632 IOPS (271,153 IOPS-272,110 IOPS) | 8,535,095 IOPS | 3.2 % | 27.8 | 0.0 | 0/0 | 12 |
| 64 KiB | 4 | seqr | 4 GiB | 1024 MiB | 7,537.6 MB/s (7,537.4 MB/s-7,537.9 MB/s) | 24,923.8 MB/s | 30.2 % | 49.8 | 0.0 | 0/0 | 2 |

## sustained writes against the model

| chunk | cell | B (raw seqw MB/s) | n writes per cut | cr | model | measured | measured / model |
|---|---|---|---|---|---|---|---|

## burst writes (ring not yet full), from the ops_write counter

| chunk | cell | ops before | ops at first sample | seconds since fio start | burst IOPS | sustained IOPS (fio) | journal used at first sample |
|---|---|---|---|---|---|---|---|

## per-cell detail

| cell | rep | IOPS | MB/s | commits | stage ms (hash; dedup; compress; seal; append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end | throttled ms / stops / delayed writes (over the cell) | gc pressure end |
|---|---|---|---|---|---|---|---|---|---|---|---|
| partA/c16k/q4/r4k/rep1 | 1 | 228,662 | 936.7 | 4 | 24; 7; 205; 33; 1,424 | 0 / 4185 | 356258 / 0 | 15729600 / 57 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep2 | 2 | 229,744 | 941.2 | 0 | 0; 0; 0; 0; 0 | 0 / 4185 | 356258 / 0 | 15729600 / 57 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep1 | 1 | 3,914 | 4,112.8 | 0 | 0; 0; 0; 0; 0 | 0 / 4185 | 356258 / 0 | 15729600 / 57 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep2 | 2 | 3,865 | 4,060.9 | 0 | 0; 0; 0; 0; 0 | 0 / 4185 | 356258 / 0 | 15729600 / 57 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep1 | 1 | 272,110 | 1,114.7 | 3 | 19; 1; 186; 23; 148 | 0 / 848 | 194793 / 0 | 59772480 / 291 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep2 | 2 | 271,153 | 1,110.8 | 0 | 0; 0; 0; 0; 0 | 0 / 848 | 194793 / 0 | 59772480 / 291 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep1 | 1 | 7,180 | 7,537.4 | 0 | 0; 0; 0; 0; 0 | 0 / 848 | 194793 / 0 | 59772480 / 291 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep2 | 2 | 7,180 | 7,537.9 | 0 | 0; 0; 0; 0; 0 | 0 / 848 | 194793 / 0 | 59772480 / 291 | 67108864 | 0 / 0 / 0 | normal |

## counters over time (first repetition of the write cells, sampler)

