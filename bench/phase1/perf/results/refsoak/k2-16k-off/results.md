# M16 Part A (2026-11-24T14:04:58Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.09 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569272 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44390.80097 IOPS, 0.18 GB/s, p99 463470.592 us, daemon CPU 95.240s (35.76 us/IO), rss 1363920 KiB, split lunix-coord=22.03(1) lunix-q3=9.48(1) lunix-q2=9.51(1) iou-wrk-1232999=0.67(1) iou-wrk-1232998=0.71(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep2: 49103.530352 IOPS, 0.20 GB/s, p99 278921.216 us, daemon CPU 112.300s (38.76 us/IO), rss 1524940 KiB, split lunix-coord=29.52(1) lunix-q3=9.88(1) lunix-q2=10.01(1) iou-wrk-1232999=0.68(1) iou-wrk-1232998=0.75(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 48626.955696 IOPS, 0.20 GB/s, p99 267386.88 us, daemon CPU 118.720s (41.38 us/IO), rss 1565216 KiB, split lunix-coord=31.20(1) lunix-q3=10.05(1) lunix-q2=10.11(1) iou-wrk-1232998=0.74(1) iou-wrk-1232999=0.69(1) lunix-space-tim=0.45(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
