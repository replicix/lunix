# M16 Part A (2026-11-20T12:38:03Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4 8, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 18000 s
- load average at the start: 19.68 38.14 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 38.14 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k
- PASS read_cache_max = 8589934592

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569288 KiB
- PASS write the 4 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c64k/q4/r4k/rep1: 349994.335599 IOPS, 1.43 GB/s, p99 20316.16 us, daemon CPU 58.920s (5.61 us/IO), rss 5231300 KiB, split lunix-q3=29.17(1) lunix-q2=29.08(1) lunix-coord=0.22(1) lunix-read-3=0.06(1) lunix-read-2=0.06(1) lunix-read-1=0.06(1) lunix-read-0=0.06(1) 
- partA/c64k/q4/r4k/rep2: 370672.510332 IOPS, 1.52 GB/s, p99 19005.44 us, daemon CPU 58.510s (5.26 us/IO), rss 5232332 KiB, split lunix-q3=29.09(1) lunix-q2=29.10(1) lunix-coord=0.23(1) 
- partA/c64k/q4/r4k/rep3: 362287.571243 IOPS, 1.48 GB/s, p99 19267.584 us, daemon CPU 59.130s (5.26 us/IO), rss 5232356 KiB, split lunix-q3=29.46(1) lunix-q2=29.54(1) lunix-coord=0.07(1) 
- partA/c64k/q4/seqr/rep1: 8886.857181 IOPS, 9.33 GB/s, p99 39583.744 us, daemon CPU 60.030s (225.16 us/IO), rss 5230272 KiB, split lunix-q3=29.95(1) lunix-q2=30.01(1) 
- partA/c64k/q4/seqr/rep2: 8759.258026 IOPS, 9.19 GB/s, p99 39059.456 us, daemon CPU 60.020s (221.04 us/IO), rss 5230188 KiB, split lunix-q3=29.98(1) lunix-q2=29.96(1) 
- partA/c64k/q4/seqr/rep3: 8897.718568 IOPS, 9.34 GB/s, p99 32112.64 us, daemon CPU 60.060s (225.00 us/IO), rss 5230188 KiB, split lunix-q3=30.00(1) lunix-q2=29.99(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 64 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1151140 KiB
- PASS write the 4 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c64k/q8/r4k/rep1: 798077.828145 IOPS, 3.27 GB/s, p99 5144.576 us, daemon CPU 107.040s (4.47 us/IO), rss 5744100 KiB, split lunix-q6=26.66(1) lunix-q4=26.73(1) lunix-q5=26.76(1) lunix-q7=26.71(1) lunix-coord=0.06(1) 
- partA/c64k/q8/r4k/rep2: 805139.15739 IOPS, 3.30 GB/s, p99 5079.04 us, daemon CPU 107.200s (4.29 us/IO), rss 5744160 KiB, split lunix-q6=26.71(1) lunix-q4=26.74(1) lunix-q5=26.76(1) lunix-q7=26.78(1) lunix-coord=0.11(1) 
- partA/c64k/q8/r4k/rep3: 802540.830611 IOPS, 3.29 GB/s, p99 5144.576 us, daemon CPU 106.970s (4.44 us/IO), rss 5744228 KiB, split lunix-q6=26.65(1) lunix-q5=26.84(1) lunix-q4=26.65(1) lunix-q7=26.71(1) 
- partA/c64k/q8/seqr/rep1: 11447.819859 IOPS, 12.01 GB/s, p99 44826.624 us, daemon CPU 118.490s (333.89 us/IO), rss 5745056 KiB, split lunix-q6=29.99(1) lunix-q4=29.80(1) lunix-q5=30.00(1) lunix-q7=28.59(1) 
- partA/c64k/q8/seqr/rep2: 10997.868514 IOPS, 11.54 GB/s, p99 44302.336 us, daemon CPU 102.070s (309.36 us/IO), rss 5745056 KiB, split lunix-q4=30.00(1) lunix-q5=30.00(1) lunix-q6=23.77(1) lunix-q7=18.19(1) 
- partA/c64k/q8/seqr/rep3: 10653.455454 IOPS, 11.18 GB/s, p99 44826.624 us, daemon CPU 92.280s (279.42 us/IO), rss 5745056 KiB, split lunix-q5=29.99(1) lunix-q4=25.69(1) lunix-q6=30.00(1) lunix-q7=6.46(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

## chunk 16 KiB
- PASS create pa16k
- PASS read_cache_max = 8589934592

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569152 KiB
- PASS write the 4 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c16k/q4/r4k/rep1: 355016.23171 IOPS, 1.45 GB/s, p99 20054.016 us, daemon CPU 60.450s (5.68 us/IO), rss 5123272 KiB, split lunix-q3=29.98(1) lunix-q2=29.97(1) lunix-coord=0.42(1) 
- partA/c16k/q4/r4k/rep2: 356149.351731 IOPS, 1.46 GB/s, p99 20054.016 us, daemon CPU 60.250s (5.64 us/IO), rss 5123416 KiB, split lunix-q3=29.99(1) lunix-q2=29.99(1) lunix-coord=0.21(1) 
- partA/c16k/q4/r4k/rep3: 356696.507132 IOPS, 1.46 GB/s, p99 20054.016 us, daemon CPU 60.050s (5.61 us/IO), rss 5123416 KiB, split lunix-q3=29.97(1) lunix-q2=29.99(1) 
- partA/c16k/q4/seqr/rep1: 7810.755911 IOPS, 8.20 GB/s, p99 33423.36 us, daemon CPU 60.080s (256.40 us/IO), rss 5124312 KiB, split lunix-q3=30.00(1) lunix-q2=30.00(1) 
- partA/c16k/q4/seqr/rep2: 7801.019388 IOPS, 8.19 GB/s, p99 33423.36 us, daemon CPU 60.060s (248.35 us/IO), rss 5124312 KiB, split lunix-q3=30.00(1) lunix-q2=30.01(1) 
- partA/c16k/q4/seqr/rep3: 7723.423393 IOPS, 8.11 GB/s, p99 33816.576 us, daemon CPU 60.070s (259.25 us/IO), rss 5124312 KiB, split lunix-q3=30.00(1) lunix-q2=30.00(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 16 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1156748 KiB
- PASS write the 4 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c16k/q8/r4k/rep1: 737600.273306 IOPS, 3.02 GB/s, p99 5603.328 us, daemon CPU 104.690s (4.73 us/IO), rss 5594412 KiB, split lunix-q5=26.06(1) lunix-q4=26.06(1) lunix-q6=26.04(1) lunix-q7=26.00(1) lunix-coord=0.42(1) 
- partA/c16k/q8/r4k/rep2: 743071.895207 IOPS, 3.04 GB/s, p99 5603.328 us, daemon CPU 104.820s (4.55 us/IO), rss 5594412 KiB, split lunix-q5=26.18(1) lunix-q4=26.16(1) lunix-q6=26.08(1) lunix-q7=26.08(1) lunix-coord=0.22(1) 
- partA/c16k/q8/r4k/rep3: 741220.551963 IOPS, 3.04 GB/s, p99 5603.328 us, daemon CPU 104.290s (4.69 us/IO), rss 5594412 KiB, split lunix-q5=26.02(1) lunix-q4=26.16(1) lunix-q7=26.03(1) lunix-q6=25.97(1) 
- partA/c16k/q8/seqr/rep1: 10770.670931 IOPS, 11.30 GB/s, p99 46399.488 us, daemon CPU 113.360s (339.51 us/IO), rss 5597404 KiB, split lunix-q5=29.94(1) lunix-q6=29.97(1) lunix-q7=28.11(1) lunix-q4=25.22(1) 
- partA/c16k/q8/seqr/rep2: 10585.084768 IOPS, 11.11 GB/s, p99 46399.488 us, daemon CPU 109.720s (345.52 us/IO), rss 5597404 KiB, split lunix-q5=29.10(1) lunix-q7=29.26(1) lunix-q4=29.96(1) lunix-q6=21.26(1) 
- partA/c16k/q8/seqr/rep3: 10716.398029 IOPS, 11.25 GB/s, p99 39059.456 us, daemon CPU 113.580s (341.89 us/IO), rss 5597404 KiB, split lunix-q5=29.95(1) lunix-q4=29.95(1) lunix-q6=29.97(1) lunix-q7=23.58(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

overall FAIL=0 (noisy host: 1)
