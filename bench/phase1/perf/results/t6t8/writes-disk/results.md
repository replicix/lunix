# M16 Part A (2026-11-21T20:03:10Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 5.15 14.90 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 14.90 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569348 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 3527.337117 IOPS, 0.01 GB/s, p99 4932501.504 us, daemon CPU 21.750s (93.43 us/IO), rss 1284712 KiB, split lunix-coord=4.32(9) lunix-q3=1.02(1) lunix-q2=1.04(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/w4k/rep2: 2903.265669 IOPS, 0.01 GB/s, p99 8489271.296 us, daemon CPU 22.240s (116.07 us/IO), rss 1319376 KiB, split lunix-coord=4.12(1) lunix-q3=0.76(1) lunix-q2=0.86(1) lunix-space-tim=0.07(1) 
- partA/c64k/q4/w4k/rep3: 3348.182903 IOPS, 0.01 GB/s, p99 4865392.64 us, daemon CPU 18.780s (91.95 us/IO), rss 1317500 KiB, split lunix-coord=2.88(1) lunix-q3=1.02(1) lunix-q2=0.99(1) lunix-space-tim=0.12(1) 
- partA/c64k/q4/seqw/rep1: 44.459012 IOPS, 0.05 GB/s, p99 14294188.032 us, daemon CPU 6.900s (4434.26 us/IO), rss 1779476 KiB, split lunix-coord=1.23(1) lunix-q3=0.26(1) lunix-q2=0.27(1) 
- partA/c64k/q4/seqw/rep2: 63.360882 IOPS, 0.07 GB/s, p99 7549747.2 us, daemon CPU 5.090s (2362.75 us/IO), rss 1789360 KiB, split lunix-coord=1.29(1) lunix-q3=0.35(1) lunix-q2=0.35(1) 
- partA/c64k/q4/seqw/rep3: 65.90882 IOPS, 0.08 GB/s, p99 7147094.016 us, daemon CPU 5.140s (2363.23 us/IO), rss 1784400 KiB, split lunix-coord=1.42(1) lunix-q3=0.35(1) lunix-q2=0.36(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569348 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 9178.232809 IOPS, 0.04 GB/s, p99 1417674.752 us, daemon CPU 27.430s (48.99 us/IO), rss 1350952 KiB, split lunix-coord=8.89(1) lunix-q2=2.43(1) lunix-q3=2.43(1) lunix-space-tim=0.14(1) 
- partA/c16k/q4/w4k/rep2: 8481.571991 IOPS, 0.03 GB/s, p99 1451229.184 us, daemon CPU 29.210s (57.40 us/IO), rss 1542832 KiB, split lunix-coord=11.38(9) lunix-q2=2.06(1) lunix-q3=2.09(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/w4k/rep3: 8738.72558 IOPS, 0.04 GB/s, p99 1333788.672 us, daemon CPU 30.650s (57.50 us/IO), rss 1556332 KiB, split lunix-coord=11.41(1) lunix-q2=2.18(1) lunix-q3=2.15(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/seqw/rep1: 67.580626 IOPS, 0.08 GB/s, p99 6341787.648 us, daemon CPU 9.180s (3995.23 us/IO), rss 1995120 KiB, split lunix-coord=4.30(1) lunix-q2=0.35(1) lunix-q3=0.36(1) 
- partA/c16k/q4/seqw/rep2: 67.817546 IOPS, 0.08 GB/s, p99 6274678.784 us, daemon CPU 10.040s (4486.19 us/IO), rss 1995356 KiB, split lunix-coord=5.10(1) lunix-q2=0.37(1) lunix-q3=0.34(1) 
- partA/c16k/q4/seqw/rep3: 66.487133 IOPS, 0.08 GB/s, p99 6274678.784 us, daemon CPU 9.690s (4286.54 us/IO), rss 1984228 KiB, split lunix-coord=4.88(1) lunix-q2=0.35(1) lunix-q3=0.36(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
