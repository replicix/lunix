# M16 Part A (2026-11-21T20:19:25Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 15.37 148.12 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 148.12 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569272 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 3850.377132 IOPS, 0.02 GB/s, p99 5133828.096 us, daemon CPU 23.600s (87.56 us/IO), rss 1271316 KiB, split lunix-coord=3.95(9) lunix-q2=1.03(1) lunix-q3=1.02(1) lunix-space-tim=0.08(1) 
- partA/c64k/q4/seqw/rep1: 49.306179 IOPS, 0.06 GB/s, p99 15367929.856 us, daemon CPU 8.690s (3325.39 us/IO), rss 1705108 KiB, split lunix-coord=2.49(1) lunix-q2=0.30(1) lunix-q3=0.28(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569192 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 9159.763022 IOPS, 0.04 GB/s, p99 1602224.128 us, daemon CPU 30.820s (52.57 us/IO), rss 1404804 KiB, split lunix-coord=10.66(9) lunix-q2=2.43(1) lunix-q3=2.37(1) lunix-space-tim=0.13(1) 
- partA/c16k/q4/seqw/rep1: 66.015316 IOPS, 0.08 GB/s, p99 6207569.92 us, daemon CPU 10.470s (2937.03 us/IO), rss 1837624 KiB, split lunix-coord=5.59(1) lunix-q2=0.36(1) lunix-q3=0.38(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
