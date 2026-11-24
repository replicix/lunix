# M16 Part A (2026-11-24T16:51:11Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569408 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 26208.981289 IOPS, 0.11 GB/s, p99 557842.432 us, daemon CPU 67.260s (42.77 us/IO), rss 1177156 KiB, split lunix-coord=11.57(1) lunix-q3=5.81(1) lunix-q2=5.81(1) iou-wrk-1405171=0.43(1) iou-wrk-1405170=0.45(1) lunix-space-tim=0.31(1) lunix-space-lad=0.22(1) 
- partA/c64k/q4/w4k/rep2: 17742.133972 IOPS, 0.07 GB/s, p99 859832.32 us, daemon CPU 57.900s (54.39 us/IO), rss 1225204 KiB, split lunix-coord=12.20(1) lunix-q3=4.32(1) lunix-q2=4.32(1) iou-wrk-1405171=0.42(1) iou-wrk-1405170=0.43(1) lunix-space-tim=0.39(1) 
- partA/c64k/q4/w4k/rep3: 24967.146917 IOPS, 0.10 GB/s, p99 574619.648 us, daemon CPU 70.090s (46.79 us/IO), rss 1255636 KiB, split lunix-coord=13.39(1) lunix-q3=5.75(1) lunix-q2=5.80(1) iou-wrk-1405171=0.47(1) iou-wrk-1405170=0.50(1) lunix-space-tim=0.42(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
