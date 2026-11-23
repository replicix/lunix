# M16 Part A (2026-11-23T16:17:18Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 1.93 116.13 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 116.13 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569292 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 42910.366991 IOPS, 0.18 GB/s, p99 417333.248 us, daemon CPU 101.190s (39.97 us/IO), rss 1390460 KiB, split lunix-coord=30.21(1) lunix-q2=8.94(1) lunix-q3=8.85(1) iou-wrk-2270376=0.66(1) iou-wrk-2270377=0.61(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep2: 42840.297911 IOPS, 0.18 GB/s, p99 312475.648 us, daemon CPU 111.140s (43.24 us/IO), rss 1516244 KiB, split lunix-coord=38.06(1) lunix-q2=9.17(1) lunix-q3=9.13(1) iou-wrk-2270376=0.68(1) iou-wrk-2270377=0.66(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep3: 36603.891154 IOPS, 0.15 GB/s, p99 379584.512 us, daemon CPU 107.120s (48.77 us/IO), rss 1549940 KiB, split lunix-coord=37.29(1) lunix-q2=8.27(1) lunix-q3=8.14(1) iou-wrk-2270376=0.60(1) iou-wrk-2270377=0.56(1) lunix-space-tim=0.45(1) 
- partA/c16k/q4/w4k/rep4: 37048.498424 IOPS, 0.15 GB/s, p99 421527.552 us, daemon CPU 110.990s (49.93 us/IO), rss 1529060 KiB, split lunix-coord=38.08(1) lunix-q2=8.21(1) lunix-q3=8.19(1) iou-wrk-2270376=0.61(1) iou-wrk-2270377=0.57(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep5: 36752.595615 IOPS, 0.15 GB/s, p99 421527.552 us, daemon CPU 115.590s (52.42 us/IO), rss 1541536 KiB, split lunix-coord=38.47(1) lunix-q2=8.08(1) lunix-q3=7.93(1) iou-wrk-2270376=0.64(1) iou-wrk-2270377=0.59(1) lunix-space-tim=0.42(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
