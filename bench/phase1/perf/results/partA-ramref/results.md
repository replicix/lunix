# M16 Part A (2026-11-19T00:15:26Z, kernel 7.3, 16 cpus): chunks 65536 16384, queues 4 8, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 6000 s
- load average at the start: 0.06 0.76 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k
- PASS read_cache_max = 4294967296

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568840 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/r4k/rep1: 380367.296604 IOPS, 1.56 GB/s, p99 18743.296 us, daemon CPU 60.050s (5.26 us/IO), rss 2713572 KiB, split lunix-q2=29.99(1) lunix-q3=29.99(1) 
- partA/c64k/q4/r4k/rep2: 380381.282496 IOPS, 1.56 GB/s, p99 18743.296 us, daemon CPU 60.040s (5.09 us/IO), rss 2713572 KiB, split lunix-q2=29.98(1) lunix-q3=29.99(1) 
- partA/c64k/q4/r4k/rep3: 380208.979102 IOPS, 1.56 GB/s, p99 18481.152 us, daemon CPU 60.060s (5.27 us/IO), rss 2713572 KiB, split lunix-q2=29.99(1) lunix-q3=30.00(1) 
- partA/c64k/q4/w4k/rep1: 5167.936418 IOPS, 0.02 GB/s, p99 1702887.424 us, daemon CPU 29.550s (57.76 us/IO), rss 633092 KiB, split lunix-q2=1.20(1) lunix-q3=1.19(1) lunix-space=10.72(1) lunix-coord=3.34(1) lunixd-trim=0.10(1) 
- partA/c64k/q4/w4k/rep2: 9526.068675 IOPS, 0.04 GB/s, p99 750780.416 us, daemon CPU 45.350s (51.19 us/IO), rss 649872 KiB, split lunix-q2=2.18(1) lunix-q3=2.18(1) lunix-space=10.66(1) lunix-coord=6.24(1) 
- partA/c64k/q4/w4k/rep3: 6063.451985 IOPS, 0.02 GB/s, p99 641728.512 us, daemon CPU 27.160s (73.43 us/IO), rss 672628 KiB, split lunix-q2=1.34(1) lunix-q3=1.33(1) lunix-space=6.43(1) lunix-coord=3.64(1) 
- partA/c64k/q4/seqw/rep1: 366.74267 IOPS, 0.39 GB/s, p99 14159970.304 us, daemon CPU 28.960s (1925.99 us/IO), rss 826344 KiB, split lunix-q2=2.62(1) lunix-q3=2.70(1) lunix-space=5.19(1) lunix-coord=4.52(1) iou-wrk-2682846=0.13(1) iou-wrk-2682845=0.12(1) 
- partA/c64k/q4/seqw/rep2: 302.153276 IOPS, 0.33 GB/s, p99 17112760.32 us, daemon CPU 23.060s (2461.90 us/IO), rss 837120 KiB, split lunix-q3=2.10(1) lunix-q2=1.63(1) lunix-space=3.81(1) lunix-coord=3.58(1) iou-wrk-2682845=0.09(1) iou-wrk-2682846=0.10(2) 
- partA/c64k/q4/seqw/rep3: 375.798027 IOPS, 0.40 GB/s, p99 12280922.112 us, daemon CPU 29.150s (1846.86 us/IO), rss 851304 KiB, split lunix-q3=2.69(1) lunix-q2=2.11(1) lunix-space=4.83(1) lunix-coord=4.57(1) iou-wrk-2682845=0.12(1) iou-wrk-2682846=0.13(1) 
- partA/c64k/q4/seqr/rep1: 8914.773863 IOPS, 9.36 GB/s, p99 30539.776 us, daemon CPU 60.090s (217.44 us/IO), rss 2970932 KiB, split lunix-q3=30.01(1) lunix-q2=29.98(1) lunix-coord=0.05(1) 
- partA/c64k/q4/seqr/rep2: 8873.17658 IOPS, 9.31 GB/s, p99 30015.488 us, daemon CPU 60.050s (225.59 us/IO), rss 2970932 KiB, split lunix-q3=30.00(1) lunix-q2=29.99(1) 
- partA/c64k/q4/seqr/rep3: 8849.290426 IOPS, 9.29 GB/s, p99 30801.92 us, daemon CPU 60.540s (220.68 us/IO), rss 2975940 KiB, split lunix-q3=29.98(1) lunix-q2=29.98(1) lunix-coord=0.52(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 64 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1148244 KiB
- PASS write the 2 GiB working set
- partA/c64k/q8/r4k/rep1: 894253.716419 IOPS, 3.66 GB/s, p99 4423.68 us, daemon CPU 110.900s (4.13 us/IO), rss 3278736 KiB, split lunix-q6=27.69(1) lunix-q4=27.78(1) lunix-q5=27.70(1) lunix-q7=27.62(1) 
- partA/c64k/q8/r4k/rep2: 883388.474102 IOPS, 3.62 GB/s, p99 4554.752 us, daemon CPU 109.830s (4.14 us/IO), rss 3278744 KiB, split lunix-q6=27.40(1) lunix-q4=27.54(1) lunix-q5=27.41(1) lunix-q7=27.33(1) 
- partA/c64k/q8/r4k/rep3: 908740.608646 IOPS, 3.72 GB/s, p99 4292.608 us, daemon CPU 111.090s (3.94 us/IO), rss 3278744 KiB, split lunix-q6=27.73(1) lunix-q4=27.84(1) lunix-q5=27.74(1) lunix-q7=27.63(1) 
- partA/c64k/q8/w4k/rep1: 5483.582315 IOPS, 0.02 GB/s, p99 692060.16 us, daemon CPU 35.460s (68.07 us/IO), rss 1203088 KiB, split lunix-q6=0.57(1) lunix-q4=0.56(1) lunix-q5=0.57(1) lunix-q7=0.57(1) lunix-space=12.13(1) lunix-coord=4.03(1) lunixd-trim=0.08(1) 
- partA/c64k/q8/w4k/rep2: 5712.160488 IOPS, 0.02 GB/s, p99 2600468.48 us, daemon CPU 38.530s (68.83 us/IO), rss 1219432 KiB, split lunix-q6=0.62(1) lunix-q4=0.62(1) lunix-q5=0.61(1) lunix-q7=0.62(1) lunix-space=12.82(1) lunix-coord=4.47(1) 
- partA/c64k/q8/w4k/rep3: 6032.009854 IOPS, 0.02 GB/s, p99 675282.944 us, daemon CPU 38.720s (68.29 us/IO), rss 1236208 KiB, split lunix-q6=0.63(1) lunix-q4=0.62(1) lunix-q5=0.61(1) lunix-q7=0.63(1) lunix-space=12.83(1) lunix-coord=4.49(1) 
- partA/c64k/q8/seqw/rep1: 331.475102 IOPS, 0.36 GB/s, p99 17112760.32 us, daemon CPU 30.660s (1594.75 us/IO), rss 1297312 KiB, split lunix-q6=1.58(1) lunix-q4=1.57(1) lunix-q5=1.57(1) lunix-q7=1.58(1) lunix-space=6.11(1) lunix-coord=4.02(1) iou-wrk-2698190=0.13(1) iou-wrk-2698191=0.13(1) iou-wrk-2698189=0.12(1) iou-wrk-2698188=0.12(1) 
- partA/c64k/q8/seqw/rep2: 283.192861 IOPS, 0.31 GB/s, p99 17112760.32 us, daemon CPU 24.960s (2937.93 us/IO), rss 1321208 KiB, split lunix-q6=1.36(1) lunix-q4=1.32(1) lunix-q5=1.34(1) lunix-q7=1.36(1) lunix-space=4.22(1) lunix-coord=3.37(1) iou-wrk-2698190=0.10(1) iou-wrk-2698191=0.11(1) iou-wrk-2698189=0.10(1) iou-wrk-2698188=0.10(1) 
- partA/c64k/q8/seqw/rep3: 326.862374 IOPS, 0.35 GB/s, p99 12280922.112 us, daemon CPU 30.250s (2257.23 us/IO), rss 1338496 KiB, split lunix-q6=1.59(1) lunix-q4=1.54(1) lunix-q5=1.57(1) lunix-q7=1.57(1) lunix-space=5.69(1) lunix-coord=4.01(1) iou-wrk-2698190=0.12(1) iou-wrk-2698191=0.13(1) iou-wrk-2698189=0.12(1) iou-wrk-2698188=0.11(1) 
- partA/c64k/q8/seqr/rep1: 10907.767637 IOPS, 11.45 GB/s, p99 37486.592 us, daemon CPU 100.330s (306.60 us/IO), rss 3460000 KiB, split lunix-q6=30.00(1) lunix-q5=29.98(1) lunix-q7=29.91(1) lunix-q4=10.28(1) lunix-coord=0.07(1) 
- partA/c64k/q8/seqr/rep2: 10738.997168 IOPS, 11.27 GB/s, p99 36438.016 us, daemon CPU 96.100s (298.29 us/IO), rss 3460004 KiB, split lunix-q6=29.99(1) lunix-q7=28.71(1) lunix-q4=29.98(1) lunix-q5=7.31(1) 
- partA/c64k/q8/seqr/rep3: 11368.34216 IOPS, 11.93 GB/s, p99 36962.304 us, daemon CPU 118.560s (347.63 us/IO), rss 3460012 KiB, split lunix-q6=29.98(1) lunix-q7=28.40(1) lunix-q4=29.41(1) lunix-q5=29.98(1) lunix-coord=0.67(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

## chunk 16 KiB
- PASS create pa16k
- PASS read_cache_max = 4294967296

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568848 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/r4k/rep1: 368161.017232 IOPS, 1.51 GB/s, p99 19267.584 us, daemon CPU 60.080s (5.26 us/IO), rss 2724848 KiB, split lunix-q2=29.77(1) lunix-q3=29.78(1) lunix-read-2=0.09(1) lunix-read-1=0.10(1) lunix-read-3=0.10(1) lunix-read-0=0.10(1) lunix-coord=0.11(1) 
- partA/c16k/q4/r4k/rep2: 371383.482202 IOPS, 1.52 GB/s, p99 18743.296 us, daemon CPU 60.030s (5.39 us/IO), rss 2724848 KiB, split lunix-q2=29.99(1) lunix-q3=29.99(1) 
- partA/c16k/q4/r4k/rep3: 370991.03423 IOPS, 1.52 GB/s, p99 19005.44 us, daemon CPU 60.130s (5.23 us/IO), rss 2724848 KiB, split lunix-q2=30.00(1) lunix-q3=29.97(1) lunix-coord=0.09(1) 
- partA/c16k/q4/w4k/rep1: 12809.52381 IOPS, 0.05 GB/s, p99 834666.496 us, daemon CPU 66.310s (69.95 us/IO), rss 755164 KiB, split lunix-q2=3.09(1) lunix-q3=3.07(1) lunix-space=31.54(1) lunix-coord=7.68(1) lunixd-trim=0.11(1) 
- partA/c16k/q4/w4k/rep2: 11922.375663 IOPS, 0.05 GB/s, p99 968884.224 us, daemon CPU 61.990s (73.23 us/IO), rss 838452 KiB, split lunix-q2=2.88(1) lunix-q3=2.88(1) lunix-space=31.82(1) lunix-coord=6.67(1) 
- partA/c16k/q4/w4k/rep3: 16153.904345 IOPS, 0.07 GB/s, p99 985661.44 us, daemon CPU 87.760s (61.04 us/IO), rss 996828 KiB, split lunix-q2=4.91(1) lunix-q3=4.89(1) lunix-space=34.80(1) lunix-coord=11.59(1) 
- partA/c16k/q4/seqw/rep1: 301.066475 IOPS, 0.32 GB/s, p99 14696841.216 us, daemon CPU 38.550s (4130.48 us/IO), rss 965932 KiB, split lunix-q2=1.91(1) lunix-q3=1.99(1) lunix-space=11.94(1) lunix-coord=6.26(1) iou-wrk-2714882=0.10(1) iou-wrk-2714883=0.09(1) 
- partA/c16k/q4/seqw/rep2: 293.046412 IOPS, 0.32 GB/s, p99 10536091.648 us, daemon CPU 37.790s (4159.86 us/IO), rss 979616 KiB, split lunix-space=10.99(1) lunix-q2=1.90(1) lunix-q3=1.97(1) lunix-coord=6.09(1) iou-wrk-2714882=0.09(1) iou-wrk-2714883=0.09(1) 
- partA/c16k/q4/seqw/rep3: 316.091763 IOPS, 0.34 GB/s, p99 10401873.92 us, daemon CPU 41.940s (4280.10 us/IO), rss 982292 KiB, split lunix-space=12.82(1) lunix-q2=1.89(1) lunix-q3=2.09(1) lunix-coord=7.06(9) iou-wrk-2714882=0.11(1) iou-wrk-2714883=0.11(1) 
- partA/c16k/q4/seqr/rep1: 7712.263523 IOPS, 8.10 GB/s, p99 35913.728 us, daemon CPU 60.200s (260.19 us/IO), rss 3105576 KiB, split lunix-q2=29.99(1) lunix-q3=30.01(1) lunix-coord=0.16(1) 
- partA/c16k/q4/seqr/rep2: 7683.162382 IOPS, 8.07 GB/s, p99 35913.728 us, daemon CPU 61.140s (265.26 us/IO), rss 3105576 KiB, split lunix-q2=29.99(1) lunix-q3=30.00(1) lunix-coord=1.11(1) 
- partA/c16k/q4/seqr/rep3: 7680.29975 IOPS, 8.06 GB/s, p99 34340.864 us, daemon CPU 60.200s (261.27 us/IO), rss 3105576 KiB, split lunix-q2=30.00(1) lunix-q3=30.00(1) lunix-coord=0.16(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 16 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1208192 KiB
- PASS write the 2 GiB working set
- partA/c16k/q8/r4k/rep1: 770461.169255 IOPS, 3.16 GB/s, p99 5341.184 us, daemon CPU 104.850s (4.39 us/IO), rss 3354576 KiB, split lunix-q6=26.05(1) lunix-q4=26.18(1) lunix-q7=26.07(1) lunix-q5=26.04(1) lunix-read-1=0.10(1) lunix-read-2=0.11(1) lunix-read-0=0.10(1) lunix-read-3=0.11(1) 
- partA/c16k/q8/r4k/rep2: 773248.450103 IOPS, 3.17 GB/s, p99 5341.184 us, daemon CPU 105.080s (4.53 us/IO), rss 3358612 KiB, split lunix-q6=26.25(1) lunix-q4=26.29(1) lunix-q7=26.18(1) lunix-q5=26.11(1) lunix-coord=0.14(1) 
- partA/c16k/q8/r4k/rep3: 773292.080528 IOPS, 3.17 GB/s, p99 5341.184 us, daemon CPU 105.240s (4.39 us/IO), rss 3358612 KiB, split lunix-q6=26.30(1) lunix-q4=26.29(1) lunix-q7=26.25(1) lunix-q5=26.16(1) lunix-coord=0.12(1) 
- partA/c16k/q8/w4k/rep1: 12354.149843 IOPS, 0.05 GB/s, p99 1283457.024 us, daemon CPU 74.780s (90.34 us/IO), rss 1440216 KiB, split lunix-q6=1.28(1) lunix-q4=1.27(1) lunix-q7=1.28(1) lunix-q5=1.27(1) lunix-space=38.63(1) lunix-coord=7.06(1) lunixd-trim=0.11(1) 
- partA/c16k/q8/w4k/rep2: 14759.417931 IOPS, 0.06 GB/s, p99 1233125.376 us, daemon CPU 75.750s (77.76 us/IO), rss 1554960 KiB, split lunix-q6=1.45(1) lunix-q4=1.44(1) lunix-q7=1.44(1) lunix-q5=1.42(1) lunix-space=36.15(1) lunix-coord=8.14(1) 
- partA/c16k/q8/w4k/rep3: 12980.069106 IOPS, 0.05 GB/s, p99 1115684.864 us, daemon CPU 69.410s (74.27 us/IO), rss 1593232 KiB, split lunix-space=32.64(1) lunix-q6=1.28(1) lunix-q4=1.28(1) lunix-q7=1.29(1) lunix-q5=1.28(1) lunix-coord=7.63(1) 
- partA/c16k/q8/seqw/rep1: 215.207752 IOPS, 0.23 GB/s, p99 14831058.944 us, daemon CPU 33.970s (5091.85 us/IO), rss 1831148 KiB, split lunix-space=10.00(1) lunix-q6=1.01(1) lunix-q4=1.02(1) lunix-q7=1.03(1) lunix-q5=1.02(1) lunix-coord=4.63(1) iou-wrk-2729749=0.09(1) iou-wrk-2729750=0.09(1) iou-wrk-2729748=0.09(1) iou-wrk-2729747=0.09(1) 
- partA/c16k/q8/seqw/rep2: 262.790849 IOPS, 0.28 GB/s, p99 10670309.376 us, daemon CPU 41.300s (5069.65 us/IO), rss 1891984 KiB, split lunix-space=12.01(1) lunix-q6=1.09(1) lunix-q4=1.18(1) lunix-q7=1.17(1) lunix-q5=1.17(1) lunix-coord=5.31(1) iou-wrk-2729750=0.12(1) iou-wrk-2729749=0.10(1) iou-wrk-2729748=0.10(1) iou-wrk-2729747=0.11(1) 
- partA/c16k/q8/seqw/rep3: 233.650121 IOPS, 0.25 GB/s, p99 11475615.744 us, daemon CPU 36.760s (5075.14 us/IO), rss 1941048 KiB, split lunix-space=10.45(1) lunix-q6=1.07(1) lunix-q4=1.06(1) lunix-q7=1.07(1) lunix-q5=1.06(1) lunix-coord=4.87(1) iou-wrk-2729750=0.10(1) iou-wrk-2729747=0.10(1) iou-wrk-2729749=0.09(1) iou-wrk-2729748=0.09(1) 
- partA/c16k/q8/seqr/rep1: 10472.588596 IOPS, 10.99 GB/s, p99 48496.64 us, daemon CPU 115.980s (369.15 us/IO), rss 3815848 KiB, split lunix-q6=29.80(1) lunix-q4=29.50(1) lunix-q7=29.78(1) lunix-q5=26.61(1) lunix-coord=0.19(1) 
- partA/c16k/q8/seqr/rep2: 10024.108421 IOPS, 10.52 GB/s, p99 48496.64 us, daemon CPU 103.140s (331.91 us/IO), rss 3815848 KiB, split lunix-q6=29.85(1) lunix-q4=29.55(1) lunix-q5=29.83(1) lunix-q7=13.75(1) 
- partA/c16k/q8/seqr/rep3: 9933.626403 IOPS, 10.42 GB/s, p99 46923.776 us, daemon CPU 102.740s (344.75 us/IO), rss 3815852 KiB, split lunix-q6=29.85(1) lunix-q4=29.80(1) lunix-q5=27.93(1) lunix-q7=13.78(1) lunix-coord=1.27(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

overall FAIL=1 (noisy host: 0)
