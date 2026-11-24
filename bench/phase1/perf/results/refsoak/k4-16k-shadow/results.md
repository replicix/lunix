# M16 Part A (2026-11-24T15:00:19Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.01 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569428 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44163.688561 IOPS, 0.18 GB/s, p99 455081.984 us, daemon CPU 97.100s (36.64 us/IO), rss 1441252 KiB, split lunix-coord=24.41(1) lunix-q3=9.34(1) lunix-q2=9.34(1) iou-wrk-1290591=0.65(1) iou-wrk-1290589=0.67(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep2: 44579.359928 IOPS, 0.18 GB/s, p99 304087.04 us, daemon CPU 105.080s (39.29 us/IO), rss 1553172 KiB, split lunix-coord=29.16(1) lunix-q3=9.39(1) lunix-q2=9.42(1) iou-wrk-1290591=0.64(1) iou-wrk-1290589=0.69(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 48917.03326 IOPS, 0.20 GB/s, p99 235929.6 us, daemon CPU 121.200s (41.29 us/IO), rss 1578796 KiB, split lunix-coord=31.94(1) lunix-q3=10.29(1) lunix-q2=10.29(1) iou-wrk-1290591=0.70(1) iou-wrk-1290589=0.73(1) lunix-space-tim=0.44(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
