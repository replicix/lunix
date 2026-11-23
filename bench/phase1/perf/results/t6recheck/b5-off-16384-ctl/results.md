# M16 Part A (2026-11-23T16:05:33Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.94 96.93 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 96.93 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569252 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 40641.600293 IOPS, 0.17 GB/s, p99 408944.64 us, daemon CPU 95.280s (39.74 us/IO), rss 1352180 KiB, split lunix-coord=30.03(1) lunix-q3=8.21(1) lunix-q2=8.16(1) iou-wrk-2243723=0.58(1) iou-wrk-2243722=0.61(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 44334.205178 IOPS, 0.18 GB/s, p99 425721.856 us, daemon CPU 110.100s (41.39 us/IO), rss 1507836 KiB, split lunix-coord=37.05(1) lunix-q3=9.11(1) lunix-q2=9.10(1) iou-wrk-2243723=0.61(1) iou-wrk-2243722=0.65(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep3: 37606.184675 IOPS, 0.15 GB/s, p99 387973.12 us, daemon CPU 101.870s (45.15 us/IO), rss 1563980 KiB, split lunix-coord=34.93(1) lunix-q3=8.07(1) lunix-q2=8.04(1) iou-wrk-2243723=0.60(1) iou-wrk-2243722=0.62(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep4: 37585.886862 IOPS, 0.15 GB/s, p99 442499.072 us, daemon CPU 105.020s (47.36 us/IO), rss 1574628 KiB, split lunix-coord=36.27(9) lunix-q3=7.92(1) lunix-q2=7.94(1) iou-wrk-2243723=0.59(1) iou-wrk-2243722=0.61(1) lunix-space-tim=0.42(1) 
- partA/c16k/q4/w4k/rep5: 37066.024714 IOPS, 0.15 GB/s, p99 442499.072 us, daemon CPU 111.500s (50.99 us/IO), rss 1536340 KiB, split lunix-coord=35.66(1) lunix-q3=8.01(1) lunix-q2=8.12(1) iou-wrk-2243722=0.62(1) iou-wrk-2243723=0.56(1) lunix-space-tim=0.43(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
