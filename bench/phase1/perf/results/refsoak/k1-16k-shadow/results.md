# M16 Part A (2026-11-24T13:37:14Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.96 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569404 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 43653.158482 IOPS, 0.18 GB/s, p99 463470.592 us, daemon CPU 94.340s (36.63 us/IO), rss 1419812 KiB, split lunix-coord=23.11(9) lunix-q2=9.04(1) lunix-q3=8.96(1) iou-wrk-1204266=0.70(1) iou-wrk-1204267=0.67(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 47600.693172 IOPS, 0.20 GB/s, p99 287309.824 us, daemon CPU 109.460s (38.33 us/IO), rss 1541968 KiB, split lunix-coord=28.70(1) lunix-q2=9.77(1) lunix-q3=9.63(1) iou-wrk-1204266=0.70(1) iou-wrk-1204267=0.65(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep3: 48538.291065 IOPS, 0.20 GB/s, p99 291504.128 us, daemon CPU 115.440s (40.31 us/IO), rss 1553068 KiB, split lunix-coord=31.20(1) lunix-q2=10.03(1) lunix-q3=9.97(1) iou-wrk-1204266=0.75(1) iou-wrk-1204267=0.71(1) lunix-space-tim=0.43(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
