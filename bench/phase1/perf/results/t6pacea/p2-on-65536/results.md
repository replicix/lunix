# M16 Part A (2026-11-22T22:45:59Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 4.85 138.73 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 138.73 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569008 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 28817.272924 IOPS, 0.12 GB/s, p99 505413.632 us, daemon CPU 70.360s (41.38 us/IO), rss 1200916 KiB, split lunix-coord=11.44(1) lunix-q3=5.80(1) lunix-q2=5.81(1) iou-wrk-783677=0.44(1) iou-wrk-783676=0.47(1) lunix-space-lad=0.40(1) lunix-space-tim=0.25(1) 
- partA/c64k/q4/w4k/rep2: 21197.010374 IOPS, 0.09 GB/s, p99 742391.808 us, daemon CPU 66.750s (52.48 us/IO), rss 1225336 KiB, split lunix-coord=14.56(1) lunix-q3=5.10(1) lunix-q2=5.13(1) iou-wrk-783677=0.49(1) iou-wrk-783676=0.51(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep3: 19565.678669 IOPS, 0.08 GB/s, p99 767557.632 us, daemon CPU 67.980s (57.91 us/IO), rss 1251120 KiB, split lunix-coord=14.35(1) lunix-q3=4.98(1) lunix-q2=4.95(1) iou-wrk-783677=0.46(1) iou-wrk-783676=0.46(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/w4k/rep4: 19639.747413 IOPS, 0.08 GB/s, p99 725614.592 us, daemon CPU 68.650s (58.26 us/IO), rss 1290324 KiB, split lunix-coord=13.95(1) lunix-q3=4.94(1) lunix-q2=4.90(1) iou-wrk-783677=0.43(1) iou-wrk-783676=0.43(1) lunix-space-tim=0.44(1) 
- partA/c64k/q4/w4k/rep5: 17460.19217 IOPS, 0.07 GB/s, p99 1035993.088 us, daemon CPU 65.570s (63.65 us/IO), rss 1324856 KiB, split lunix-coord=14.24(1) lunix-q3=4.59(1) lunix-q2=4.66(1) iou-wrk-783677=0.43(1) iou-wrk-783676=0.45(1) lunix-space-tim=0.45(1) 
- partA/c64k/q4/seqw/rep1: 620.998974 IOPS, 0.66 GB/s, p99 1769996.288 us, daemon CPU 39.250s (2179.47 us/IO), rss 1697352 KiB, split lunix-coord=8.98(1) lunix-q2=3.07(1) lunix-q3=2.64(1) iou-wrk-783677=0.78(1) iou-wrk-783676=0.87(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 700.065985 IOPS, 0.74 GB/s, p99 2071986.176 us, daemon CPU 43.170s (2055.52 us/IO), rss 1710596 KiB, split lunix-coord=8.62(1) lunix-q2=3.55(1) lunix-q3=3.13(1) iou-wrk-783676=0.98(1) iou-wrk-783677=0.85(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 761.634293 IOPS, 0.81 GB/s, p99 2365587.456 us, daemon CPU 50.100s (2055.61 us/IO), rss 1728376 KiB, split lunix-coord=9.55(1) lunix-q2=4.03(1) lunix-q3=3.60(1) iou-wrk-783676=1.13(1) iou-wrk-783677=0.96(1) lunix-space-tim=0.11(1) 
- partA/c64k/q4/seqw/rep4: 659.422095 IOPS, 0.70 GB/s, p99 2432696.32 us, daemon CPU 43.790s (2142.15 us/IO), rss 1768424 KiB, split lunix-coord=8.57(1) lunix-q2=3.49(1) lunix-q3=3.08(1) iou-wrk-783676=0.95(1) iou-wrk-783677=0.81(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep5: 692.560916 IOPS, 0.73 GB/s, p99 2835349.504 us, daemon CPU 48.640s (2128.25 us/IO), rss 1623004 KiB, split lunix-coord=8.52(1) lunix-q2=3.83(1) lunix-q3=3.23(1) iou-wrk-783676=1.03(1) iou-wrk-783677=0.88(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
