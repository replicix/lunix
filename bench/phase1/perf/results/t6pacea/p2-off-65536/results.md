# M16 Part A (2026-11-22T22:57:03Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.86 138.51 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 138.51 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568944 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 19579.961689 IOPS, 0.08 GB/s, p99 480247.808 us, daemon CPU 44.790s (38.13 us/IO), rss 951384 KiB, split lunix-coord=7.17(1) lunix-q2=3.76(1) lunix-q3=3.76(1) lunix-space-lad=0.71(1) iou-wrk-827470=0.28(1) iou-wrk-827471=0.27(1) lunix-space-tim=0.16(1) 
- partA/c64k/q4/w4k/rep2: 23309.459932 IOPS, 0.10 GB/s, p99 513802.24 us, daemon CPU 55.540s (39.71 us/IO), rss 1188268 KiB, split lunix-coord=10.41(1) lunix-q2=4.59(1) lunix-q3=4.46(1) lunix-space-lad=1.02(1) iou-wrk-827470=0.34(1) iou-wrk-827471=0.31(1) lunix-space-tim=0.23(1) 
- partA/c64k/q4/w4k/rep3: 31833.781303 IOPS, 0.13 GB/s, p99 392167.424 us, daemon CPU 82.480s (43.18 us/IO), rss 1248528 KiB, split lunix-coord=15.61(1) lunix-q2=6.54(1) lunix-q3=6.50(1) lunix-space-lad=0.64(1) iou-wrk-827470=0.49(1) iou-wrk-827471=0.47(1) lunix-space-tim=0.30(1) 
- partA/c64k/q4/w4k/rep4: 23456.173374 IOPS, 0.10 GB/s, p99 566231.04 us, daemon CPU 62.850s (44.66 us/IO), rss 1284700 KiB, split lunix-coord=12.26(1) lunix-q2=4.73(1) lunix-q3=4.80(1) lunix-space-lad=0.80(1) iou-wrk-827470=0.39(1) iou-wrk-827471=0.38(1) lunix-space-tim=0.28(1) 
- partA/c64k/q4/w4k/rep5: 31765.091041 IOPS, 0.13 GB/s, p99 480247.808 us, daemon CPU 84.410s (44.29 us/IO), rss 1333008 KiB, split lunix-coord=16.24(1) lunix-q2=6.53(1) lunix-q3=6.51(1) lunix-space-lad=0.75(1) iou-wrk-827470=0.49(1) iou-wrk-827471=0.47(1) lunix-space-tim=0.33(1) 
- partA/c64k/q4/seqw/rep1: 699.077872 IOPS, 0.74 GB/s, p99 2801795.072 us, daemon CPU 41.690s (1987.86 us/IO), rss 1733252 KiB, split lunix-coord=8.56(1) lunix-q2=3.54(1) lunix-q3=3.05(1) iou-wrk-827470=1.00(1) iou-wrk-827471=0.86(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 734.183978 IOPS, 0.78 GB/s, p99 2088763.392 us, daemon CPU 48.240s (2053.30 us/IO), rss 1657688 KiB, split lunix-coord=9.67(1) lunix-q2=4.01(1) lunix-q3=3.39(1) iou-wrk-827470=1.15(1) iou-wrk-827471=0.95(1) lunix-space-tim=0.11(1) 
- partA/c64k/q4/seqw/rep3: 683.117549 IOPS, 0.72 GB/s, p99 2004877.312 us, daemon CPU 43.990s (2146.53 us/IO), rss 1704852 KiB, split lunix-coord=8.66(1) lunix-q2=3.68(1) lunix-q3=3.17(1) iou-wrk-827470=1.00(1) iou-wrk-827471=0.85(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 685.273607 IOPS, 0.73 GB/s, p99 2432696.32 us, daemon CPU 44.900s (2113.59 us/IO), rss 1586664 KiB, split lunix-coord=8.47(1) lunix-q2=3.66(1) lunix-q3=3.27(1) iou-wrk-827470=0.99(1) iou-wrk-827471=0.83(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 728.80348 IOPS, 0.77 GB/s, p99 2298478.592 us, daemon CPU 49.520s (2123.34 us/IO), rss 1799080 KiB, split lunix-coord=8.48(1) lunix-q2=3.95(1) lunix-q3=3.51(1) iou-wrk-827470=1.09(1) iou-wrk-827471=0.92(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
