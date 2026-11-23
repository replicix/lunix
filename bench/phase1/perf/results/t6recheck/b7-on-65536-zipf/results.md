# M16 Part A (2026-11-23T16:29:02Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 2.11 121.18 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 121.18 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569092 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4kz/rep1: 36702.790969 IOPS, 0.15 GB/s, p99 329252.864 us, daemon CPU 93.460s (43.16 us/IO), rss 1095348 KiB, split lunix-q3=23.84(1) lunix-q2=23.91(1) lunix-coord=11.78(1) iou-wrk-2297400=1.21(1) iou-wrk-2297399=1.23(1) lunix-space-tim=0.20(1) 
- partA/c64k/q4/w4kz/rep2: 48207.796751 IOPS, 0.20 GB/s, p99 295698.432 us, daemon CPU 125.220s (43.29 us/IO), rss 1225716 KiB, split lunix-q2=31.12(1) lunix-q3=30.75(1) lunix-coord=17.71(1) iou-wrk-2297400=1.50(1) iou-wrk-2297399=1.51(1) lunix-space-tim=0.19(1) 
- partA/c64k/q4/w4kz/rep3: 45448.076699 IOPS, 0.19 GB/s, p99 287309.824 us, daemon CPU 119.870s (44.70 us/IO), rss 1277700 KiB, split lunix-q2=31.44(1) lunix-q3=30.95(1) lunix-coord=16.84(1) iou-wrk-2297400=1.38(1) iou-wrk-2297399=1.41(1) lunix-space-tim=0.21(1) 
- partA/c64k/q4/w4kz/rep4: 50407.732045 IOPS, 0.21 GB/s, p99 267386.88 us, daemon CPU 134.210s (45.13 us/IO), rss 1323616 KiB, split lunix-q2=35.00(1) lunix-q3=35.23(1) lunix-coord=18.48(1) iou-wrk-2297400=1.55(1) iou-wrk-2297399=1.55(1) lunix-space-tim=0.18(1) 
- partA/c64k/q4/w4kz/rep5: 48522.702694 IOPS, 0.20 GB/s, p99 270532.608 us, daemon CPU 128.890s (44.27 us/IO), rss 1370360 KiB, split lunix-q2=33.02(1) lunix-q3=33.02(1) lunix-coord=18.22(1) iou-wrk-2297400=1.46(1) iou-wrk-2297399=1.47(1) lunix-space-tim=0.18(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
