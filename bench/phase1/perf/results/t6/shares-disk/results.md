# M16 Part A (2026-11-20T12:04:38Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 49.41 122.05 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 122.05 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569176 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 2903.734826 IOPS, 0.01 GB/s, p99 10804527.104 us, daemon CPU 17.270s (92.93 us/IO), rss 1264940 KiB, split lunix-coord=4.46(9) lunix-q3=0.51(1) lunix-q2=0.52(1) 
- partA/c64k/q4/seqw/rep1: 25.717219 IOPS, 0.03 GB/s, p99 16978542.592 us, daemon CPU 6.280s (4522.12 us/IO), rss 1684560 KiB, split lunix-coord=1.65(1) lunix-q3=0.17(1) lunix-q2=0.17(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569328 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 6917.178165 IOPS, 0.03 GB/s, p99 3405774.848 us, daemon CPU 22.340s (52.09 us/IO), rss 1371564 KiB, split lunix-coord=9.30(9) lunix-q2=1.18(1) lunix-q3=1.16(1) 
- partA/c16k/q4/seqw/rep1: 37.378464 IOPS, 0.05 GB/s, p99 12012486.656 us, daemon CPU 8.720s (4320.17 us/IO), rss 1726064 KiB, split lunix-coord=4.90(1) lunix-q2=0.23(1) lunix-q3=0.21(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
