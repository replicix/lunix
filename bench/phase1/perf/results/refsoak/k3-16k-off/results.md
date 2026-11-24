# M16 Part A (2026-11-24T14:32:39Z, kernel 7.0.0, 16 cpus): chunks 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 7200 s
- load average at the start: 0.03 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569392 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 43446.627341 IOPS, 0.18 GB/s, p99 438304.768 us, daemon CPU 93.120s (36.33 us/IO), rss 1419620 KiB, split lunix-coord=23.44(1) lunix-q3=8.71(1) lunix-q2=8.70(1) iou-wrk-1262061=0.67(1) iou-wrk-1262062=0.62(1) lunix-space-tim=0.41(1) 
- partA/c16k/q4/w4k/rep2: 46636.353086 IOPS, 0.19 GB/s, p99 248512.512 us, daemon CPU 107.940s (38.58 us/IO), rss 1545460 KiB, split lunix-coord=29.02(1) lunix-q3=9.72(1) lunix-q2=9.73(1) iou-wrk-1262061=0.68(1) iou-wrk-1262062=0.64(1) lunix-space-tim=0.44(1) 
- partA/c16k/q4/w4k/rep3: 50482.711526 IOPS, 0.21 GB/s, p99 233832.448 us, daemon CPU 120.840s (39.89 us/IO), rss 1565004 KiB, split lunix-coord=31.18(1) lunix-q2=10.67(1) lunix-q3=10.65(1) iou-wrk-1262061=0.79(1) iou-wrk-1262062=0.75(1) lunix-space-tim=0.42(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 0)
