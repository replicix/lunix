# M16 Part A (2026-11-19T01:53:22Z, kernel 7.3, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 5400 s
- load average at the start: 0.00 0.64 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569128 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 2283.973433 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 15.190s (72.29 us/IO), rss 727708 KiB, split lunix-coord=2.64(1) lunix-q2=0.45(1) lunix-q3=0.47(1) 
- partA/c64k/q4/w4k/rep2: 2867.32856 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 13.590s (51.52 us/IO), rss 741204 KiB, split lunix-coord=2.82(1) lunix-q2=0.43(1) lunix-q3=0.42(1) 
- partA/c64k/q4/w4k/rep3: 2419.694767 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 14.250s (61.35 us/IO), rss 754260 KiB, split lunix-coord=2.49(1) lunix-q2=0.41(1) lunix-q3=0.40(1) 
- partA/c64k/q4/seqw/rep1: 32.478877 IOPS, 0.04 GB/s, p99 12280922.112 us, daemon CPU 2.800s (2052.62 us/IO), rss 751792 KiB, split lunix-coord=0.51(1) lunix-q2=0.17(1) lunix-q3=0.17(1) 
- partA/c64k/q4/seqw/rep2: 32.679174 IOPS, 0.04 GB/s, p99 12146704.384 us, daemon CPU 2.620s (1908.89 us/IO), rss 752320 KiB, split lunix-coord=0.40(1) lunix-q2=0.16(1) lunix-q3=0.16(1) 
- partA/c64k/q4/seqw/rep3: 32.500287 IOPS, 0.04 GB/s, p99 12012486.656 us, daemon CPU 2.670s (1956.03 us/IO), rss 752720 KiB, split lunix-coord=0.50(1) lunix-q2=0.16(1) lunix-q3=0.16(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569076 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 2440.788711 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 13.410s (49.05 us/IO), rss 753816 KiB, split lunix-coord=3.71(1) lunix-q3=0.58(1) lunix-q2=0.59(1) 
- partA/c16k/q4/w4k/rep2: 3976.181929 IOPS, 0.02 GB/s, p99 8556380.16 us, daemon CPU 11.880s (30.80 us/IO), rss 796536 KiB, split lunix-coord=3.43(1) lunix-q3=0.70(1) lunix-q2=0.71(1) 
- partA/c16k/q4/w4k/rep3: ? IOPS, 0.00 GB/s, p99 ? us, daemon CPU 14.260s (? us/IO), rss 878836 KiB, split lunix-coord=3.94(1) lunix-q3=0.69(1) lunix-q2=0.70(1) 
- partA/c16k/q4/seqw/rep1: 25.31024 IOPS, 0.03 GB/s, p99 17112760.32 us, daemon CPU 5.480s (3866.31 us/IO), rss 899452 KiB, split lunix-coord=1.65(1) lunix-q3=0.16(1) lunix-q2=0.16(1) 
- partA/c16k/q4/seqw/rep2: 28.250417 IOPS, 0.04 GB/s, p99 15770583.04 us, daemon CPU 4.060s (2817.94 us/IO), rss 899452 KiB, split lunix-coord=1.00(1) lunix-q3=0.16(1) lunix-q2=0.16(1) 
- partA/c16k/q4/seqw/rep3: 28.13826 IOPS, 0.04 GB/s, p99 15904800.768 us, daemon CPU 4.080s (2843.10 us/IO), rss 899464 KiB, split lunix-coord=0.99(1) lunix-q3=0.16(1) lunix-q2=0.15(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=1 (noisy host: 0)
