# M16 Part A (2026-11-24T15:55:59Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.19 1.96 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569296 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 22921.615677 IOPS, 0.09 GB/s, p99 549453.824 us, daemon CPU 55.480s (40.34 us/IO), rss 1156440 KiB, split lunix-coord=9.46(1) lunix-q3=4.80(1) lunix-q2=4.81(1) iou-wrk-1348472=0.35(1) iou-wrk-1348471=0.36(1) lunix-space-lad=0.45(1) lunix-space-tim=0.27(1) 
- partA/c64k/q4/w4k/rep2: 19274.767506 IOPS, 0.08 GB/s, p99 759169.024 us, daemon CPU 66.760s (58.71 us/IO), rss 1234008 KiB, split lunix-coord=13.78(1) lunix-q3=5.17(1) lunix-q2=5.25(1) iou-wrk-1348472=0.45(1) iou-wrk-1348471=0.47(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/w4k/rep3: 22515.938077 IOPS, 0.09 GB/s, p99 675282.944 us, daemon CPU 79.130s (59.57 us/IO), rss 1259632 KiB, split lunix-coord=15.89(1) lunix-q3=5.74(1) lunix-q2=5.77(1) iou-wrk-1348472=0.51(1) iou-wrk-1348471=0.53(1) lunix-space-tim=0.45(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
