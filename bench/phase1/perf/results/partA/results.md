# M16 Part A (2026-11-18T12:48:03Z, kernel 7.3, 16 cpus): chunks 65536 16384, queues 4 8, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 18000 s
- load average at the start: 0.00 0.68 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## references
- null_blk reference: raw/fio-nullblk-ref.json
- journal file system raw limits: raw/fio-journal-raw.json

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569108 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/r4k/rep1: 241899.113452 IOPS, 0.99 GB/s, p99 29491.2 us, daemon CPU 164.740s (21.97 us/IO), rss 784068 KiB, split lunix-read-1=27.84(1) lunix-read-3=27.81(1) lunix-read-2=27.82(1) lunix-read-0=27.75(1) lunix-q3=26.62(1) lunix-q2=26.69(1) lunix-coord=0.07(1) 
- partA/c64k/q4/r4k/rep2: 263295.760565 IOPS, 1.08 GB/s, p99 27656.192 us, daemon CPU 175.020s (21.44 us/IO), rss 783556 KiB, split lunix-read-1=29.52(1) lunix-read-3=29.49(1) lunix-read-2=29.48(1) lunix-read-0=29.48(1) lunix-q3=28.41(1) lunix-q2=28.49(1) 
- partA/c64k/q4/r4k/rep3: 262616.617784 IOPS, 1.08 GB/s, p99 28442.624 us, daemon CPU 174.870s (22.20 us/IO), rss 783684 KiB, split lunix-read-1=29.51(1) lunix-read-3=29.48(1) lunix-read-2=29.47(1) lunix-read-0=29.46(1) lunix-q3=28.30(1) lunix-q2=28.42(1) lunix-coord=0.07(1) 
- partA/c64k/q4/w4k/rep1: ? IOPS, 0.00 GB/s, p99 ? us, daemon CPU 17.990s (? us/IO), rss 685156 KiB, split lunix-q3=0.57(1) lunix-q2=0.59(1) lunix-coord=2.99(1) 
- partA/c64k/q4/w4k/rep2: 3762.000636 IOPS, 0.02 GB/s, p99 17112760.32 us, daemon CPU 13.920s (40.22 us/IO), rss 707480 KiB, split lunix-q3=0.56(1) lunix-q2=0.56(1) lunix-coord=2.00(1) 
- partA/c64k/q4/w4k/rep3: ? IOPS, 0.00 GB/s, p99 ? us, daemon CPU 17.030s (? us/IO), rss 712732 KiB, split lunix-q3=0.58(1) lunix-q2=0.59(1) lunix-coord=6.85(9) 
- partA/c64k/q4/seqw/rep1: 29.94012 IOPS, 0.04 GB/s, p99 14294188.032 us, daemon CPU 3.220s (2150.96 us/IO), rss 743188 KiB, split lunix-q3=0.17(1) lunix-q2=0.16(1) lunix-coord=0.48(1) 
- partA/c64k/q4/seqw/rep2: 27.487108 IOPS, 0.04 GB/s, p99 13757317.12 us, daemon CPU 2.550s (2319.27 us/IO), rss 744160 KiB, split lunix-q3=0.14(1) lunix-q2=0.15(1) lunix-coord=0.51(9) 
- partA/c64k/q4/seqw/rep3: 33.251991 IOPS, 0.04 GB/s, p99 14294188.032 us, daemon CPU 2.510s (1797.24 us/IO), rss 744712 KiB, split lunix-q3=0.16(1) lunix-q2=0.15(1) lunix-coord=0.27(9) 
- partA/c64k/q4/seqr/rep1: 7780.090588 IOPS, 8.17 GB/s, p99 50069.504 us, daemon CPU 141.580s (587.02 us/IO), rss 899840 KiB, split lunix-q3=29.09(1) lunix-q2=29.09(1) lunix-read-1=20.72(1) lunix-read-2=20.71(1) lunix-read-3=20.68(1) lunix-read-0=20.70(1) lunix-coord=0.49(1) 
- partA/c64k/q4/seqr/rep2: 8123.78859 IOPS, 8.53 GB/s, p99 45350.912 us, daemon CPU 141.990s (582.61 us/IO), rss 899840 KiB, split lunix-q3=29.72(1) lunix-q2=29.73(1) lunix-read-1=20.61(1) lunix-read-2=20.63(1) lunix-read-3=20.60(1) lunix-read-0=20.62(1) 
- partA/c64k/q4/seqr/rep3: 8021.714514 IOPS, 8.42 GB/s, p99 45350.912 us, daemon CPU 141.070s (586.20 us/IO), rss 898816 KiB, split lunix-q3=29.71(1) lunix-q2=29.73(1) lunix-read-1=20.26(1) lunix-read-2=20.26(1) lunix-read-3=20.27(1) lunix-read-0=20.29(1) lunix-coord=0.45(1) 
- PASS detach for verify_reads
- PASS volume.json verify_reads = true
- partA/c64k/q4/r4k-verifyreads/rep1: 184703.859228 IOPS, 0.76 GB/s, p99 41156.608 us, daemon CPU 159.750s (28.83 us/IO), rss 788964 KiB, split lunix-read-1=29.13(1) lunix-read-0=29.12(1) lunix-read-3=29.12(1) lunix-read-2=29.11(1) lunix-q2=21.63(1) lunix-q3=21.51(1) 
- partA/c64k/q4/seqr-verifyreads/rep1: 7695.7405 IOPS, 8.08 GB/s, p99 54788.096 us, daemon CPU 161.560s (699.78 us/IO), rss 830792 KiB, split lunix-read-2=25.57(1) lunix-read-1=25.56(1) lunix-read-0=25.54(1) lunix-read-3=25.52(1) lunix-q2=29.24(1) lunix-q3=29.24(1) lunix-coord=0.80(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 64 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1154064 KiB
- PASS write the 4 GiB working set
- partA/c64k/q8/r4k/rep1: 236317.847025 IOPS, 0.97 GB/s, p99 21102.592 us, daemon CPU 187.460s (26.44 us/IO), rss 1375516 KiB, split lunix-read-3=27.85(1) lunix-read-2=27.85(1) lunix-read-0=27.83(1) lunix-read-1=27.81(1) lunix-q5=18.83(1) lunix-q4=19.08(1) lunix-q6=18.96(1) lunix-q7=17.68(1) lunix-coord=1.40(1) 
- partA/c64k/q8/r4k/rep2: 260130.278287 IOPS, 1.07 GB/s, p99 20054.016 us, daemon CPU 198.020s (25.37 us/IO), rss 1377500 KiB, split lunix-read-2=29.70(1) lunix-read-3=29.67(1) lunix-read-0=29.69(1) lunix-read-1=29.68(1) lunix-q4=19.93(1) lunix-q5=19.63(1) lunix-q6=19.83(1) lunix-q7=19.59(1) lunix-coord=0.10(1) 
- partA/c64k/q8/r4k/rep3: 259806.265622 IOPS, 1.06 GB/s, p99 20578.304 us, daemon CPU 197.890s (24.57 us/IO), rss 1377500 KiB, split lunix-read-2=29.70(1) lunix-read-3=29.69(1) lunix-read-0=29.69(1) lunix-read-1=29.68(1) lunix-q4=19.95(1) lunix-q5=19.71(1) lunix-q6=19.62(1) lunix-q7=19.69(1) 
- partA/c64k/q8/w4k/rep1: 2072.699642 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 15.520s (84.13 us/IO), rss 1243752 KiB, split lunix-q4=0.23(1) lunix-q5=0.22(1) lunix-q6=0.22(1) lunix-q7=0.21(1) lunix-coord=2.38(1) 
- partA/c64k/q8/w4k/rep2: 2425.543312 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 15.080s (64.76 us/IO), rss 1259456 KiB, split lunix-q4=0.22(1) lunix-q5=0.22(1) lunix-q6=0.22(1) lunix-q7=0.21(1) lunix-coord=2.66(1) 
- partA/c64k/q8/w4k/rep3: 1700.892137 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 16.180s (88.90 us/IO), rss 1281808 KiB, split lunix-q4=0.22(1) lunix-q5=0.21(1) lunix-q6=0.23(1) lunix-q7=0.21(1) lunix-coord=2.66(9) 
- partA/c64k/q8/seqw/rep1: 30.538051 IOPS, 0.04 GB/s, p99 14428405.76 us, daemon CPU 3.200s (2183.07 us/IO), rss 1352240 KiB, split lunix-q4=0.09(1) lunix-q5=0.09(1) lunix-q6=0.10(1) lunix-q7=0.09(1) lunix-coord=0.48(1) 
- partA/c64k/q8/seqw/rep2: 30.527693 IOPS, 0.04 GB/s, p99 17112760.32 us, daemon CPU 3.070s (2139.67 us/IO), rss 1388408 KiB, split lunix-q4=0.10(1) lunix-q5=0.08(1) lunix-q6=0.10(1) lunix-q7=0.10(1) lunix-coord=0.51(9) 
- partA/c64k/q8/seqw/rep3: 30.248343 IOPS, 0.04 GB/s, p99 17112760.32 us, daemon CPU 2.750s (2164.62 us/IO), rss 1333104 KiB, split lunix-q4=0.09(1) lunix-q5=0.07(1) lunix-q6=0.08(1) lunix-q7=0.09(1) lunix-coord=0.36(9) 
- partA/c64k/q8/seqr/rep1: 6461.328541 IOPS, 6.78 GB/s, p99 96993.28 us, daemon CPU 193.600s (966.54 us/IO), rss 1518796 KiB, split lunix-read-0=22.45(1) lunix-read-2=22.36(1) lunix-read-3=22.37(1) lunix-read-1=22.27(1) lunix-q4=26.53(1) lunix-q6=26.57(1) lunix-q5=26.12(1) lunix-q7=24.50(1) lunix-coord=0.43(1) 
- partA/c64k/q8/seqr/rep2: 7570.810019 IOPS, 7.95 GB/s, p99 74973.184 us, daemon CPU 203.700s (896.87 us/IO), rss 1485948 KiB, split lunix-read-0=24.49(1) lunix-read-2=24.48(1) lunix-read-3=24.46(1) lunix-read-1=24.46(1) lunix-q6=29.79(1) lunix-q4=28.30(1) lunix-q7=29.79(1) lunix-q5=17.79(1) 
- partA/c64k/q8/seqr/rep3: 7192.234691 IOPS, 7.55 GB/s, p99 79167.488 us, daemon CPU 204.320s (916.40 us/IO), rss 1497876 KiB, split lunix-read-0=23.69(1) lunix-read-3=23.76(1) lunix-read-2=23.69(1) lunix-read-1=23.75(1) lunix-q6=29.71(1) lunix-q4=27.30(1) lunix-q7=21.13(1) lunix-q5=29.16(1) lunix-coord=1.98(1) 
- PASS detach for verify_reads
- PASS volume.json verify_reads = true
- partA/c64k/q8/r4k-verifyreads/rep1: 178539.458775 IOPS, 0.73 GB/s, p99 27656.192 us, daemon CPU 178.790s (33.38 us/IO), rss 1457016 KiB, split lunix-read-3=29.03(1) lunix-read-1=29.02(1) lunix-read-0=29.01(1) lunix-read-2=28.99(1) lunix-q5=15.73(1) lunix-q6=15.71(1) lunix-q7=15.65(1) lunix-q4=15.49(1) 
- partA/c64k/q8/seqr-verifyreads/rep1: 6265.461687 IOPS, 6.58 GB/s, p99 91750.4 us, daemon CPU 210.110s (1117.82 us/IO), rss 1640784 KiB, split lunix-read-1=26.29(1) lunix-read-3=26.22(1) lunix-read-2=26.15(1) lunix-read-0=26.04(1) lunix-q7=29.17(1) lunix-q4=29.29(1) lunix-q5=26.93(1) lunix-q6=18.66(1) lunix-coord=1.19(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569024 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/r4k/rep1: 212124.775045 IOPS, 0.87 GB/s, p99 31326.208 us, daemon CPU 112.200s (17.06 us/IO), rss 812888 KiB, split lunix-q2=22.46(1) lunix-q3=22.41(1) lunix-read-0=16.80(1) lunix-read-1=16.77(1) lunix-read-3=16.71(1) lunix-read-2=16.70(1) lunix-coord=0.28(1) 
- partA/c16k/q4/r4k/rep2: 235546.957275 IOPS, 0.96 GB/s, p99 28704.768 us, daemon CPU 111.070s (15.72 us/IO), rss 812888 KiB, split lunix-q2=23.14(1) lunix-q3=22.96(1) lunix-read-0=16.25(1) lunix-read-1=16.19(1) lunix-read-3=16.24(1) lunix-read-2=16.22(1) 
- partA/c16k/q4/r4k/rep3: 234711.348109 IOPS, 0.96 GB/s, p99 27918.336 us, daemon CPU 111.260s (15.80 us/IO), rss 812988 KiB, split lunix-q2=23.24(1) lunix-q3=23.07(1) lunix-read-0=16.18(1) lunix-read-3=16.14(1) lunix-read-1=16.13(1) lunix-read-2=16.14(1) lunix-coord=0.28(1) 
- partA/c16k/q4/w4k/rep1: 2445.214317 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 12.740s (46.94 us/IO), rss 674316 KiB, split lunix-q2=0.59(1) lunix-q3=0.57(1) lunix-coord=2.85(1) 
- partA/c16k/q4/w4k/rep2: 2539.484454 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 12.780s (43.01 us/IO), rss 740940 KiB, split lunix-q2=0.59(1) lunix-q3=0.59(1) lunix-coord=3.34(1) 
- partA/c16k/q4/w4k/rep3: 1736.639877 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 10.920s (62.26 us/IO), rss 789196 KiB, split lunix-q2=0.49(1) lunix-q3=0.49(1) lunix-coord=2.26(1) 
- partA/c16k/q4/seqw/rep1: 29.39423 IOPS, 0.04 GB/s, p99 15367929.856 us, daemon CPU 4.420s (3132.70 us/IO), rss 823780 KiB, split lunix-q2=0.15(1) lunix-q3=0.16(1) lunix-coord=0.90(1) 
- partA/c16k/q4/seqw/rep2: 28.202022 IOPS, 0.04 GB/s, p99 15770583.04 us, daemon CPU 3.900s (2942.30 us/IO), rss 827880 KiB, split lunix-q2=0.15(1) lunix-q3=0.13(1) lunix-coord=0.81(1) 
- partA/c16k/q4/seqw/rep3: 28.531173 IOPS, 0.04 GB/s, p99 14428405.76 us, daemon CPU 3.790s (2710.96 us/IO), rss 828428 KiB, split lunix-q2=0.16(1) lunix-q3=0.15(1) lunix-coord=0.86(9) 
- partA/c16k/q4/seqr/rep1: 3343.144102 IOPS, 3.51 GB/s, p99 103284.736 us, daemon CPU 111.760s (1114.32 us/IO), rss 998124 KiB, split lunix-q2=15.89(1) lunix-q3=15.98(1) lunix-read-2=20.07(1) lunix-read-0=19.91(1) lunix-read-3=19.95(1) lunix-read-1=19.88(1) 
- partA/c16k/q4/seqr/rep2: 3688.379052 IOPS, 3.88 GB/s, p99 73924.608 us, daemon CPU 112.650s (1018.06 us/IO), rss 1000240 KiB, split lunix-q2=15.94(1) lunix-q3=15.91(1) lunix-read-2=19.94(1) lunix-read-0=19.96(1) lunix-read-3=19.89(1) lunix-read-1=19.91(1) lunix-coord=1.05(1) 
- partA/c16k/q4/seqr/rep3: 3794.519637 IOPS, 3.99 GB/s, p99 70778.88 us, daemon CPU 116.600s (991.24 us/IO), rss 1003464 KiB, split lunix-q2=16.41(1) lunix-q3=16.15(1) lunix-read-2=20.24(1) lunix-read-0=20.23(1) lunix-read-3=20.24(1) lunix-read-1=20.21(1) lunix-coord=3.03(1) 
- PASS detach for verify_reads
- PASS volume.json verify_reads = true
- partA/c16k/q4/r4k-verifyreads/rep1: 205188.568572 IOPS, 0.84 GB/s, p99 32374.784 us, daemon CPU 119.630s (18.81 us/IO), rss 811036 KiB, split lunix-q2=22.09(1) lunix-q3=22.01(1) lunix-read-0=18.88(1) lunix-read-2=18.87(1) lunix-read-1=18.85(1) lunix-read-3=18.83(1) 
- partA/c16k/q4/seqr-verifyreads/rep1: 3086.93195 IOPS, 3.25 GB/s, p99 110624.768 us, daemon CPU 114.320s (1234.45 us/IO), rss 935624 KiB, split lunix-read-2=21.14(1) lunix-read-0=21.11(1) lunix-read-1=21.17(1) lunix-read-3=21.13(1) lunix-q2=14.51(1) lunix-q3=14.33(1) lunix-coord=0.87(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 16 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1196108 KiB
- PASS write the 4 GiB working set
- partA/c16k/q8/r4k/rep1: 268610.272306 IOPS, 1.10 GB/s, p99 20578.304 us, daemon CPU 159.080s (19.74 us/IO), rss 1390072 KiB, split lunix-read-2=20.34(1) lunix-read-3=20.31(1) lunix-read-1=20.28(1) lunix-read-0=20.27(1) lunix-q5=19.32(1) lunix-q6=19.44(1) lunix-q7=19.42(1) lunix-q4=19.32(1) lunix-coord=0.22(1) 
- partA/c16k/q8/r4k/rep2: 305177.776296 IOPS, 1.25 GB/s, p99 18481.152 us, daemon CPU 162.190s (17.72 us/IO), rss 1394296 KiB, split lunix-read-2=20.86(1) lunix-read-0=20.89(1) lunix-read-1=20.84(1) lunix-read-3=20.80(1) lunix-q5=19.60(1) lunix-q6=19.70(1) lunix-q7=19.67(1) lunix-q4=19.66(1) 
- partA/c16k/q8/r4k/rep3: 305460.205306 IOPS, 1.25 GB/s, p99 18219.008 us, daemon CPU 162.680s (17.18 us/IO), rss 1395988 KiB, split lunix-read-2=20.78(1) lunix-read-0=20.78(1) lunix-read-1=20.72(1) lunix-read-3=20.71(1) lunix-q5=19.65(1) lunix-q6=19.74(1) lunix-q7=19.68(1) lunix-q4=19.72(1) lunix-coord=0.75(1) 
- partA/c16k/q8/w4k/rep1: 2889.65057 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 10.920s (41.53 us/IO), rss 1300604 KiB, split lunix-q5=0.25(1) lunix-q6=0.25(1) lunix-q7=0.24(1) lunix-q4=0.24(1) lunix-coord=1.94(1) 
- partA/c16k/q8/w4k/rep2: 2596.351539 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 13.050s (46.11 us/IO), rss 1324388 KiB, split lunix-q5=0.34(1) lunix-q6=0.33(1) lunix-q7=0.32(1) lunix-q4=0.33(1) lunix-coord=2.97(1) 
- partA/c16k/q8/w4k/rep3: 3041.239576 IOPS, 0.01 GB/s, p99 17112760.32 us, daemon CPU 11.260s (40.69 us/IO), rss 1374140 KiB, split lunix-q5=0.26(1) lunix-q6=0.27(1) lunix-q7=0.26(1) lunix-q4=0.28(1) lunix-coord=2.89(1) 
- partA/c16k/q8/seqw/rep1: 28.077011 IOPS, 0.04 GB/s, p99 17112760.32 us, daemon CPU 4.830s (3373.08 us/IO), rss 1414764 KiB, split lunix-q5=0.10(1) lunix-q6=0.09(1) lunix-q7=0.09(1) lunix-q4=0.11(1) lunix-coord=0.94(9) 
- partA/c16k/q8/seqw/rep2: 26.415094 IOPS, 0.03 GB/s, p99 13354663.936 us, daemon CPU 3.960s (3331.43 us/IO), rss 1397492 KiB, split lunix-q5=0.07(1) lunix-q6=0.08(1) lunix-q7=0.08(1) lunix-q4=0.07(1) lunix-coord=0.80(1) 
- partA/c16k/q8/seqw/rep3: 28.285277 IOPS, 0.04 GB/s, p99 17112760.32 us, daemon CPU 4.820s (3215.22 us/IO), rss 1429488 KiB, split lunix-q5=0.09(1) lunix-q6=0.09(1) lunix-q7=0.10(1) lunix-q4=0.09(1) lunix-coord=1.11(1) 
- partA/c16k/q8/seqr/rep1: 3424.324055 IOPS, 3.60 GB/s, p99 106430.464 us, daemon CPU 116.400s (1096.52 us/IO), rss 1598528 KiB, split lunix-read-2=20.29(1) lunix-read-0=20.27(1) lunix-read-1=20.24(1) lunix-read-3=20.20(1) lunix-q4=12.28(1) lunix-q5=9.12(1) lunix-q6=7.06(1) lunix-q7=5.95(1) lunix-coord=0.88(1) 
- partA/c16k/q8/seqr/rep2: 3791.234371 IOPS, 3.98 GB/s, p99 69730.304 us, daemon CPU 116.580s (1025.00 us/IO), rss 1608776 KiB, split lunix-read-2=20.34(1) lunix-read-0=20.40(1) lunix-read-1=20.41(1) lunix-read-3=20.46(1) lunix-q4=13.45(1) lunix-q5=9.69(1) lunix-q6=11.11(1) lunix-q7=0.06(1) lunix-coord=0.59(1) 
- partA/c16k/q8/seqr/rep3: 3860.995644 IOPS, 4.06 GB/s, p99 70778.88 us, daemon CPU 117.350s (980.44 us/IO), rss 1587272 KiB, split lunix-read-2=20.64(1) lunix-read-0=20.59(1) lunix-read-1=20.62(1) lunix-read-3=20.61(1) lunix-q4=13.75(1) lunix-q5=11.91(1) lunix-q6=4.69(1) lunix-q7=4.45(1) 
- PASS detach for verify_reads
- PASS volume.json verify_reads = true
- partA/c16k/q8/r4k-verifyreads/rep1: 273048.26184 IOPS, 1.12 GB/s, p99 20316.16 us, daemon CPU 176.860s (21.59 us/IO), rss 1610000 KiB, split lunix-read-0=23.63(1) lunix-read-2=23.59(1) lunix-read-1=23.61(1) lunix-read-3=23.58(1) lunix-q6=20.67(1) lunix-q7=20.60(1) lunix-q4=20.59(1) lunix-q5=20.41(1) 
- partA/c16k/q8/seqr-verifyreads/rep1: 3298.95913 IOPS, 3.47 GB/s, p99 104333.312 us, daemon CPU 120.880s (1221.40 us/IO), rss 1580480 KiB, split lunix-read-1=21.94(1) lunix-read-3=21.91(1) lunix-read-2=21.90(1) lunix-read-0=21.87(1) lunix-q5=12.55(1) lunix-q6=11.08(1) lunix-q7=4.70(1) lunix-q4=3.60(1) lunix-coord=1.28(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

overall FAIL=1 (noisy host: 0)
