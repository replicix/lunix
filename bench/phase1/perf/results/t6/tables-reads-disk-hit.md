# T6 tables disk: read cache holds the working set (8 GiB), warmed

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
| 16 KiB | 4 | r4k | 4 GiB | 1024 MiB | 356,149 IOPS (355,016 IOPS-356,697 IOPS) | 251,881 IOPS | 141.4 % | 20.1 | 0.0 | 0/0/0 | 3 |
| 16 KiB | 8 | r4k | 4 GiB | 1024 MiB | 741,221 IOPS (737,600 IOPS-743,072 IOPS) | 3,188,286 IOPS | 23.2 % | 5.6 | 0.0 | 0/0/0 | 3 |
| 16 KiB | 4 | seqr | 4 GiB | 1024 MiB | 8,188.6 MB/s (8,107.3 MB/s-8,198.8 MB/s) | 12,874.5 MB/s | 63.6 % | 33.4 | 0.0 | 0/0/0 | 2 |
| 16 KiB | 8 | seqr | 4 GiB | 1024 MiB | 11,245.7 MB/s (11,108.0 MB/s-11,302.5 MB/s) | 43,978.6 MB/s | 25.6 % | 46.4 | 0.0 | 0/0/0 | 2 |
| 64 KiB | 4 | r4k | 4 GiB | 1024 MiB | 362,288 IOPS (349,994 IOPS-370,673 IOPS) | 251,881 IOPS | 143.8 % | 19.3 | 0.0 | 0/0/0 | 34 |
| 64 KiB | 8 | r4k | 4 GiB | 1024 MiB | 802,541 IOPS (798,078 IOPS-805,139 IOPS) | 3,188,286 IOPS | 25.2 % | 5.1 | 0.0 | 0/0/0 | 3 |
| 64 KiB | 4 | seqr | 4 GiB | 1024 MiB | 9,327.2 MB/s (9,193.4 MB/s-9,338.6 MB/s) | 12,874.5 MB/s | 72.4 % | 39.1 | 0.0 | 0/0/0 | 37 |
| 64 KiB | 8 | seqr | 4 GiB | 1024 MiB | 11,540.8 MB/s (11,179.7 MB/s-12,012.6 MB/s) | 43,978.6 MB/s | 26.2 % | 44.8 | 0.0 | 0/0/0 | 3 |

## sustained writes against the model

| chunk | cell | B (raw seqw MB/s) | n writes per cut | cr | model | measured | measured / model |
|---|---|---|---|---|---|---|---|

## burst writes (ring not yet full), from the ops_write counter

| chunk | cell | ops before | ops at first sample | seconds since fio start | burst IOPS | sustained IOPS (fio) | journal used at first sample |
|---|---|---|---|---|---|---|---|

## per-cell detail

| cell | rep | IOPS | MB/s | commits | stage ms (hash; dedup; compress; seal; append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end | throttled ms / stops / delayed writes (over the cell) | gc pressure end |
|---|---|---|---|---|---|---|---|---|---|---|---|
| partA/c16k/q4/r4k/rep1 | 1 | 355,016 | 1,454.3 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep2 | 2 | 356,149 | 1,458.9 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep3 | 3 | 356,697 | 1,461.2 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep1 | 1 | 737,600 | 3,021.3 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep2 | 2 | 743,072 | 3,043.8 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep3 | 3 | 741,221 | 3,036.2 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep1 | 1 | 7,811 | 8,198.8 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep2 | 2 | 7,801 | 8,188.6 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep3 | 3 | 7,723 | 8,107.3 | 0 | 0; 0; 0; 0; 0 | 0 / 874 | 262144 / 0 | 60821120 / 230 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep1 | 1 | 10,771 | 11,302.5 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep2 | 2 | 10,585 | 11,108.0 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep3 | 3 | 10,716 | 11,245.7 | 0 | 0; 0; 0; 0; 0 | 0 / 594 | 262144 / 0 | 46140160 / 146 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep1 | 1 | 349,994 | 1,433.7 | 5 | 21; 1; 178; 17; 1,319 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep2 | 2 | 370,673 | 1,518.4 | 0 | 0; 0; 0; 0; 0 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep3 | 3 | 362,288 | 1,484.1 | 0 | 0; 0; 0; 0; 0 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep1 | 1 | 798,078 | 3,269.1 | 1 | 2; 2; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep2 | 2 | 805,139 | 3,298.0 | 0 | 0; 0; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep3 | 3 | 802,541 | 3,287.3 | 0 | 0; 0; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep1 | 1 | 8,887 | 9,327.2 | 0 | 0; 0; 0; 0; 0 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep2 | 2 | 8,759 | 9,193.4 | 0 | 0; 0; 0; 0; 0 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep3 | 3 | 8,898 | 9,338.6 | 0 | 0; 0; 0; 0; 0 | 0 / 2958 | 336757 / 0 | 1048640 / 352 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep1 | 1 | 11,448 | 12,012.6 | 0 | 0; 0; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep2 | 2 | 10,998 | 11,540.8 | 0 | 0; 0; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep3 | 3 | 10,653 | 11,179.7 | 0 | 0; 0; 0; 0; 0 | 0 / 1339 | 73459 / 0 | 13632320 / 8 | 268435456 | 0 / 0 / 0 | normal |

## counters over time (first repetition of the write cells, sampler)

