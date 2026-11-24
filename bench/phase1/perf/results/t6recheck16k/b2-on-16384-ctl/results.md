# M16 Part A (2026-11-24T04:44:52Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569308 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44313.115492 IOPS, 0.18 GB/s, p99 354418.688 us, daemon CPU 95.590s (35.95 us/IO), rss 1396472 KiB, split lunix-coord=24.10(1) lunix-q3=9.20(1) lunix-q2=9.31(1) iou-wrk-178384=0.64(1) iou-wrk-178383=0.69(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep2: 44225.747316 IOPS, 0.18 GB/s, p99 329252.864 us, daemon CPU 103.740s (39.09 us/IO), rss 1520352 KiB, split lunix-coord=29.32(1) lunix-q3=9.29(1) lunix-q2=9.33(1) iou-wrk-178384=0.63(1) iou-wrk-178383=0.67(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 46079.696742 IOPS, 0.19 GB/s, p99 299892.736 us, daemon CPU 111.460s (40.31 us/IO), rss 1547172 KiB, split lunix-coord=30.07(1) lunix-q3=9.59(1) lunix-q2=9.57(1) iou-wrk-178384=0.67(1) iou-wrk-178383=0.72(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep4: 47685.883235 IOPS, 0.20 GB/s, p99 270532.608 us, daemon CPU 115.320s (40.31 us/IO), rss 1569768 KiB, split lunix-coord=28.87(1) lunix-q3=9.89(1) lunix-q2=9.89(1) iou-wrk-178383=0.69(1) iou-wrk-178384=0.65(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep5: 45830.578239 IOPS, 0.19 GB/s, p99 320864.256 us, daemon CPU 126.840s (46.13 us/IO), rss 1607728 KiB, split lunix-coord=30.78(1) lunix-q3=9.62(1) lunix-q2=9.72(1) iou-wrk-178383=0.71(1) iou-wrk-178384=0.66(1) lunix-space-tim=0.43(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
