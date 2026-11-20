# T6 tables RAM (tmpfs): read cache holds the working set (4 GiB), warmed

## raw baseline (fio on the file system that holds the journal and the store)

| cell | IOPS median (min-max) | MB/s median | p50 ms | p99 ms |
|---|---|---|---|---|
| w4k | 1,832,157 (1,821,367-1,842,948) | 7,504.6 | 0.3 | 4.0 |
| r4k | 7,990,099 (7,962,842-8,017,356) | 32,727.6 | 0.1 | 0.2 |
| seqw | 9,306 (9,213-9,400) | 9,767.1 | 26.5 | 57.1 |
| seqr | 25,351 (25,306-25,396) | 26,591.3 | 10.0 | 10.9 |
| commit64k | 201,761 (200,514-203,007) | 13,222.6 | 0.0 | 0.0 |
| commit4k | 1,770,521 (1,750,705-1,790,336) | 7,252.1 | 0.0 | 0.0 |
| commit4k-x8 | 2,583,773 (2,564,796-2,602,749) | 10,583.1 | 0.0 | 0.0 |

## Lunix against raw

| chunk | queues | cell | working set | ring | Lunix median (min-max) | counterpart | ratio | p99 ms | max completion s | 1 s windows over 1 s | status probe max ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 16 KiB | 4 | r4k | 2 GiB | 1024 MiB | 357,260 IOPS (357,258 IOPS-358,869 IOPS) | 251,881 IOPS | 141.8 % | 19.5 | 0.0 | 0/0/0 | 3 |
| 16 KiB | 8 | r4k | 2 GiB | 1024 MiB | 789,738 IOPS (763,179 IOPS-791,405 IOPS) | 3,188,286 IOPS | 24.8 % | 5.3 | 0.0 | 0/0/0 | 3 |
| 16 KiB | 4 | seqr | 2 GiB | 1024 MiB | 8,585.8 MB/s (8,489.4 MB/s-8,653.9 MB/s) | 12,874.5 MB/s | 66.7 % | 31.9 | 0.0 | 0/0/0 | 3 |
| 16 KiB | 8 | seqr | 2 GiB | 1024 MiB | 11,450.6 MB/s (11,434.3 MB/s-11,561.0 MB/s) | 43,978.6 MB/s | 26.0 % | 45.9 | 0.0 | 0/0/0 | 2 |
| 64 KiB | 4 | r4k | 2 GiB | 1024 MiB | 374,371 IOPS (372,064 IOPS-374,475 IOPS) | 251,881 IOPS | 148.6 % | 19.3 | 0.0 | 0/0/0 | 2 |
| 64 KiB | 8 | r4k | 2 GiB | 1024 MiB | 897,423 IOPS (890,047 IOPS-898,602 IOPS) | 3,188,286 IOPS | 28.1 % | 4.4 | 0.0 | 0/0/0 | 3 |
| 64 KiB | 4 | seqr | 2 GiB | 1024 MiB | 9,967.2 MB/s (9,848.9 MB/s-10,039.3 MB/s) | 12,874.5 MB/s | 77.4 % | 27.7 | 0.0 | 0/0/0 | 3 |
| 64 KiB | 8 | seqr | 2 GiB | 1024 MiB | 11,911.3 MB/s (11,582.8 MB/s-11,966.6 MB/s) | 43,978.6 MB/s | 27.1 % | 43.8 | 0.0 | 0/0/0 | 2 |

## sustained writes against the model

| chunk | cell | B (raw seqw MB/s) | n writes per cut | cr | model | measured | measured / model |
|---|---|---|---|---|---|---|---|

## burst writes (ring not yet full), from the ops_write counter

| chunk | cell | ops before | ops at first sample | seconds since fio start | burst IOPS | sustained IOPS (fio) | journal used at first sample |
|---|---|---|---|---|---|---|---|

## per-cell detail

| cell | rep | IOPS | MB/s | commits | stage ms (hash; dedup; compress; seal; append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end | throttled ms / stops / delayed writes (over the cell) | gc pressure end |
|---|---|---|---|---|---|---|---|---|---|---|---|
| partA/c16k/q4/r4k/rep1 | 1 | 357,260 | 1,463.5 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep2 | 2 | 357,258 | 1,463.5 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/r4k/rep3 | 3 | 358,869 | 1,470.1 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep1 | 1 | 763,179 | 3,126.1 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep2 | 2 | 789,738 | 3,234.9 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/r4k/rep3 | 3 | 791,405 | 3,241.7 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep1 | 1 | 8,245 | 8,653.9 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep2 | 2 | 8,180 | 8,585.8 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q4/seqr/rep3 | 3 | 8,088 | 8,489.4 | 0 | 0; 0; 0; 0; 0 | 0 / 175 | 131072 / 0 | 32507840 / 18 | 67108864 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep1 | 1 | 11,017 | 11,561.0 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep2 | 2 | 10,912 | 11,450.6 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c16k/q8/seqr/rep3 | 3 | 10,896 | 11,434.3 | 0 | 0; 0; 0; 0; 0 | 0 / 94 | 131072 / 0 | 33556480 / 2 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep1 | 1 | 372,064 | 1,524.1 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep2 | 2 | 374,371 | 1,533.6 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/r4k/rep3 | 3 | 374,475 | 1,534.0 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep1 | 1 | 898,602 | 3,680.8 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep2 | 2 | 897,423 | 3,676.0 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/r4k/rep3 | 3 | 890,047 | 3,645.8 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep1 | 1 | 9,566 | 10,039.3 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep2 | 2 | 9,497 | 9,967.2 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q4/seqr/rep3 | 3 | 9,384 | 9,848.9 | 0 | 0; 0; 0; 0; 0 | 0 / 135 | 32768 / 0 | 180366080 / 56 | 67108864 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep1 | 1 | 11,351 | 11,911.3 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep2 | 2 | 11,038 | 11,582.8 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |
| partA/c64k/q8/seqr/rep3 | 3 | 11,404 | 11,966.6 | 0 | 0; 0; 0; 0; 0 | 0 / 90 | 32768 / 0 | 33556480 / 1 | 134217728 | 0 / 0 / 0 | normal |

## counters over time (first repetition of the write cells, sampler)

