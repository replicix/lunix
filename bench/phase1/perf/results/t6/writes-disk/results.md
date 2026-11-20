# M16 Part A (2026-11-20T10:04:02Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 7.87 34.28 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 34.28 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569240 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/r4k/rep1: 54024.159419 IOPS, 0.22 GB/s, p99 367001.6 us, daemon CPU 38.690s (23.87 us/IO), rss 1237356 KiB, split lunix-q3=6.13(1) lunix-read-1=6.54(1) lunix-read-0=6.54(1) lunix-read-3=6.52(1) lunix-read-2=6.51(1) lunix-q2=6.11(1) lunix-coord=0.14(1) 
- partA/c64k/q4/r4k/rep2: 243693.484419 IOPS, 1.00 GB/s, p99 32112.64 us, daemon CPU 168.220s (22.27 us/IO), rss 1242436 KiB, split lunix-read-0=29.08(1) lunix-read-2=29.07(1) lunix-read-3=29.04(1) lunix-read-1=28.98(1) lunix-q3=25.87(1) lunix-q2=25.91(1) lunix-coord=0.11(1) 
- partA/c64k/q4/r4k/rep3: 250277.087152 IOPS, 1.03 GB/s, p99 30539.776 us, daemon CPU 173.920s (23.16 us/IO), rss 1242300 KiB, split lunix-read-0=29.60(1) lunix-read-2=29.60(1) lunix-read-3=29.60(1) lunix-read-1=29.60(1) lunix-q3=27.59(1) lunix-q2=27.66(1) lunix-coord=0.11(1) 
- partA/c64k/q4/w4k/rep1: 3268.006187 IOPS, 0.01 GB/s, p99 8791261.184 us, daemon CPU 19.410s (89.99 us/IO), rss 1353728 KiB, split lunix-q3=0.57(1) lunix-q2=0.57(1) lunix-coord=5.49(1) 
- partA/c64k/q4/w4k/rep2: 3567.365098 IOPS, 0.01 GB/s, p99 8019509.248 us, daemon CPU 18.180s (82.20 us/IO), rss 1421708 KiB, split lunix-q3=0.50(1) lunix-q2=0.53(1) lunix-coord=4.23(1) 
- partA/c64k/q4/w4k/rep3: 2584.709769 IOPS, 0.01 GB/s, p99 11878268.928 us, daemon CPU 16.680s (93.53 us/IO), rss 1413340 KiB, split lunix-q3=0.40(1) lunix-q2=0.45(1) lunix-coord=4.15(1) 
- partA/c64k/q4/seqw/rep1: 13.529166 IOPS, 0.02 GB/s, p99 17112760.32 us, daemon CPU 5.890s (7378.91 us/IO), rss 1921104 KiB, split lunix-q3=0.13(1) lunix-q2=0.13(1) lunix-coord=1.24(1) 
- partA/c64k/q4/seqw/rep2: 33.422495 IOPS, 0.04 GB/s, p99 10401873.92 us, daemon CPU 3.670s (2745.16 us/IO), rss 1870212 KiB, split lunix-q3=0.21(1) lunix-q2=0.21(1) lunix-coord=1.46(1) 
- partA/c64k/q4/seqw/rep3: 37.453386 IOPS, 0.05 GB/s, p99 11609833.472 us, daemon CPU 3.970s (2789.43 us/IO), rss 1871372 KiB, split lunix-q3=0.22(1) lunix-q2=0.24(1) lunix-coord=1.67(1) 
- partA/c64k/q4/seqr/rep1: 7778.09232 IOPS, 8.16 GB/s, p99 47448.064 us, daemon CPU 138.660s (540.21 us/IO), rss 1659952 KiB, split lunix-q3=29.55(1) lunix-q2=29.57(1) lunix-read-3=19.59(1) lunix-read-2=19.58(1) lunix-read-0=19.56(1) lunix-read-1=19.60(1) lunix-coord=1.11(1) 
- partA/c64k/q4/seqr/rep2: 7436.017852 IOPS, 7.81 GB/s, p99 79167.488 us, daemon CPU 132.960s (596.02 us/IO), rss 1594420 KiB, split lunix-q3=28.85(1) lunix-q2=28.88(1) lunix-read-0=18.76(1) lunix-read-3=18.73(1) lunix-read-2=18.72(1) lunix-read-1=18.75(1) lunix-coord=0.17(1) 
- partA/c64k/q4/seqr/rep3: 7546.644463 IOPS, 7.92 GB/s, p99 60555.264 us, daemon CPU 136.290s (601.99 us/IO), rss 1587212 KiB, split lunix-q3=29.34(1) lunix-q2=29.36(1) lunix-read-0=19.36(1) lunix-read-3=19.40(1) lunix-read-2=19.37(1) lunix-read-1=19.39(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569204 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/r4k/rep1: 217972.870284 IOPS, 0.89 GB/s, p99 31850.496 us, daemon CPU 104.020s (5.49 us/IO), rss 671212 KiB, split lunix-q2=20.80(1) lunix-q3=20.75(1) lunix-read-1=15.28(1) lunix-read-2=15.24(1) lunix-read-0=15.21(1) lunix-read-3=15.14(1) lunix-coord=1.16(1) 
- partA/c16k/q4/r4k/rep2: 228395.993734 IOPS, 0.94 GB/s, p99 29753.344 us, daemon CPU 109.640s (16.00 us/IO), rss 813524 KiB, split lunix-q2=23.04(1) lunix-q3=22.93(1) lunix-read-1=15.82(1) lunix-read-2=15.80(1) lunix-read-0=15.81(1) lunix-read-3=15.80(1) lunix-coord=0.32(1) 
- partA/c16k/q4/r4k/rep3: 231051.62645 IOPS, 0.95 GB/s, p99 28966.912 us, daemon CPU 109.680s (15.82 us/IO), rss 813608 KiB, split lunix-q2=23.22(1) lunix-q3=23.09(1) lunix-read-1=15.81(1) lunix-read-0=15.83(1) lunix-read-2=15.79(1) lunix-read-3=15.81(1) 
- partA/c16k/q4/w4k/rep1: 6743.926964 IOPS, 0.03 GB/s, p99 4395630.592 us, daemon CPU 22.040s (51.88 us/IO), rss 1480880 KiB, split lunix-q2=1.17(1) lunix-q3=1.17(1) lunix-coord=8.90(1) 
- partA/c16k/q4/w4k/rep2: 7078.897264 IOPS, 0.03 GB/s, p99 4110417.92 us, daemon CPU 22.110s (48.80 us/IO), rss 1661832 KiB, split lunix-q2=1.13(1) lunix-q3=1.06(1) lunix-coord=8.78(9) 
- partA/c16k/q4/w4k/rep3: 4969.560614 IOPS, 0.02 GB/s, p99 5335154.688 us, daemon CPU 16.300s (44.93 us/IO), rss 1637964 KiB, split lunix-q2=0.76(1) lunix-q3=0.77(1) lunix-coord=4.97(1) 
- partA/c16k/q4/seqw/rep1: 16.348119 IOPS, 0.02 GB/s, p99 17112760.32 us, daemon CPU 5.220s (3893.94 us/IO), rss 1361520 KiB, split lunix-q2=0.16(1) lunix-q3=0.17(1) lunix-coord=1.29(9) 
- partA/c16k/q4/seqw/rep2: ? IOPS, 0.00 GB/s, p99 ? us, daemon CPU 1.610s (? us/IO), rss 1607960 KiB, split lunix-q2=0.10(1) lunix-q3=0.08(1) lunix-coord=0.47(1) 
- partA/c16k/q4/seqw/rep3: 33.986965 IOPS, 0.04 GB/s, p99 17112760.32 us, daemon CPU 3.870s (2996.50 us/IO), rss 1737716 KiB, split lunix-q2=0.20(1) lunix-q3=0.23(1) lunix-coord=1.16(1) 
- partA/c16k/q4/seqr/rep1: 1200.931315 IOPS, 1.27 GB/s, p99 3003121.664 us, daemon CPU 36.930s (960.97 us/IO), rss 1910800 KiB, split lunix-q2=5.22(1) lunix-q3=5.03(1) lunix-read-1=6.49(1) lunix-read-0=6.47(1) lunix-read-2=6.48(1) lunix-read-3=6.47(1) lunix-coord=0.58(1) 
- partA/c16k/q4/seqr/rep2: 3724.990023 IOPS, 3.91 GB/s, p99 88604.672 us, daemon CPU 115.920s (1037.32 us/IO), rss 1925004 KiB, split lunix-q2=16.14(1) lunix-q3=16.13(1) lunix-read-1=20.28(1) lunix-read-0=20.31(1) lunix-read-2=20.30(1) lunix-read-3=20.32(1) lunix-coord=2.29(1) 
- partA/c16k/q4/seqr/rep3: 3751.139047 IOPS, 3.94 GB/s, p99 74973.184 us, daemon CPU 121.290s (1043.04 us/IO), rss 1919856 KiB, split lunix-q2=16.31(1) lunix-q3=16.29(1) lunix-read-1=20.20(1) lunix-read-0=20.19(1) lunix-read-3=20.21(1) lunix-read-2=20.15(1) lunix-coord=7.85(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=1 (noisy host: 1)
