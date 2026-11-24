# M16 Part A (2026-11-24T05:16:32Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.96 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569228 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44537.716828 IOPS, 0.18 GB/s, p99 371195.904 us, daemon CPU 94.090s (35.21 us/IO), rss 1407004 KiB, split lunix-coord=24.32(9) lunix-q2=9.06(1) lunix-q3=9.06(1) iou-wrk-210232=0.72(1) iou-wrk-210233=0.69(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 44897.018429 IOPS, 0.18 GB/s, p99 299892.736 us, daemon CPU 102.320s (38.63 us/IO), rss 1539636 KiB, split lunix-coord=28.93(1) lunix-q2=9.39(1) lunix-q3=9.41(1) iou-wrk-210232=0.70(1) iou-wrk-210233=0.65(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep3: 47407.561695 IOPS, 0.19 GB/s, p99 299892.736 us, daemon CPU 114.770s (40.35 us/IO), rss 1587436 KiB, split lunix-coord=30.20(1) lunix-q2=9.92(1) lunix-q3=9.81(1) iou-wrk-210232=0.78(1) iou-wrk-210233=0.72(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep4: 47919.682656 IOPS, 0.20 GB/s, p99 287309.824 us, daemon CPU 122.330s (42.55 us/IO), rss 1600552 KiB, split lunix-coord=30.53(1) lunix-q2=9.79(1) lunix-q3=9.79(1) iou-wrk-210232=0.74(1) iou-wrk-210233=0.69(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep5: 46163.381074 IOPS, 0.19 GB/s, p99 274726.912 us, daemon CPU 126.180s (45.56 us/IO), rss 1618660 KiB, split lunix-coord=29.10(1) lunix-q2=9.95(1) lunix-q3=9.78(1) iou-wrk-210232=0.70(1) iou-wrk-210233=0.66(1) lunix-space-tim=0.43(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
