# M16 Part A (2026-11-22T23:08:08Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.63 138.65 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 138.65 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568864 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 31221.745682 IOPS, 0.13 GB/s, p99 480247.808 us, daemon CPU 73.090s (39.02 us/IO), rss 1181304 KiB, split lunix-coord=12.98(1) lunix-q2=6.24(1) lunix-q3=6.19(1) iou-wrk-872493=0.47(1) iou-wrk-872494=0.45(1) lunix-space-lad=0.30(1) lunix-space-tim=0.23(1) 
- partA/c64k/q4/w4k/rep2: 24153.917539 IOPS, 0.10 GB/s, p99 549453.824 us, daemon CPU 69.520s (47.97 us/IO), rss 1211424 KiB, split lunix-coord=13.25(1) lunix-q2=5.50(1) lunix-q3=5.40(1) iou-wrk-872493=0.42(1) iou-wrk-872494=0.42(1) lunix-space-tim=0.38(1) lunix-space-lad=0.11(1) 
- partA/c64k/q4/w4k/rep3: 23876.419351 IOPS, 0.10 GB/s, p99 541065.216 us, daemon CPU 68.250s (47.64 us/IO), rss 1243044 KiB, split lunix-coord=13.45(1) lunix-q2=5.65(1) lunix-q3=5.62(1) iou-wrk-872493=0.42(1) iou-wrk-872494=0.41(1) lunix-space-tim=0.42(1) lunix-space-lad=0.10(1) 
- partA/c64k/q4/w4k/rep4: 27477.795176 IOPS, 0.11 GB/s, p99 599785.472 us, daemon CPU 73.400s (44.52 us/IO), rss 1293104 KiB, split lunix-coord=13.83(1) lunix-q2=6.01(1) lunix-q3=5.90(1) iou-wrk-872493=0.48(1) iou-wrk-872494=0.45(1) lunix-space-tim=0.38(1) lunix-space-lad=0.28(1) 
- partA/c64k/q4/w4k/rep5: 25679.636079 IOPS, 0.11 GB/s, p99 549453.824 us, daemon CPU 73.650s (47.80 us/IO), rss 1318948 KiB, split lunix-coord=14.77(1) lunix-q2=5.90(1) lunix-q3=5.90(1) iou-wrk-872493=0.46(1) iou-wrk-872494=0.44(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/seqw/rep1: 565.881202 IOPS, 0.60 GB/s, p99 2365587.456 us, daemon CPU 34.460s (1964.39 us/IO), rss 1538380 KiB, split lunix-coord=7.13(1) lunix-q2=2.89(1) lunix-q3=2.48(1) iou-wrk-872493=0.76(1) iou-wrk-872494=0.68(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 643.366925 IOPS, 0.68 GB/s, p99 2936012.8 us, daemon CPU 41.460s (2013.82 us/IO), rss 1723728 KiB, split lunix-coord=8.13(1) lunix-q2=3.79(1) lunix-q3=2.99(1) iou-wrk-872493=0.90(1) iou-wrk-872494=0.81(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 681.190682 IOPS, 0.72 GB/s, p99 2600468.48 us, daemon CPU 46.510s (2133.67 us/IO), rss 1710956 KiB, split lunix-coord=8.87(1) lunix-q2=4.24(1) lunix-q3=3.28(1) iou-wrk-872493=1.01(1) iou-wrk-872494=0.88(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 740.002555 IOPS, 0.78 GB/s, p99 2088763.392 us, daemon CPU 51.540s (2246.72 us/IO), rss 1744984 KiB, split lunix-coord=9.84(1) lunix-q2=4.30(1) lunix-q3=3.37(1) iou-wrk-872493=1.08(1) iou-wrk-872494=0.94(1) lunix-space-tim=0.11(1) 
- partA/c64k/q4/seqw/rep5: 709.737147 IOPS, 0.75 GB/s, p99 2432696.32 us, daemon CPU 46.890s (2278.16 us/IO), rss 1814160 KiB, split lunix-coord=8.84(1) lunix-q2=4.07(1) lunix-q3=3.03(1) iou-wrk-872493=0.97(1) iou-wrk-872494=0.84(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
