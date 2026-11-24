# M16 Part A (2026-11-24T04:13:17Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.94 0.85 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569140 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 41424.374719 IOPS, 0.17 GB/s, p99 438304.768 us, daemon CPU 86.460s (35.38 us/IO), rss 1424920 KiB, split lunix-coord=21.64(9) lunix-q3=8.40(1) lunix-q2=8.38(1) iou-wrk-146058=0.59(1) iou-wrk-146057=0.62(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep2: 40337.450338 IOPS, 0.17 GB/s, p99 333447.168 us, daemon CPU 93.680s (39.36 us/IO), rss 1575132 KiB, split lunix-coord=24.33(1) lunix-q3=8.47(1) lunix-q2=8.48(1) iou-wrk-146058=0.59(1) iou-wrk-146057=0.63(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 47066.965476 IOPS, 0.19 GB/s, p99 229638.144 us, daemon CPU 109.040s (38.61 us/IO), rss 1591568 KiB, split lunix-coord=26.37(1) lunix-q3=9.69(1) lunix-q2=9.64(1) iou-wrk-146058=0.65(1) iou-wrk-146057=0.69(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep4: 43860.721359 IOPS, 0.18 GB/s, p99 320864.256 us, daemon CPU 109.040s (42.14 us/IO), rss 1601444 KiB, split lunix-coord=25.69(1) lunix-q3=9.34(1) lunix-q2=9.28(1) iou-wrk-146057=0.69(1) iou-wrk-146058=0.65(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep5: 42710.21809 IOPS, 0.18 GB/s, p99 358612.992 us, daemon CPU 113.130s (44.15 us/IO), rss 1624500 KiB, split lunix-coord=25.10(1) lunix-q3=9.28(1) lunix-q2=9.39(1) iou-wrk-146057=0.71(1) iou-wrk-146058=0.65(1) lunix-space-tim=0.43(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
