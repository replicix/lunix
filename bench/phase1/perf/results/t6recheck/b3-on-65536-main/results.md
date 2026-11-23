# M16 Part A (2026-11-23T15:40:12Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.93 95.80 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 95.80 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569052 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 30256.806745 IOPS, 0.12 GB/s, p99 509607.936 us, daemon CPU 70.520s (39.50 us/IO), rss 1174924 KiB, split lunix-coord=11.52(1) lunix-q3=6.07(1) lunix-q2=6.10(1) iou-wrk-2153908=0.44(1) iou-wrk-2153907=0.45(1) lunix-space-lad=0.48(1) lunix-space-tim=0.27(1) 
- partA/c64k/q4/w4k/rep2: 24365.991835 IOPS, 0.10 GB/s, p99 608174.08 us, daemon CPU 74.320s (50.84 us/IO), rss 1225660 KiB, split lunix-coord=16.75(9) lunix-q3=5.75(1) lunix-q2=5.83(1) iou-wrk-2153908=0.44(1) iou-wrk-2153907=0.46(1) lunix-space-tim=0.45(1) 
- partA/c64k/q4/w4k/rep3: 22602.267242 IOPS, 0.09 GB/s, p99 624951.296 us, daemon CPU 79.480s (58.61 us/IO), rss 1270096 KiB, split lunix-coord=17.01(1) lunix-q3=5.76(1) lunix-q2=5.81(1) iou-wrk-2153908=0.50(1) iou-wrk-2153907=0.52(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/w4k/rep4: 20852.815728 IOPS, 0.09 GB/s, p99 708837.376 us, daemon CPU 71.240s (57.90 us/IO), rss 1272924 KiB, split lunix-coord=14.11(1) lunix-q3=5.04(1) lunix-q2=5.11(1) iou-wrk-2153908=0.40(1) iou-wrk-2153907=0.41(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/w4k/rep5: 24955.848605 IOPS, 0.10 GB/s, p99 574619.648 us, daemon CPU 78.250s (53.14 us/IO), rss 1314772 KiB, split lunix-coord=14.79(1) lunix-q3=5.78(1) lunix-q2=5.79(1) iou-wrk-2153908=0.44(1) iou-wrk-2153907=0.46(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/seqw/rep1: 746.277566 IOPS, 0.79 GB/s, p99 1484783.616 us, daemon CPU 43.530s (1944.32 us/IO), rss 1769640 KiB, split lunix-coord=8.48(1) lunix-q2=3.61(1) lunix-q3=3.13(1) iou-wrk-2153907=0.96(1) iou-wrk-2153908=0.81(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 697.758233 IOPS, 0.74 GB/s, p99 2139095.04 us, daemon CPU 41.330s (2042.50 us/IO), rss 1567340 KiB, split lunix-coord=7.97(1) lunix-q2=3.35(1) lunix-q3=2.99(1) iou-wrk-2153907=0.90(1) iou-wrk-2153908=0.74(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep3: 675.773733 IOPS, 0.72 GB/s, p99 2566914.048 us, daemon CPU 44.100s (2105.11 us/IO), rss 1691536 KiB, split lunix-coord=7.94(1) lunix-q2=3.62(1) lunix-q3=3.10(1) iou-wrk-2153907=0.93(1) iou-wrk-2153908=0.77(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep4: 657.315187 IOPS, 0.70 GB/s, p99 3036676.096 us, daemon CPU 43.910s (2087.56 us/IO), rss 1797596 KiB, split lunix-coord=7.59(1) lunix-q2=3.55(1) lunix-q3=3.04(1) iou-wrk-2153907=0.91(1) iou-wrk-2153908=0.75(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 671.175533 IOPS, 0.71 GB/s, p99 2734686.208 us, daemon CPU 45.470s (2185.38 us/IO), rss 1815044 KiB, split lunix-coord=7.98(1) lunix-q2=3.61(1) lunix-q3=3.10(1) iou-wrk-2153907=0.92(1) iou-wrk-2153908=0.75(1) lunix-space-tim=0.10(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
