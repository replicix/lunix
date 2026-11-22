# M16 Part A (2026-11-22T22:01:27Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.80 137.84 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 137.84 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569220 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 37109.944338 IOPS, 0.15 GB/s, p99 583008.256 us, daemon CPU 79.770s (35.83 us/IO), rss 1390500 KiB, split lunix-coord=20.27(1) lunix-q2=7.47(1) lunix-q3=7.36(1) iou-wrk-616364=0.55(1) lunix-space-lad=0.76(1) iou-wrk-616365=0.52(1) lunix-space-tim=0.37(1) 
- partA/c16k/q4/w4k/rep2: 46255.665534 IOPS, 0.19 GB/s, p99 408944.64 us, daemon CPU 101.750s (37.28 us/IO), rss 1541056 KiB, split lunix-coord=25.84(1) lunix-q2=9.30(1) lunix-q3=9.19(1) iou-wrk-616364=0.68(1) iou-wrk-616365=0.65(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep3: 42542.45736 IOPS, 0.17 GB/s, p99 341835.776 us, daemon CPU 101.370s (40.39 us/IO), rss 1568428 KiB, split lunix-coord=26.09(1) lunix-q2=8.73(1) lunix-q3=8.61(1) iou-wrk-616364=0.61(1) iou-wrk-616365=0.59(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep4: 42921.98227 IOPS, 0.18 GB/s, p99 278921.216 us, daemon CPU 106.750s (41.45 us/IO), rss 1599868 KiB, split lunix-coord=27.05(1) lunix-q2=9.00(1) lunix-q3=8.81(1) iou-wrk-616364=0.66(1) iou-wrk-616365=0.62(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep5: 44066.884391 IOPS, 0.18 GB/s, p99 299892.736 us, daemon CPU 116.380s (44.02 us/IO), rss 1592620 KiB, split lunix-coord=27.33(1) lunix-q2=9.53(1) lunix-q3=9.28(1) iou-wrk-616364=0.65(1) iou-wrk-616365=0.61(1) lunix-space-tim=0.45(1) 
- partA/c16k/q4/seqw/rep1: 543.839958 IOPS, 0.58 GB/s, p99 1686110.208 us, daemon CPU 57.300s (3512.06 us/IO), rss 1722500 KiB, split lunix-coord=14.30(1) lunix-q2=2.44(1) lunix-q3=2.48(1) iou-wrk-616364=0.72(1) iou-wrk-616365=0.65(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/seqw/rep2: 636.250246 IOPS, 0.68 GB/s, p99 876609.536 us, daemon CPU 66.520s (3485.00 us/IO), rss 1909956 KiB, split lunix-coord=14.38(1) lunix-q2=2.92(1) lunix-q3=2.55(1) iou-wrk-616364=0.84(1) iou-wrk-616365=0.72(1) lunix-space-tim=0.09(1) 
- partA/c16k/q4/seqw/rep3: 641.433341 IOPS, 0.68 GB/s, p99 1019215.872 us, daemon CPU 69.290s (3600.79 us/IO), rss 1916568 KiB, split lunix-coord=14.73(1) lunix-q2=2.90(1) lunix-q3=2.72(1) iou-wrk-616364=0.85(1) iou-wrk-616365=0.74(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep4: 586.099927 IOPS, 0.62 GB/s, p99 1468006.4 us, daemon CPU 65.990s (3753.06 us/IO), rss 1837684 KiB, split lunix-coord=14.17(1) lunix-q2=2.57(1) lunix-q3=2.70(1) iou-wrk-616364=0.80(1) iou-wrk-616365=0.67(1) lunix-space-tim=0.09(1) 
- partA/c16k/q4/seqw/rep5: 575.92775 IOPS, 0.61 GB/s, p99 1669332.992 us, daemon CPU 67.050s (3880.70 us/IO), rss 1937744 KiB, split lunix-coord=13.84(1) lunix-q2=2.61(1) lunix-q3=2.38(1) iou-wrk-616364=0.78(1) iou-wrk-616365=0.66(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
