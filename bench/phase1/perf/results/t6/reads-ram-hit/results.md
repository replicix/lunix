# M16 Part A (2026-11-20T14:31:59Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4 8, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 18000 s
- load average at the start: 3.92 111.76 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 111.76 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k
- PASS read_cache_max = 4294967296

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569220 KiB
- PASS write the 2 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c64k/q4/r4k/rep1: 372063.726961 IOPS, 1.52 GB/s, p99 19267.584 us, daemon CPU 60.060s (5.38 us/IO), rss 2978372 KiB, split lunix-q2=30.00(1) lunix-q3=29.98(1) 
- partA/c64k/q4/r4k/rep2: 374371.129554 IOPS, 1.53 GB/s, p99 19267.584 us, daemon CPU 60.070s (5.18 us/IO), rss 2978372 KiB, split lunix-q2=29.99(1) lunix-q3=29.99(1) 
- partA/c64k/q4/r4k/rep3: 374474.970004 IOPS, 1.53 GB/s, p99 19267.584 us, daemon CPU 60.070s (5.35 us/IO), rss 2978372 KiB, split lunix-q2=30.00(1) lunix-q3=29.98(1) 
- partA/c64k/q4/seqr/rep1: 9565.932785 IOPS, 10.04 GB/s, p99 27394.048 us, daemon CPU 60.070s (202.57 us/IO), rss 2978484 KiB, split lunix-q2=30.00(1) lunix-q3=29.99(1) 
- partA/c64k/q4/seqr/rep2: 9497.235545 IOPS, 9.97 GB/s, p99 27656.192 us, daemon CPU 60.060s (210.80 us/IO), rss 2978484 KiB, split lunix-q2=29.98(1) lunix-q3=30.00(1) 
- partA/c64k/q4/seqr/rep3: 9384.348869 IOPS, 9.85 GB/s, p99 28180.48 us, daemon CPU 60.060s (206.45 us/IO), rss 2978484 KiB, split lunix-q2=30.00(1) lunix-q3=29.99(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 64 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1412012 KiB
- PASS write the 2 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c64k/q8/r4k/rep1: 898602.413253 IOPS, 3.68 GB/s, p99 4358.144 us, daemon CPU 111.840s (4.15 us/IO), rss 3675632 KiB, split lunix-q4=27.87(1) lunix-q7=27.98(1) lunix-q5=27.95(1) lunix-q6=27.91(1) 
- partA/c64k/q8/r4k/rep2: 897423.005133 IOPS, 3.68 GB/s, p99 4358.144 us, daemon CPU 111.550s (4.01 us/IO), rss 3675632 KiB, split lunix-q4=27.80(1) lunix-q7=27.91(1) lunix-q5=27.88(1) lunix-q6=27.84(1) 
- partA/c64k/q8/r4k/rep3: 890047.165094 IOPS, 3.65 GB/s, p99 4423.68 us, daemon CPU 111.220s (4.17 us/IO), rss 3675632 KiB, split lunix-q4=27.70(1) lunix-q5=27.86(1) lunix-q7=27.77(1) lunix-q6=27.76(1) 
- partA/c64k/q8/seqr/rep1: 11351.2226 IOPS, 11.91 GB/s, p99 43778.048 us, daemon CPU 107.290s (304.90 us/IO), rss 3678108 KiB, split lunix-q5=29.99(1) lunix-q7=28.94(1) lunix-q4=27.62(1) lunix-q6=20.62(1) 
- partA/c64k/q8/seqr/rep2: 11037.979744 IOPS, 11.58 GB/s, p99 43253.76 us, daemon CPU 92.260s (278.61 us/IO), rss 3678112 KiB, split lunix-q5=29.99(1) lunix-q7=30.00(1) lunix-q4=29.99(1) lunix-q6=2.16(1) 
- partA/c64k/q8/seqr/rep3: 11403.937769 IOPS, 11.97 GB/s, p99 43778.048 us, daemon CPU 111.620s (315.74 us/IO), rss 3678116 KiB, split lunix-q5=29.99(1) lunix-q7=30.01(1) lunix-q4=21.49(1) lunix-q6=30.00(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

## chunk 16 KiB
- PASS create pa16k
- PASS read_cache_max = 4294967296

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569224 KiB
- PASS write the 2 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c16k/q4/r4k/rep1: 357260.140653 IOPS, 1.46 GB/s, p99 19529.728 us, daemon CPU 60.140s (5.61 us/IO), rss 2927856 KiB, split lunix-q2=29.98(1) lunix-q3=30.00(1) lunix-coord=0.11(1) 
- partA/c16k/q4/r4k/rep2: 357257.840883 IOPS, 1.46 GB/s, p99 19005.44 us, daemon CPU 60.150s (5.61 us/IO), rss 2927856 KiB, split lunix-q2=29.99(1) lunix-q3=29.98(1) lunix-coord=0.11(1) 
- partA/c16k/q4/r4k/rep3: 358868.85082 IOPS, 1.47 GB/s, p99 19529.728 us, daemon CPU 60.040s (5.40 us/IO), rss 2927856 KiB, split lunix-q2=30.00(1) lunix-q3=29.98(1) 
- partA/c16k/q4/seqr/rep1: 8244.696793 IOPS, 8.65 GB/s, p99 31850.496 us, daemon CPU 60.080s (242.90 us/IO), rss 2928752 KiB, split lunix-q2=30.00(1) lunix-q3=30.01(1) 
- partA/c16k/q4/seqr/rep2: 8179.758811 IOPS, 8.59 GB/s, p99 31850.496 us, daemon CPU 60.050s (244.71 us/IO), rss 2928752 KiB, split lunix-q2=29.99(1) lunix-q3=29.99(1) 
- partA/c16k/q4/seqr/rep3: 8087.825681 IOPS, 8.49 GB/s, p99 32374.784 us, daemon CPU 60.040s (247.45 us/IO), rss 2928752 KiB, split lunix-q2=29.99(1) lunix-q3=29.98(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 16 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1385536 KiB
- PASS write the 2 GiB working set
- PASS read the working set once (cache warm-up)
- partA/c16k/q8/r4k/rep1: 763178.688087 IOPS, 3.13 GB/s, p99 5472.256 us, daemon CPU 106.430s (4.65 us/IO), rss 3638736 KiB, split lunix-q5=26.61(1) lunix-q6=26.53(1) lunix-q4=26.52(1) lunix-q7=26.56(1) lunix-coord=0.11(1) 
- partA/c16k/q8/r4k/rep2: 789738.284114 IOPS, 3.23 GB/s, p99 5275.648 us, daemon CPU 107.890s (4.41 us/IO), rss 3638736 KiB, split lunix-q5=27.03(1) lunix-q6=26.89(1) lunix-q7=26.90(1) lunix-q4=26.83(1) lunix-coord=0.12(1) 
- partA/c16k/q8/r4k/rep3: 791405.306313 IOPS, 3.24 GB/s, p99 5275.648 us, daemon CPU 108.180s (4.56 us/IO), rss 3638736 KiB, split lunix-q5=27.09(1) lunix-q6=26.98(1) lunix-q7=26.97(1) lunix-q4=27.02(1) 
- partA/c16k/q8/seqr/rep1: 11017.121348 IOPS, 11.56 GB/s, p99 46399.488 us, daemon CPU 114.840s (336.25 us/IO), rss 3639708 KiB, split lunix-q5=29.96(1) lunix-q6=29.93(1) lunix-q4=29.95(1) lunix-q7=24.88(1) 
- partA/c16k/q8/seqr/rep2: 10911.896913 IOPS, 11.45 GB/s, p99 45875.2 us, daemon CPU 111.350s (340.15 us/IO), rss 3639708 KiB, split lunix-q5=29.98(1) lunix-q4=29.95(1) lunix-q6=27.26(1) lunix-q7=24.05(1) 
- partA/c16k/q8/seqr/rep3: 10896.342682 IOPS, 11.43 GB/s, p99 39059.456 us, daemon CPU 110.110s (325.98 us/IO), rss 3639708 KiB, split lunix-q5=29.67(1) lunix-q6=29.97(1) lunix-q4=21.20(1) lunix-q7=29.14(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

overall FAIL=0 (noisy host: 1)
