# M16 Part A (2026-11-24T15:28:05Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.04 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569360 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 24133.406652 IOPS, 0.10 GB/s, p99 541065.216 us, daemon CPU 56.090s (38.74 us/IO), rss 1201760 KiB, split lunix-coord=9.30(1) lunix-q3=5.14(1) lunix-q2=5.17(1) iou-wrk-1319590=0.38(1) iou-wrk-1319589=0.40(1) lunix-space-lad=0.36(1) lunix-space-tim=0.27(1) 
- partA/c64k/q4/w4k/rep2: 24601.223526 IOPS, 0.10 GB/s, p99 650117.12 us, daemon CPU 69.090s (47.60 us/IO), rss 1226060 KiB, split lunix-coord=13.15(1) lunix-q3=5.58(1) lunix-q2=5.55(1) iou-wrk-1319590=0.44(1) iou-wrk-1319589=0.46(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep3: 26741.635195 IOPS, 0.11 GB/s, p99 599785.472 us, daemon CPU 80.340s (50.07 us/IO), rss 1274300 KiB, split lunix-coord=15.64(1) lunix-q3=6.11(1) lunix-q2=6.21(1) iou-wrk-1319590=0.53(1) iou-wrk-1319589=0.57(1) lunix-space-tim=0.41(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
