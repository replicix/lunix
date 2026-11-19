# M16 Part A (2026-11-19T02:21:57Z, kernel 7.3, 16 cpus): chunks 65536, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 2400 s
- load average at the start: 0.26 68.24 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 68.24 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568980 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 2763.02449 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 12.520s (59.62 us/IO), rss 687952 KiB, split lunix-coord=1.82(1) lunix-q3=0.59(1) lunix-q2=0.43(1) 
- partA/c64k/q4/seqw/rep1: 33.521337 IOPS, 0.04 GB/s, p99 11744051.2 us, daemon CPU 2.970s (1926.09 us/IO), rss 700868 KiB, split lunix-coord=0.55(9) lunix-q3=0.18(1) lunix-q2=0.19(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
