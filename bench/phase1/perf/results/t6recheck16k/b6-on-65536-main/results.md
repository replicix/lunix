# M16 Part A (2026-11-24T06:51:33Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569156 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 28617.093248 IOPS, 0.12 GB/s, p99 530579.456 us, daemon CPU 70.870s (41.97 us/IO), rss 1204180 KiB, split lunix-coord=12.42(9) lunix-q2=5.95(1) lunix-q3=5.98(1) iou-wrk-327225=0.45(1) iou-wrk-327227=0.43(1) lunix-space-lad=0.31(1) lunix-space-tim=0.28(1) 
- partA/c64k/q4/w4k/rep2: 22064.714514 IOPS, 0.09 GB/s, p99 658505.728 us, daemon CPU 64.550s (48.76 us/IO), rss 1228028 KiB, split lunix-coord=12.70(1) lunix-q2=4.99(1) lunix-q3=5.01(1) iou-wrk-327225=0.42(1) iou-wrk-327227=0.40(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep3: 25320.112844 IOPS, 0.10 GB/s, p99 608174.08 us, daemon CPU 72.020s (47.41 us/IO), rss 1266532 KiB, split lunix-coord=12.22(1) lunix-q2=5.86(1) lunix-q3=5.85(1) iou-wrk-327225=0.48(1) iou-wrk-327227=0.47(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/w4k/rep4: 26255.668283 IOPS, 0.11 GB/s, p99 599785.472 us, daemon CPU 76.540s (49.41 us/IO), rss 1307504 KiB, split lunix-coord=13.01(1) lunix-q2=5.94(1) lunix-q3=5.89(1) iou-wrk-327225=0.47(1) iou-wrk-327227=0.45(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/w4k/rep5: 22171.498829 IOPS, 0.09 GB/s, p99 650117.12 us, daemon CPU 72.650s (54.61 us/IO), rss 1326700 KiB, split lunix-coord=14.07(1) lunix-q2=5.37(1) lunix-q3=5.28(1) iou-wrk-327225=0.49(1) iou-wrk-327227=0.47(1) lunix-space-tim=0.40(1) 
- partA/c64k/q4/seqw/rep1: 703.359756 IOPS, 0.75 GB/s, p99 2432696.32 us, daemon CPU 40.530s (1920.78 us/IO), rss 1720992 KiB, split lunix-coord=8.13(1) lunix-q2=3.46(1) lunix-q3=2.74(1) iou-wrk-327225=0.93(1) iou-wrk-327227=0.80(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep2: 712.791925 IOPS, 0.76 GB/s, p99 2332033.024 us, daemon CPU 43.170s (2088.44 us/IO), rss 1517900 KiB, split lunix-coord=8.31(1) lunix-q2=3.93(1) lunix-q3=2.97(1) iou-wrk-327225=0.96(1) iou-wrk-327227=0.83(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 661.851522 IOPS, 0.70 GB/s, p99 2264924.16 us, daemon CPU 41.200s (2146.54 us/IO), rss 1791724 KiB, split lunix-coord=7.69(1) lunix-q2=3.71(1) lunix-q3=2.92(1) iou-wrk-327225=0.89(1) iou-wrk-327227=0.76(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep4: 677.363007 IOPS, 0.72 GB/s, p99 2365587.456 us, daemon CPU 44.690s (2199.21 us/IO), rss 1606204 KiB, split lunix-coord=7.95(1) lunix-q2=3.93(1) lunix-q3=2.98(1) iou-wrk-327225=0.95(1) iou-wrk-327227=0.81(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 676.413508 IOPS, 0.72 GB/s, p99 3170893.824 us, daemon CPU 45.640s (2249.12 us/IO), rss 1620016 KiB, split lunix-coord=8.23(1) lunix-q2=3.94(1) lunix-q3=2.85(1) iou-wrk-327225=0.96(1) iou-wrk-327227=0.80(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
