# M16 Part A (2026-11-24T05:48:13Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 5 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.00 1.96 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569276 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 41499.375385 IOPS, 0.17 GB/s, p99 501219.328 us, daemon CPU 88.280s (35.45 us/IO), rss 1417008 KiB, split lunix-coord=22.22(1) lunix-q3=8.56(1) lunix-q2=8.57(1) iou-wrk-241926=0.64(1) iou-wrk-241927=0.60(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 40402.137065 IOPS, 0.17 GB/s, p99 333447.168 us, daemon CPU 93.680s (39.30 us/IO), rss 1545596 KiB, split lunix-coord=23.74(1) lunix-q3=8.68(1) lunix-q2=8.66(1) iou-wrk-241926=0.63(1) iou-wrk-241927=0.60(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep3: 42904.602524 IOPS, 0.18 GB/s, p99 308281.344 us, daemon CPU 101.570s (40.12 us/IO), rss 1581216 KiB, split lunix-coord=24.08(1) lunix-q3=8.95(1) lunix-q2=9.00(1) iou-wrk-241926=0.66(1) iou-wrk-241927=0.61(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep4: 41693.173716 IOPS, 0.17 GB/s, p99 316669.952 us, daemon CPU 105.680s (42.25 us/IO), rss 1595556 KiB, split lunix-coord=24.97(1) lunix-q2=9.02(1) lunix-q3=8.99(1) iou-wrk-241926=0.69(1) iou-wrk-241927=0.65(1) lunix-space-tim=0.43(1) 
- partA/c16k/q4/w4k/rep5: 43805.721712 IOPS, 0.18 GB/s, p99 254803.968 us, daemon CPU 114.890s (43.71 us/IO), rss 1594264 KiB, split lunix-coord=25.31(1) lunix-q2=9.19(1) lunix-q3=9.20(1) iou-wrk-241926=0.66(1) iou-wrk-241927=0.60(1) lunix-space-tim=0.44(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
