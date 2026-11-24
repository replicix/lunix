# M16 Part A (2026-11-24T16:23:35Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569356 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 29100.228284 IOPS, 0.12 GB/s, p99 574619.648 us, daemon CPU 84.200s (48.22 us/IO), rss 1155524 KiB, split lunix-coord=13.63(1) lunix-q3=6.81(1) lunix-q2=6.90(1) iou-wrk-1376628=0.50(1) iou-wrk-1376627=0.53(1) lunix-space-tim=0.36(1) lunix-space-lad=0.13(1) 
- partA/c64k/q4/w4k/rep2: 19172.413793 IOPS, 0.08 GB/s, p99 742391.808 us, daemon CPU 66.720s (58.00 us/IO), rss 1219348 KiB, split lunix-coord=13.87(1) lunix-q3=4.83(1) lunix-q2=4.89(1) iou-wrk-1376628=0.40(1) iou-wrk-1376627=0.42(1) lunix-space-tim=0.44(1) 
- partA/c64k/q4/w4k/rep3: 19231.293424 IOPS, 0.08 GB/s, p99 717225.984 us, daemon CPU 69.740s (61.46 us/IO), rss 1236468 KiB, split lunix-coord=14.42(1) lunix-q3=4.88(1) lunix-q2=5.01(1) iou-wrk-1376628=0.43(1) iou-wrk-1376627=0.46(1) lunix-space-tim=0.46(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
