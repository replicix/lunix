# M16 Part A (2026-11-21T00:34:27Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 5.72 13.08 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 13.08 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569248 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 16847.960424 IOPS, 0.07 GB/s, p99 599785.472 us, daemon CPU 41.420s (40.30 us/IO), rss 863516 KiB, split lunix-coord=6.90(1) lunix-q3=3.42(1) lunix-q2=3.43(1) lunix-space-lad=1.09(1) iou-wrk-241889=0.26(1) iou-wrk-241888=0.27(1) lunix-space-tim=0.18(1) 
- partA/c64k/q4/w4k/rep2: 31584.38012 IOPS, 0.13 GB/s, p99 488636.416 us, daemon CPU 78.770s (41.57 us/IO), rss 1238488 KiB, split lunix-coord=13.12(1) lunix-q3=6.24(1) lunix-q2=6.23(1) lunix-space-lad=0.92(1) iou-wrk-241889=0.39(1) iou-wrk-241888=0.39(1) lunix-space-tim=0.21(1) 
- partA/c64k/q4/w4k/rep3: 19377.21697 IOPS, 0.08 GB/s, p99 557842.432 us, daemon CPU 55.600s (44.83 us/IO), rss 1257492 KiB, split lunix-coord=11.07(1) lunix-q3=4.13(1) lunix-q2=4.26(1) lunix-space-lad=1.11(1) iou-wrk-241889=0.26(1) iou-wrk-241888=0.29(1) lunix-space-tim=0.19(1) 
- partA/c64k/q4/seqw/rep1: 595.097883 IOPS, 0.63 GB/s, p99 2105540.608 us, daemon CPU 36.860s (1147.03 us/IO), rss 764152 KiB, split lunix-coord=7.50(1) lunix-q3=2.96(1) lunix-q2=3.12(1) iou-wrk-241888=0.73(1) iou-wrk-241889=0.63(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep2: 612.039206 IOPS, 0.65 GB/s, p99 3372220.416 us, daemon CPU 39.200s (2001.51 us/IO), rss 1456892 KiB, split lunix-coord=7.70(1) lunix-q2=3.48(1) lunix-q3=3.22(1) iou-wrk-241888=0.75(1) iou-wrk-241889=0.64(1) lunix-space-tim=0.07(1) 
- partA/c64k/q4/seqw/rep3: 665.804078 IOPS, 0.71 GB/s, p99 2600468.48 us, daemon CPU 43.650s (1986.66 us/IO), rss 1560488 KiB, split lunix-coord=8.62(1) lunix-q2=3.72(1) lunix-q3=3.46(1) iou-wrk-241888=0.81(1) iou-wrk-241889=0.69(1) lunix-space-tim=0.08(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569280 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 42099.522926 IOPS, 0.17 GB/s, p99 484442.112 us, daemon CPU 92.140s (35.88 us/IO), rss 1384840 KiB, split lunix-coord=23.38(1) lunix-q2=8.60(1) lunix-q3=8.58(1) iou-wrk-306086=0.69(1) iou-wrk-306087=0.67(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 42182.586449 IOPS, 0.17 GB/s, p99 316669.952 us, daemon CPU 96.210s (37.39 us/IO), rss 1545836 KiB, split lunix-coord=26.21(1) lunix-q2=8.59(1) lunix-q3=8.54(1) iou-wrk-306086=0.55(1) iou-wrk-306087=0.53(1) lunix-space-tim=0.34(1) 
- partA/c16k/q4/w4k/rep3: 43406.888393 IOPS, 0.18 GB/s, p99 320864.256 us, daemon CPU 100.900s (38.74 us/IO), rss 1571028 KiB, split lunix-coord=26.19(1) lunix-q2=8.76(1) lunix-q3=8.62(1) iou-wrk-306086=0.56(1) iou-wrk-306087=0.53(1) lunix-space-tim=0.33(1) 
- partA/c16k/q4/seqw/rep1: 537.96046 IOPS, 0.57 GB/s, p99 1686110.208 us, daemon CPU 52.400s (1837.83 us/IO), rss 1809428 KiB, split lunix-coord=13.69(1) lunix-q2=2.29(1) lunix-q3=2.61(1) iou-wrk-306086=0.59(1) iou-wrk-306087=0.55(1) lunix-space-tim=0.09(1) 
- partA/c16k/q4/seqw/rep2: 673.841005 IOPS, 0.72 GB/s, p99 641728.512 us, daemon CPU 63.890s (3058.54 us/IO), rss 1661924 KiB, split lunix-coord=15.11(1) lunix-q2=3.31(1) lunix-q3=3.23(1) iou-wrk-306086=0.73(1) iou-wrk-306087=0.67(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/seqw/rep3: 574.27887 IOPS, 0.61 GB/s, p99 1199570.944 us, daemon CPU 65.510s (3679.79 us/IO), rss 1832380 KiB, split lunix-coord=23.55(1) lunix-q2=2.50(1) lunix-q3=2.58(1) iou-wrk-306086=0.63(1) iou-wrk-306087=0.56(1) lunix-space-tim=0.07(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
