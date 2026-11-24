# M16 Part A (2026-11-24T06:19:58Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.01 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569208 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 19971.108667 IOPS, 0.08 GB/s, p99 591396.864 us, daemon CPU 47.450s (39.60 us/IO), rss 1077036 KiB, split lunix-coord=7.62(1) lunix-q2=4.33(1) lunix-q3=4.31(1) iou-wrk-279765=0.34(1) iou-wrk-279766=0.34(1) lunix-space-lad=0.47(1) lunix-space-tim=0.28(1) 
- partA/c64k/q4/w4k/rep2: 25756.788666 IOPS, 0.11 GB/s, p99 616562.688 us, daemon CPU 73.280s (47.42 us/IO), rss 1227076 KiB, split lunix-coord=13.56(1) lunix-q2=5.74(1) lunix-q3=5.76(1) iou-wrk-279765=0.46(1) iou-wrk-279766=0.45(1) lunix-space-tim=0.40(1) 
- partA/c64k/q4/w4k/rep3: 15539.572175 IOPS, 0.06 GB/s, p99 1115684.864 us, daemon CPU 53.490s (58.34 us/IO), rss 1254236 KiB, split lunix-coord=10.84(1) lunix-q2=3.92(1) lunix-q3=3.91(1) iou-wrk-279765=0.39(1) iou-wrk-279766=0.38(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep4: 23041.84294 IOPS, 0.09 GB/s, p99 683671.552 us, daemon CPU 67.400s (48.75 us/IO), rss 1276364 KiB, split lunix-coord=12.57(1) lunix-q2=5.49(1) lunix-q3=5.41(1) iou-wrk-279765=0.49(1) iou-wrk-279766=0.48(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep5: 18091.337586 IOPS, 0.07 GB/s, p99 859832.32 us, daemon CPU 59.820s (56.04 us/IO), rss 1299040 KiB, split lunix-coord=13.52(9) lunix-q2=4.68(1) lunix-q3=4.61(1) iou-wrk-279765=0.48(1) iou-wrk-279766=0.47(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/seqw/rep1: 727.466754 IOPS, 0.77 GB/s, p99 1669332.992 us, daemon CPU 43.460s (1991.39 us/IO), rss 1557932 KiB, split lunix-coord=8.43(1) lunix-q2=3.77(1) lunix-q3=3.24(1) iou-wrk-279765=1.01(1) iou-wrk-279766=0.88(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 718.743795 IOPS, 0.76 GB/s, p99 1887436.8 us, daemon CPU 42.470s (1969.64 us/IO), rss 1650228 KiB, split lunix-coord=7.98(1) lunix-q2=3.64(1) lunix-q3=3.17(1) iou-wrk-279765=0.97(1) iou-wrk-279766=0.83(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 691.971762 IOPS, 0.73 GB/s, p99 2264924.16 us, daemon CPU 44.120s (2125.33 us/IO), rss 1709908 KiB, split lunix-coord=7.99(1) lunix-q2=3.68(1) lunix-q3=3.00(1) iou-wrk-279765=0.98(1) iou-wrk-279766=0.83(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 688.861012 IOPS, 0.73 GB/s, p99 2231369.728 us, daemon CPU 43.330s (2029.06 us/IO), rss 1816504 KiB, split lunix-coord=7.75(1) lunix-q2=3.57(1) lunix-q3=2.97(1) iou-wrk-279765=0.96(1) iou-wrk-279766=0.82(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 655.047661 IOPS, 0.70 GB/s, p99 2038431.744 us, daemon CPU 43.410s (2137.74 us/IO), rss 1812000 KiB, split lunix-coord=7.75(1) lunix-q2=3.36(1) lunix-q3=2.90(1) iou-wrk-279765=0.92(1) iou-wrk-279766=0.80(1) lunix-space-tim=0.11(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
