# M16 Part A (2026-11-22T22:12:36Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.89 139.52 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 139.52 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569264 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 40948.254374 IOPS, 0.17 GB/s, p99 509607.936 us, daemon CPU 87.580s (36.25 us/IO), rss 1358560 KiB, split lunix-coord=22.38(1) lunix-q3=8.29(1) lunix-q2=8.33(1) iou-wrk-658378=0.59(1) iou-wrk-658377=0.61(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 37467.507582 IOPS, 0.15 GB/s, p99 362807.296 us, daemon CPU 87.770s (39.04 us/IO), rss 1524072 KiB, split lunix-coord=25.42(9) lunix-q3=7.90(1) lunix-q2=8.03(1) iou-wrk-658378=0.55(1) iou-wrk-658377=0.57(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 41250.426092 IOPS, 0.17 GB/s, p99 371195.904 us, daemon CPU 100.800s (40.73 us/IO), rss 1541692 KiB, split lunix-coord=26.47(1) lunix-q3=8.70(1) lunix-q2=8.75(1) iou-wrk-658378=0.61(1) iou-wrk-658377=0.64(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep4: 43359.544037 IOPS, 0.18 GB/s, p99 396361.728 us, daemon CPU 106.480s (40.93 us/IO), rss 1550124 KiB, split lunix-coord=26.25(1) lunix-q3=8.97(1) lunix-q2=9.11(1) iou-wrk-658378=0.61(1) iou-wrk-658377=0.64(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep5: 39667.577702 IOPS, 0.16 GB/s, p99 295698.432 us, daemon CPU 105.590s (44.36 us/IO), rss 1578044 KiB, split lunix-coord=26.56(1) lunix-q3=8.55(1) lunix-q2=8.55(1) iou-wrk-658377=0.58(1) iou-wrk-658378=0.55(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/seqw/rep1: 568.98139 IOPS, 0.61 GB/s, p99 1434451.968 us, daemon CPU 58.800s (3444.75 us/IO), rss 1698680 KiB, split lunix-coord=14.78(1) lunix-q2=2.78(1) lunix-q3=2.15(1) iou-wrk-658377=0.73(1) iou-wrk-658378=0.65(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep2: 563.061881 IOPS, 0.60 GB/s, p99 1686110.208 us, daemon CPU 62.340s (3817.80 us/IO), rss 1689100 KiB, split lunix-coord=13.19(1) lunix-q2=3.27(1) lunix-q3=2.42(1) iou-wrk-658377=0.80(1) iou-wrk-658378=0.70(1) lunix-space-tim=0.11(1) 
- partA/c16k/q4/seqw/rep3: 571.475518 IOPS, 0.61 GB/s, p99 1551892.48 us, daemon CPU 62.450s (3768.22 us/IO), rss 1729896 KiB, split lunix-coord=13.87(1) lunix-q2=2.94(1) lunix-q3=2.22(1) iou-wrk-658377=0.76(1) iou-wrk-658378=0.66(1) lunix-space-tim=0.11(1) 
- partA/c16k/q4/seqw/rep4: 646.979201 IOPS, 0.69 GB/s, p99 843055.104 us, daemon CPU 71.330s (3801.75 us/IO), rss 1788820 KiB, split lunix-coord=15.17(1) lunix-q2=3.36(1) lunix-q3=2.49(1) iou-wrk-658377=0.88(1) iou-wrk-658378=0.71(1) lunix-space-tim=0.11(1) 
- partA/c16k/q4/seqw/rep5: 575.541704 IOPS, 0.61 GB/s, p99 2231369.728 us, daemon CPU 64.820s (3754.14 us/IO), rss 1704672 KiB, split lunix-coord=13.05(1) lunix-q2=2.89(1) lunix-q3=2.20(1) iou-wrk-658377=0.76(1) iou-wrk-658378=0.64(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
