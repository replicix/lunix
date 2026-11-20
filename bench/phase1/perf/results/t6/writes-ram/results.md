# M16 Part A (2026-11-20T14:08:17Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 1.50 11.33 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 11.33 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569292 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/r4k/rep1: 271778.396214 IOPS, 1.11 GB/s, p99 28180.48 us, daemon CPU 174.800s (19.49 us/IO), rss 1002212 KiB, split lunix-read-2=29.31(1) lunix-read-1=29.31(1) lunix-read-3=29.28(1) lunix-read-0=29.29(1) lunix-q3=28.78(1) lunix-q2=28.63(1) 
- partA/c64k/q4/r4k/rep2: 274763.931476 IOPS, 1.13 GB/s, p99 27131.904 us, daemon CPU 176.230s (21.38 us/IO), rss 1001192 KiB, split lunix-read-1=29.45(1) lunix-read-2=29.44(1) lunix-read-3=29.45(1) lunix-read-0=29.45(1) lunix-q3=29.06(1) lunix-q2=29.16(1) 
- partA/c64k/q4/r4k/rep3: 275182.6029 IOPS, 1.13 GB/s, p99 26869.76 us, daemon CPU 176.220s (20.66 us/IO), rss 1002604 KiB, split lunix-read-1=29.46(1) lunix-read-3=29.44(1) lunix-read-2=29.43(1) lunix-read-0=29.43(1) lunix-q3=29.08(1) lunix-q2=29.18(1) 
- partA/c64k/q4/w4k/rep1: 18059.095045 IOPS, 0.07 GB/s, p99 541065.216 us, daemon CPU 55.950s (50.79 us/IO), rss 1145588 KiB, split lunix-q3=4.07(1) lunix-q2=4.16(1) lunix-coord=10.62(1) iou-wrk-737192=0.50(1) iou-wrk-737191=0.52(1) lunix-space-lad=0.73(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/w4k/rep2: 16064.999544 IOPS, 0.07 GB/s, p99 608174.08 us, daemon CPU 64.250s (51.94 us/IO), rss 1256636 KiB, split lunix-q3=4.95(1) lunix-q2=5.00(1) lunix-coord=13.22(1) lunix-space-lad=1.06(1) iou-wrk-737192=0.52(1) iou-wrk-737191=0.52(1) lunix-space-tim=0.20(1) 
- partA/c64k/q4/w4k/rep3: 25860.479219 IOPS, 0.11 GB/s, p99 438304.768 us, daemon CPU 79.940s (49.86 us/IO), rss 1299124 KiB, split lunix-q3=6.03(1) lunix-q2=6.11(1) lunix-coord=15.54(1) lunix-space-lad=0.87(1) iou-wrk-737192=0.66(1) iou-wrk-737191=0.67(1) lunix-space-tim=0.13(1) 
- partA/c64k/q4/seqw/rep1: 649.928913 IOPS, 0.69 GB/s, p99 2566914.048 us, daemon CPU 43.120s (1251.81 us/IO), rss 1643356 KiB, split lunix-q2=3.70(1) lunix-q3=3.50(1) lunix-coord=10.25(1) iou-wrk-737192=0.91(1) iou-wrk-737191=0.90(1) 
- partA/c64k/q4/seqw/rep2: 608.334139 IOPS, 0.65 GB/s, p99 3976200.192 us, daemon CPU 40.620s (2153.95 us/IO), rss 1616316 KiB, split lunix-q2=3.70(1) lunix-q3=3.59(1) lunix-coord=7.46(1) iou-wrk-737192=0.85(1) iou-wrk-737191=0.85(1) 
- partA/c64k/q4/seqw/rep3: 629.029581 IOPS, 0.67 GB/s, p99 2600468.48 us, daemon CPU 41.030s (2104.11 us/IO), rss 1646572 KiB, split lunix-q2=3.20(1) lunix-q3=3.23(1) lunix-coord=8.03(1) iou-wrk-737192=0.80(1) iou-wrk-737191=0.80(1) 
- partA/c64k/q4/seqr/rep1: 7981.915673 IOPS, 8.38 GB/s, p99 46399.488 us, daemon CPU 142.400s (540.62 us/IO), rss 1735452 KiB, split lunix-q2=29.65(1) lunix-q3=29.65(1) lunix-read-1=20.59(1) lunix-read-2=20.59(1) lunix-read-3=20.59(1) lunix-read-0=20.57(1) lunix-coord=0.63(1) 
- partA/c64k/q4/seqr/rep2: 7792.732481 IOPS, 8.18 GB/s, p99 46923.776 us, daemon CPU 139.440s (596.45 us/IO), rss 1766808 KiB, split lunix-q2=29.74(1) lunix-q3=29.72(1) lunix-read-1=19.84(1) lunix-read-2=19.83(1) lunix-read-3=19.84(1) lunix-read-0=19.83(1) lunix-coord=0.50(1) 
- partA/c64k/q4/seqr/rep3: 7761.996736 IOPS, 8.15 GB/s, p99 46399.488 us, daemon CPU 138.450s (594.56 us/IO), rss 1766808 KiB, split lunix-q2=29.75(1) lunix-q3=29.73(1) lunix-read-1=19.72(1) lunix-read-2=19.70(1) lunix-read-3=19.69(1) lunix-read-0=19.72(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569236 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/r4k/rep1: 228507.28212 IOPS, 0.94 GB/s, p99 28442.624 us, daemon CPU 110.140s (3.62 us/IO), rss 692816 KiB, split lunix-q3=23.54(1) lunix-q2=23.14(1) lunix-read-3=15.79(1) lunix-read-0=15.77(1) lunix-read-2=15.75(1) lunix-read-1=15.73(1) lunix-coord=0.32(1) 
- partA/c16k/q4/r4k/rep2: 240792.001333 IOPS, 0.99 GB/s, p99 27394.048 us, daemon CPU 111.640s (15.45 us/IO), rss 841172 KiB, split lunix-q3=24.42(1) lunix-q2=24.26(1) lunix-read-2=15.72(1) lunix-read-3=15.69(1) lunix-read-0=15.71(1) lunix-read-1=15.71(1) 
- partA/c16k/q4/r4k/rep3: 243929.078487 IOPS, 1.00 GB/s, p99 28704.768 us, daemon CPU 111.930s (15.30 us/IO), rss 841172 KiB, split lunix-q3=24.23(1) lunix-q2=24.22(1) lunix-read-2=15.84(1) lunix-read-0=15.85(1) lunix-read-3=15.84(1) lunix-read-1=15.83(1) 
- partA/c16k/q4/w4k/rep1: 48813.124584 IOPS, 0.20 GB/s, p99 459276.288 us, daemon CPU 110.750s (37.19 us/IO), rss 1448776 KiB, split lunix-q3=10.29(1) lunix-q2=10.54(1) lunix-coord=25.29(1) iou-wrk-821670=1.57(1) iou-wrk-821669=1.57(1) lunix-space-lad=1.54(1) 
- partA/c16k/q4/w4k/rep2: 39721.326889 IOPS, 0.16 GB/s, p99 509607.936 us, daemon CPU 99.360s (41.01 us/IO), rss 1565056 KiB, split lunix-q3=9.34(1) lunix-q2=9.66(1) lunix-coord=26.82(1) iou-wrk-821670=0.88(1) iou-wrk-821669=0.90(1) lunix-space-tim=0.27(1) 
- partA/c16k/q4/w4k/rep3: 47848.390585 IOPS, 0.20 GB/s, p99 455081.984 us, daemon CPU 123.130s (42.89 us/IO), rss 1598592 KiB, split lunix-q2=11.22(1) lunix-q3=11.09(1) lunix-coord=31.51(1) iou-wrk-821670=1.15(1) iou-wrk-821669=1.16(1) lunix-space-tim=0.22(1) 
- partA/c16k/q4/seqw/rep1: 521.727223 IOPS, 0.55 GB/s, p99 3405774.848 us, daemon CPU 58.420s (2112.72 us/IO), rss 1839268 KiB, split lunix-q2=3.03(1) lunix-q3=2.72(1) lunix-coord=14.47(1) iou-wrk-821669=0.77(1) iou-wrk-821670=0.74(1) 
- partA/c16k/q4/seqw/rep2: 581.256771 IOPS, 0.62 GB/s, p99 4076863.488 us, daemon CPU 67.780s (3533.62 us/IO), rss 1778728 KiB, split lunix-coord=14.93(1) lunix-q2=3.62(1) lunix-q3=3.49(1) iou-wrk-821669=0.86(1) iou-wrk-821670=0.88(1) 
- partA/c16k/q4/seqw/rep3: 733.606167 IOPS, 0.78 GB/s, p99 2197815.296 us, daemon CPU 84.450s (3597.38 us/IO), rss 1869652 KiB, split lunix-coord=18.34(1) lunix-q2=4.37(1) lunix-q3=4.17(1) iou-wrk-821669=1.06(1) iou-wrk-821670=1.08(1) 
- partA/c16k/q4/seqr/rep1: 3930.980575 IOPS, 4.13 GB/s, p99 73924.608 us, daemon CPU 119.710s (982.35 us/IO), rss 1732396 KiB, split lunix-coord=2.64(1) lunix-q2=16.79(1) lunix-q3=16.81(1) lunix-read-2=20.84(1) lunix-read-0=20.86(1) lunix-read-1=20.85(1) lunix-read-3=20.82(1) 
- partA/c16k/q4/seqr/rep2: 3947.24763 IOPS, 4.15 GB/s, p99 71827.456 us, daemon CPU 119.610s (1010.07 us/IO), rss 1735900 KiB, split lunix-q2=17.57(1) lunix-q3=17.41(1) lunix-coord=1.15(1) lunix-read-2=20.88(1) lunix-read-0=20.85(1) lunix-read-3=20.86(1) lunix-read-1=20.80(1) 
- partA/c16k/q4/seqr/rep3: 3927.257609 IOPS, 4.13 GB/s, p99 67633.152 us, daemon CPU 118.340s (972.03 us/IO), rss 1735900 KiB, split lunix-q2=17.56(1) lunix-q3=17.53(1) lunix-read-2=20.75(1) lunix-read-0=20.80(1) lunix-read-1=20.83(1) lunix-read-3=20.76(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
