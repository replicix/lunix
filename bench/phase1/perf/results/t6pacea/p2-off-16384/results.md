# M16 Part A (2026-11-22T22:34:50Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.68 137.50 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 137.50 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569228 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44279.986007 IOPS, 0.18 GB/s, p99 270532.608 us, daemon CPU 93.660s (35.25 us/IO), rss 1410572 KiB, split lunix-coord=22.78(1) lunix-q3=9.07(1) lunix-q2=8.89(1) lunix-space-lad=1.01(1) iou-wrk-741914=0.61(1) iou-wrk-741913=0.65(1) lunix-space-tim=0.35(1) 
- partA/c16k/q4/w4k/rep2: 39986.569353 IOPS, 0.16 GB/s, p99 337641.472 us, daemon CPU 92.200s (38.43 us/IO), rss 1553280 KiB, split lunix-coord=24.43(1) lunix-q3=8.65(1) lunix-q2=8.53(1) iou-wrk-741914=0.62(1) iou-wrk-741913=0.64(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep3: 45678.180455 IOPS, 0.19 GB/s, p99 325058.56 us, daemon CPU 107.930s (39.38 us/IO), rss 1572972 KiB, split lunix-coord=27.92(1) lunix-q3=9.97(1) lunix-q2=9.86(1) iou-wrk-741914=0.73(1) iou-wrk-741913=0.76(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep4: 44876.097496 IOPS, 0.18 GB/s, p99 274726.912 us, daemon CPU 109.910s (41.51 us/IO), rss 1605180 KiB, split lunix-coord=24.73(1) lunix-q3=9.63(1) lunix-q2=9.54(1) iou-wrk-741914=0.61(1) iou-wrk-741913=0.66(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep5: 41144.446665 IOPS, 0.17 GB/s, p99 308281.344 us, daemon CPU 107.880s (44.44 us/IO), rss 1613632 KiB, split lunix-coord=26.92(1) lunix-q3=8.94(1) lunix-q2=8.93(1) iou-wrk-741913=0.63(1) iou-wrk-741914=0.59(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/seqw/rep1: 594.240665 IOPS, 0.63 GB/s, p99 1619001.344 us, daemon CPU 60.220s (3377.98 us/IO), rss 1843024 KiB, split lunix-coord=14.04(1) lunix-q3=2.20(1) lunix-q2=3.14(1) iou-wrk-741913=0.79(1) iou-wrk-741914=0.66(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep2: 601.764687 IOPS, 0.64 GB/s, p99 1333788.672 us, daemon CPU 62.360s (3454.28 us/IO), rss 1707420 KiB, split lunix-coord=13.46(1) lunix-q2=3.17(1) lunix-q3=2.38(1) iou-wrk-741913=0.79(1) iou-wrk-741914=0.67(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep3: 560.028827 IOPS, 0.60 GB/s, p99 1686110.208 us, daemon CPU 64.240s (3823.61 us/IO), rss 1756028 KiB, split lunix-coord=13.99(1) lunix-q2=2.88(1) lunix-q3=2.18(1) iou-wrk-741913=0.77(1) iou-wrk-741914=0.65(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep4: 597.907522 IOPS, 0.64 GB/s, p99 1182793.728 us, daemon CPU 67.870s (3914.23 us/IO), rss 1811932 KiB, split lunix-coord=14.44(1) lunix-q2=3.28(1) lunix-q3=2.44(1) iou-wrk-741913=0.81(1) iou-wrk-741914=0.68(1) lunix-space-tim=0.10(1) 
- partA/c16k/q4/seqw/rep5: 564.467005 IOPS, 0.60 GB/s, p99 1702887.424 us, daemon CPU 64.800s (3826.62 us/IO), rss 1722636 KiB, split lunix-coord=13.05(1) lunix-q2=3.04(1) lunix-q3=2.13(1) iou-wrk-741913=0.75(1) iou-wrk-741914=0.63(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
