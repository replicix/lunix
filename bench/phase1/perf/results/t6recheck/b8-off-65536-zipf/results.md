# M16 Part A (2026-11-23T16:39:53Z, kernel 7.0.0, 16 cpus): chunks 65536, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 2.54 130.85 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 130.85 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569204 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4kz/rep1: 34322.449794 IOPS, 0.14 GB/s, p99 341835.776 us, daemon CPU 87.540s (42.51 us/IO), rss 1160888 KiB, split lunix-q3=22.19(1) lunix-q2=21.74(1) lunix-coord=11.18(1) iou-wrk-2325440=1.13(1) iou-wrk-2325439=1.11(1) lunix-space-tim=0.21(1) 
- partA/c64k/q4/w4kz/rep2: 40696.775482 IOPS, 0.17 GB/s, p99 299892.736 us, daemon CPU 106.490s (43.61 us/IO), rss 1226932 KiB, split lunix-q3=26.26(1) lunix-q2=25.64(1) lunix-coord=14.31(1) iou-wrk-2325440=1.26(1) iou-wrk-2325439=1.25(1) lunix-space-tim=0.26(1) 
- partA/c64k/q4/w4kz/rep3: 41467.530953 IOPS, 0.17 GB/s, p99 295698.432 us, daemon CPU 110.250s (45.06 us/IO), rss 1278728 KiB, split lunix-q3=28.92(1) lunix-q2=28.79(1) lunix-coord=14.68(1) iou-wrk-2325440=1.25(1) iou-wrk-2325439=1.25(1) lunix-space-tim=0.24(1) 
- partA/c64k/q4/w4kz/rep4: 41515.787194 IOPS, 0.17 GB/s, p99 274726.912 us, daemon CPU 110.310s (45.03 us/IO), rss 1330192 KiB, split lunix-q3=29.59(1) lunix-q2=28.75(1) lunix-coord=13.81(1) iou-wrk-2325440=1.15(1) iou-wrk-2325439=1.13(1) lunix-space-tim=0.30(1) 
- partA/c64k/q4/w4kz/rep5: 42133.258903 IOPS, 0.17 GB/s, p99 278921.216 us, daemon CPU 113.370s (44.85 us/IO), rss 1378108 KiB, split lunix-q3=29.71(1) lunix-q2=29.45(1) lunix-coord=14.82(1) iou-wrk-2325440=1.16(1) iou-wrk-2325439=1.17(1) lunix-space-tim=0.28(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
