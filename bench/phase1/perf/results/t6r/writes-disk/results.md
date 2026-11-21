# M16 Part A (2026-11-20T23:54:01Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 4.82 14.52 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 14.52 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569316 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 2993.805479 IOPS, 0.01 GB/s, p99 6207569.92 us, daemon CPU 20.080s (106.46 us/IO), rss 1224384 KiB, split lunix-coord=4.23(1) lunix-q3=1.04(1) lunix-q2=0.97(1) lunix-space-tim=0.16(1) 
- partA/c64k/q4/w4k/rep2: 3490.743035 IOPS, 0.01 GB/s, p99 5737807.872 us, daemon CPU 20.190s (94.82 us/IO), rss 1341296 KiB, split lunix-coord=5.30(1) lunix-q3=0.87(1) lunix-q2=0.87(1) lunix-space-tim=0.11(1) 
- partA/c64k/q4/w4k/rep3: 3101.16073 IOPS, 0.01 GB/s, p99 6744440.832 us, daemon CPU 21.430s (107.97 us/IO), rss 1309888 KiB, split lunix-coord=5.04(1) lunix-q3=0.91(1) lunix-q2=0.89(1) lunix-space-tim=0.14(1) 
- partA/c64k/q4/seqw/rep1: 44.479079 IOPS, 0.05 GB/s, p99 10670309.376 us, daemon CPU 8.460s (3522.26 us/IO), rss 1608148 KiB, split lunix-coord=3.25(1) lunix-q3=0.26(1) lunix-q2=0.25(1) 
- partA/c64k/q4/seqw/rep2: 64.120411 IOPS, 0.07 GB/s, p99 6677331.968 us, daemon CPU 5.540s (2468.57 us/IO), rss 1797160 KiB, split lunix-coord=2.03(1) lunix-q3=0.32(1) lunix-q2=0.33(1) 
- partA/c64k/q4/seqw/rep3: 62.943156 IOPS, 0.07 GB/s, p99 7147094.016 us, daemon CPU 4.990s (2331.70 us/IO), rss 1802284 KiB, split lunix-coord=1.67(1) lunix-q3=0.33(1) lunix-q2=0.34(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569416 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 9079.328363 IOPS, 0.04 GB/s, p99 1786773.504 us, daemon CPU 27.770s (50.98 us/IO), rss 1384572 KiB, split lunix-coord=10.68(9) lunix-q2=2.30(1) lunix-q3=2.30(1) lunix-space-tim=0.15(1) 
- partA/c16k/q4/w4k/rep2: 8062.084945 IOPS, 0.03 GB/s, p99 1719664.64 us, daemon CPU 29.880s (59.78 us/IO), rss 1500388 KiB, split lunix-coord=12.70(1) lunix-q2=1.98(1) lunix-q3=1.99(1) lunix-space-tim=0.13(1) 
- partA/c16k/q4/w4k/rep3: 8980.698835 IOPS, 0.04 GB/s, p99 1619001.344 us, daemon CPU 30.250s (56.14 us/IO), rss 1481668 KiB, split lunix-coord=12.30(1) lunix-q2=2.19(1) lunix-q3=2.19(1) lunix-space-tim=0.12(1) 
- partA/c16k/q4/seqw/rep1: 67.953045 IOPS, 0.08 GB/s, p99 6140461.056 us, daemon CPU 15.100s (4115.04 us/IO), rss 1833112 KiB, split lunix-coord=10.06(1) lunix-q2=0.35(1) lunix-q3=0.33(1) 
- partA/c16k/q4/seqw/rep2: 65.72589 IOPS, 0.08 GB/s, p99 6408896.512 us, daemon CPU 8.120s (3633.63 us/IO), rss 1731204 KiB, split lunix-coord=3.66(1) lunix-q2=0.36(1) lunix-q3=0.33(1) 
- partA/c16k/q4/seqw/rep3: 67.692767 IOPS, 0.08 GB/s, p99 6207569.92 us, daemon CPU 9.640s (4188.47 us/IO), rss 1757040 KiB, split lunix-coord=5.24(1) lunix-q2=0.36(1) lunix-q3=0.34(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
