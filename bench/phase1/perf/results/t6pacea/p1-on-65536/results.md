# M16 Part A (2026-11-22T21:50:20Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.69 124.49 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 124.49 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568888 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 18417.181284 IOPS, 0.08 GB/s, p99 574619.648 us, daemon CPU 42.640s (38.59 us/IO), rss 934476 KiB, split lunix-coord=7.12(1) lunix-q3=3.82(1) lunix-q2=3.83(1) iou-wrk-572942=0.29(1) iou-wrk-572941=0.30(1) lunix-space-lad=0.39(1) lunix-space-tim=0.26(1) 
- partA/c64k/q4/w4k/rep2: 22739.156377 IOPS, 0.09 GB/s, p99 725614.592 us, daemon CPU 64.800s (48.30 us/IO), rss 1219580 KiB, split lunix-coord=13.23(1) lunix-q3=4.85(1) lunix-q2=4.89(1) iou-wrk-572942=0.38(1) iou-wrk-572941=0.40(1) lunix-space-tim=0.40(1) 
- partA/c64k/q4/w4k/rep3: 16127.412678 IOPS, 0.07 GB/s, p99 918552.576 us, daemon CPU 56.680s (59.57 us/IO), rss 1243512 KiB, split lunix-coord=12.53(1) lunix-q3=4.04(1) lunix-q2=4.03(1) iou-wrk-572942=0.36(1) iou-wrk-572941=0.38(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep4: 18073.075012 IOPS, 0.07 GB/s, p99 792723.456 us, daemon CPU 57.730s (53.24 us/IO), rss 1274960 KiB, split lunix-coord=12.38(1) lunix-q3=4.50(1) lunix-q2=4.51(1) iou-wrk-572942=0.39(1) iou-wrk-572941=0.39(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/w4k/rep5: 21352.887412 IOPS, 0.09 GB/s, p99 826277.888 us, daemon CPU 66.530s (51.93 us/IO), rss 1305344 KiB, split lunix-coord=13.47(1) lunix-q3=5.03(1) lunix-q2=5.08(1) iou-wrk-572942=0.45(1) iou-wrk-572941=0.47(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/seqw/rep1: 615.091842 IOPS, 0.65 GB/s, p99 2399141.888 us, daemon CPU 36.610s (2052.40 us/IO), rss 1529668 KiB, split lunix-coord=7.90(1) lunix-q2=2.93(1) lunix-q3=2.68(1) iou-wrk-572941=0.84(1) iou-wrk-572942=0.73(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep2: 708.444094 IOPS, 0.75 GB/s, p99 1753219.072 us, daemon CPU 43.210s (2103.20 us/IO), rss 1698260 KiB, split lunix-coord=8.71(1) lunix-q2=3.58(1) lunix-q3=3.28(1) iou-wrk-572941=1.02(1) iou-wrk-572942=0.84(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep3: 678.850282 IOPS, 0.72 GB/s, p99 2399141.888 us, daemon CPU 44.530s (2049.88 us/IO), rss 1557840 KiB, split lunix-coord=8.31(1) lunix-q2=3.60(1) lunix-q3=3.41(1) iou-wrk-572941=1.01(1) iou-wrk-572942=0.85(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 734.484048 IOPS, 0.78 GB/s, p99 2533359.616 us, daemon CPU 49.040s (2086.50 us/IO), rss 1758384 KiB, split lunix-coord=8.44(1) lunix-q2=4.07(1) lunix-q3=3.54(1) iou-wrk-572941=1.11(1) iou-wrk-572942=0.90(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 655.952751 IOPS, 0.70 GB/s, p99 3137339.392 us, daemon CPU 45.380s (2231.67 us/IO), rss 1595848 KiB, split lunix-coord=8.31(1) lunix-q2=3.58(1) lunix-q3=3.38(1) iou-wrk-572941=0.97(1) iou-wrk-572942=0.80(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
