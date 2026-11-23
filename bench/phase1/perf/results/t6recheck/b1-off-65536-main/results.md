# M16 Part A (2026-11-23T15:14:48Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.01 1.96 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569120 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 28544.183882 IOPS, 0.12 GB/s, p99 505413.632 us, daemon CPU 70.270s (41.03 us/IO), rss 1207384 KiB, split lunix-coord=11.71(1) lunix-q2=5.73(1) lunix-q3=5.71(1) iou-wrk-2062744=0.47(1) iou-wrk-2062745=0.43(1) lunix-space-lad=0.30(1) lunix-space-tim=0.25(1) 
- partA/c64k/q4/w4k/rep2: 23062.288918 IOPS, 0.09 GB/s, p99 675282.944 us, daemon CPU 64.790s (46.82 us/IO), rss 1202676 KiB, split lunix-coord=12.03(1) lunix-q2=5.26(1) lunix-q3=5.26(1) iou-wrk-2062744=0.48(1) iou-wrk-2062745=0.44(1) lunix-space-tim=0.38(1) 
- partA/c64k/q4/w4k/rep3: 18721.274399 IOPS, 0.08 GB/s, p99 784334.848 us, daemon CPU 60.510s (53.87 us/IO), rss 1246084 KiB, split lunix-coord=11.84(1) lunix-q2=4.66(1) lunix-q3=4.67(1) iou-wrk-2062744=0.46(1) iou-wrk-2062745=0.43(1) lunix-space-tim=0.40(1) 
- partA/c64k/q4/w4k/rep4: 24589.707443 IOPS, 0.10 GB/s, p99 599785.472 us, daemon CPU 70.970s (48.10 us/IO), rss 1275876 KiB, split lunix-coord=13.18(1) lunix-q2=5.66(1) lunix-q3=5.68(1) iou-wrk-2062744=0.48(1) iou-wrk-2062745=0.45(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/w4k/rep5: 22628.115587 IOPS, 0.09 GB/s, p99 683671.552 us, daemon CPU 66.720s (49.14 us/IO), rss 1298012 KiB, split lunix-coord=11.88(1) lunix-q2=5.36(1) lunix-q3=5.30(1) iou-wrk-2062744=0.51(1) iou-wrk-2062745=0.46(1) lunix-space-tim=0.43(1) 
- partA/c64k/q4/seqw/rep1: 640.263711 IOPS, 0.68 GB/s, p99 1887436.8 us, daemon CPU 39.100s (1969.95 us/IO), rss 1761188 KiB, split lunix-coord=7.70(1) lunix-q2=3.66(1) lunix-q3=2.66(1) iou-wrk-2062744=1.00(1) iou-wrk-2062745=0.79(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep2: 650.314273 IOPS, 0.69 GB/s, p99 2969567.232 us, daemon CPU 41.810s (2009.12 us/IO), rss 1553312 KiB, split lunix-coord=8.11(1) lunix-q2=3.99(1) lunix-q3=2.88(1) iou-wrk-2062744=1.05(1) iou-wrk-2062745=0.79(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 703.376257 IOPS, 0.75 GB/s, p99 2332033.024 us, daemon CPU 44.220s (2028.01 us/IO), rss 1795724 KiB, split lunix-coord=8.10(1) lunix-q2=4.11(1) lunix-q3=3.03(1) iou-wrk-2062744=1.10(1) iou-wrk-2062745=0.84(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep4: 658.572107 IOPS, 0.70 GB/s, p99 2667577.344 us, daemon CPU 43.000s (2106.22 us/IO), rss 1762720 KiB, split lunix-coord=8.02(3) lunix-q2=3.84(1) lunix-q3=2.79(1) iou-wrk-2062744=1.04(1) iou-wrk-2062745=0.79(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep5: 712.552909 IOPS, 0.76 GB/s, p99 2566914.048 us, daemon CPU 48.910s (2214.21 us/IO), rss 1609964 KiB, split lunix-coord=8.53(1) lunix-q2=4.33(1) lunix-q3=3.08(1) iou-wrk-2062744=1.14(1) iou-wrk-2062745=0.86(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
