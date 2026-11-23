# M16 Part A (2026-11-23T15:52:53Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.92 96.69 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 96.69 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569048 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 20675.747597 IOPS, 0.08 GB/s, p99 574619.648 us, daemon CPU 49.880s (40.89 us/IO), rss 1177792 KiB, split lunix-coord=8.16(1) lunix-q3=4.27(1) lunix-q2=4.29(1) iou-wrk-2199151=0.31(1) lunix-space-lad=0.53(1) iou-wrk-2199150=0.33(1) lunix-space-tim=0.25(1) 
- partA/c64k/q4/w4k/rep2: 23697.352417 IOPS, 0.10 GB/s, p99 666894.336 us, daemon CPU 69.720s (49.04 us/IO), rss 1228028 KiB, split lunix-coord=13.36(1) lunix-q3=5.17(1) lunix-q2=5.26(1) iou-wrk-2199151=0.39(1) iou-wrk-2199150=0.42(1) lunix-space-lad=0.18(1) lunix-space-tim=0.38(1) 
- partA/c64k/q4/w4k/rep3: 20118.656456 IOPS, 0.08 GB/s, p99 834666.496 us, daemon CPU 62.670s (52.80 us/IO), rss 1273836 KiB, split lunix-coord=12.01(1) lunix-q3=4.73(1) lunix-q2=4.69(1) iou-wrk-2199151=0.41(1) iou-wrk-2199150=0.42(1) lunix-space-tim=0.39(1) 
- partA/c64k/q4/w4k/rep4: 20639.793487 IOPS, 0.08 GB/s, p99 775946.24 us, daemon CPU 63.630s (52.25 us/IO), rss 1291956 KiB, split lunix-coord=12.00(1) lunix-q3=4.95(1) lunix-q2=5.05(1) iou-wrk-2199151=0.46(1) iou-wrk-2199150=0.48(1) lunix-space-tim=0.41(1) 
- partA/c64k/q4/w4k/rep5: 17846.597808 IOPS, 0.07 GB/s, p99 901775.36 us, daemon CPU 59.590s (56.59 us/IO), rss 1316960 KiB, split lunix-coord=12.22(1) lunix-q3=4.32(1) lunix-q2=4.42(1) iou-wrk-2199151=0.38(1) iou-wrk-2199150=0.41(1) lunix-space-tim=0.42(1) 
- partA/c64k/q4/seqw/rep1: 642.030391 IOPS, 0.68 GB/s, p99 1853882.368 us, daemon CPU 38.750s (2011.85 us/IO), rss 1563760 KiB, split lunix-coord=7.41(1) lunix-q3=3.16(1) lunix-q2=3.32(1) iou-wrk-2199150=0.88(1) iou-wrk-2199151=0.73(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep2: 665.563957 IOPS, 0.71 GB/s, p99 2399141.888 us, daemon CPU 40.950s (1984.73 us/IO), rss 1523612 KiB, split lunix-coord=7.65(1) lunix-q2=3.51(1) lunix-q3=3.31(1) iou-wrk-2199150=0.88(1) iou-wrk-2199151=0.73(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep3: 622.340271 IOPS, 0.66 GB/s, p99 4865392.64 us, daemon CPU 42.450s (2006.18 us/IO), rss 1611508 KiB, split lunix-coord=7.30(1) lunix-q2=3.62(1) lunix-q3=3.49(1) iou-wrk-2199150=0.92(1) iou-wrk-2199151=0.74(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep4: 654.359486 IOPS, 0.69 GB/s, p99 2231369.728 us, daemon CPU 43.060s (2056.40 us/IO), rss 1549356 KiB, split lunix-coord=7.31(1) lunix-q2=3.45(1) lunix-q3=3.44(1) iou-wrk-2199150=0.89(1) iou-wrk-2199151=0.73(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep5: 659.264098 IOPS, 0.70 GB/s, p99 2466250.752 us, daemon CPU 46.140s (2257.65 us/IO), rss 1587184 KiB, split lunix-coord=8.17(1) lunix-q2=3.71(1) lunix-q3=3.36(1) iou-wrk-2199150=0.94(1) iou-wrk-2199151=0.76(1) lunix-space-tim=0.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
