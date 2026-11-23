# M16 Part A (2026-11-23T15:27:32Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.95 88.37 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 88.37 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569144 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 29508.973355 IOPS, 0.12 GB/s, p99 526385.152 us, daemon CPU 68.270s (38.56 us/IO), rss 1203868 KiB, split lunix-coord=12.56(9) lunix-q2=5.96(1) lunix-q3=5.95(1) iou-wrk-2108831=0.46(1) iou-wrk-2108832=0.42(1) lunix-space-lad=0.35(1) lunix-space-tim=0.28(1) 
- partA/c64k/q4/w4k/rep2: 24784.606156 IOPS, 0.10 GB/s, p99 599785.472 us, daemon CPU 69.930s (47.82 us/IO), rss 1244200 KiB, split lunix-coord=14.49(1) lunix-q2=5.60(1) lunix-q3=5.51(1) iou-wrk-2108831=0.47(1) iou-wrk-2108832=0.44(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep3: 24680.267814 IOPS, 0.10 GB/s, p99 583008.256 us, daemon CPU 75.920s (51.27 us/IO), rss 1267608 KiB, split lunix-coord=14.35(1) lunix-q2=5.96(1) lunix-q3=5.97(1) iou-wrk-2108831=0.54(1) iou-wrk-2108832=0.53(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/w4k/rep4: 22059.815389 IOPS, 0.09 GB/s, p99 666894.336 us, daemon CPU 73.090s (55.22 us/IO), rss 1286956 KiB, split lunix-coord=14.31(1) lunix-q2=5.45(1) lunix-q3=5.47(1) iou-wrk-2108831=0.47(1) iou-wrk-2108832=0.45(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/w4k/rep5: 22963.283801 IOPS, 0.09 GB/s, p99 641728.512 us, daemon CPU 76.600s (56.54 us/IO), rss 1337736 KiB, split lunix-coord=15.40(1) lunix-q2=5.95(1) lunix-q3=5.96(1) iou-wrk-2108831=0.52(1) iou-wrk-2108832=0.51(1) lunix-space-tim=0.44(1) 
- partA/c64k/q4/seqw/rep1: 665.297206 IOPS, 0.71 GB/s, p99 1786773.504 us, daemon CPU 40.170s (1947.71 us/IO), rss 1682880 KiB, split lunix-coord=7.99(1) lunix-q2=3.28(1) lunix-q3=2.68(1) iou-wrk-2108831=0.89(1) iou-wrk-2108832=0.74(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 660.749128 IOPS, 0.70 GB/s, p99 2902458.368 us, daemon CPU 40.460s (2041.12 us/IO), rss 1351364 KiB, split lunix-coord=8.02(1) lunix-q2=3.36(1) lunix-q3=2.87(1) iou-wrk-2108831=0.88(1) iou-wrk-2108832=0.73(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 669.323092 IOPS, 0.71 GB/s, p99 3036676.096 us, daemon CPU 42.690s (2126.03 us/IO), rss 1774856 KiB, split lunix-coord=8.12(1) lunix-q2=3.51(1) lunix-q3=2.92(1) iou-wrk-2108831=0.91(1) iou-wrk-2108832=0.74(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 701.498805 IOPS, 0.74 GB/s, p99 2600468.48 us, daemon CPU 45.600s (2096.89 us/IO), rss 1812508 KiB, split lunix-coord=8.33(1) lunix-q2=3.69(1) lunix-q3=3.06(1) iou-wrk-2108831=0.96(1) iou-wrk-2108832=0.78(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 684.222293 IOPS, 0.73 GB/s, p99 2734686.208 us, daemon CPU 46.670s (2200.28 us/IO), rss 1794840 KiB, split lunix-coord=8.47(1) lunix-q2=3.75(1) lunix-q3=3.03(1) iou-wrk-2108831=0.97(1) iou-wrk-2108832=0.77(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
