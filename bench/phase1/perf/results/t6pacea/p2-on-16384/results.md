# M16 Part A (2026-11-22T22:23:43Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 5.26 139.12 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 139.12 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568904 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 40058.853915 IOPS, 0.16 GB/s, p99 541065.216 us, daemon CPU 87.030s (36.82 us/IO), rss 1408892 KiB, split lunix-coord=22.16(1) lunix-q3=8.30(1) lunix-q2=8.51(1) iou-wrk-700073=0.60(1) iou-wrk-700072=0.63(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep2: 42738.877595 IOPS, 0.18 GB/s, p99 299892.736 us, daemon CPU 102.090s (39.81 us/IO), rss 1529052 KiB, split lunix-coord=27.19(1) lunix-q3=9.14(1) lunix-q2=9.32(1) iou-wrk-700073=0.63(1) iou-wrk-700072=0.66(1) lunix-space-tim=0.47(1) 
- partA/c16k/q4/w4k/rep3: 41500.308267 IOPS, 0.17 GB/s, p99 404750.336 us, daemon CPU 104.210s (41.85 us/IO), rss 1511956 KiB, split lunix-coord=28.19(1) lunix-q3=9.27(1) lunix-q2=9.34(1) iou-wrk-700073=0.64(1) iou-wrk-700072=0.66(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep4: 42709.591415 IOPS, 0.18 GB/s, p99 346030.08 us, daemon CPU 102.920s (40.84 us/IO), rss 1547456 KiB, split lunix-coord=25.61(1) lunix-q2=8.96(1) lunix-q3=8.80(1) iou-wrk-700073=0.61(1) iou-wrk-700072=0.64(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep5: 41276.929481 IOPS, 0.17 GB/s, p99 413138.944 us, daemon CPU 108.480s (43.80 us/IO), rss 1559636 KiB, split lunix-coord=26.52(1) lunix-q2=8.83(1) lunix-q3=8.75(1) iou-wrk-700073=0.63(1) iou-wrk-700072=0.63(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/seqw/rep1: 555.759923 IOPS, 0.59 GB/s, p99 1652555.776 us, daemon CPU 58.600s (3514.71 us/IO), rss 1842028 KiB, split lunix-coord=11.93(1) lunix-q2=2.76(1) lunix-q3=2.73(1) iou-wrk-700072=0.74(1) iou-wrk-700073=0.64(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep2: 627.472346 IOPS, 0.67 GB/s, p99 1400897.536 us, daemon CPU 68.610s (3770.46 us/IO), rss 1757656 KiB, split lunix-coord=14.54(1) lunix-q2=2.95(1) lunix-q3=2.94(1) iou-wrk-700072=0.82(1) iou-wrk-700073=0.73(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep3: 597.377721 IOPS, 0.63 GB/s, p99 1568669.696 us, daemon CPU 66.390s (3832.27 us/IO), rss 1838572 KiB, split lunix-coord=13.93(1) lunix-q2=2.71(1) lunix-q3=2.81(1) iou-wrk-700072=0.78(1) iou-wrk-700073=0.69(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep4: 578.348324 IOPS, 0.61 GB/s, p99 1266679.808 us, daemon CPU 68.120s (3926.12 us/IO), rss 1719836 KiB, split lunix-coord=14.21(1) lunix-q3=2.73(1) lunix-q2=2.61(1) iou-wrk-700072=0.76(1) iou-wrk-700073=0.68(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep5: 587.897184 IOPS, 0.62 GB/s, p99 1484783.616 us, daemon CPU 70.630s (4004.67 us/IO), rss 1774088 KiB, split lunix-coord=13.07(1) lunix-q3=2.83(1) lunix-q2=2.57(1) iou-wrk-700072=0.75(1) iou-wrk-700073=0.69(1) lunix-space-tim=0.11(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
