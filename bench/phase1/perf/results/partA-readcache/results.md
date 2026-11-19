# M16 Part A (2026-11-18T23:25:54Z, kernel 7.3, 16 cpus): chunks 65536 16384, queues 4 8, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 6000 s
- load average at the start: 0.00 0.35 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k
- PASS read_cache_max = 8589934592

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568844 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/r4k/rep1: 318086.691331 IOPS, 1.30 GB/s, p99 26345.472 us, daemon CPU 54.500s (5.71 us/IO), rss 4849080 KiB, split lunix-q2=27.01(1) lunix-q3=26.64(1) lunix-coord=0.08(1) 
- partA/c64k/q4/r4k/rep2: 373178.848782 IOPS, 1.53 GB/s, p99 18481.152 us, daemon CPU 60.040s (5.36 us/IO), rss 4849080 KiB, split lunix-q2=30.00(1) lunix-q3=29.97(1) 
- partA/c64k/q4/r4k/rep3: 373047.12862 IOPS, 1.53 GB/s, p99 18743.296 us, daemon CPU 60.080s (5.20 us/IO), rss 4849080 KiB, split lunix-q2=29.98(1) lunix-q3=30.00(1) lunix-coord=0.06(1) 
- partA/c64k/q4/seqr/rep1: 9117.846912 IOPS, 9.57 GB/s, p99 29491.2 us, daemon CPU 59.730s (218.36 us/IO), rss 4850104 KiB, split lunix-q2=29.85(1) lunix-q3=29.83(1) 
- partA/c64k/q4/seqr/rep2: 9202.591088 IOPS, 9.66 GB/s, p99 28704.768 us, daemon CPU 60.050s (217.51 us/IO), rss 4850104 KiB, split lunix-q2=29.99(1) lunix-q3=30.00(1) 
- partA/c64k/q4/seqr/rep3: 9142.176621 IOPS, 9.59 GB/s, p99 28966.912 us, daemon CPU 60.030s (218.88 us/IO), rss 4850104 KiB, split lunix-q2=29.99(1) lunix-q3=29.99(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 64 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1149200 KiB
- PASS write the 4 GiB working set
- partA/c64k/q8/r4k/rep1: 583372.008533 IOPS, 2.39 GB/s, p99 16711.68 us, daemon CPU 89.340s (5.10 us/IO), rss 5405512 KiB, split lunix-q4=22.56(1) lunix-q5=22.50(1) lunix-q6=22.43(1) lunix-q7=20.58(1) lunix-coord=0.06(1) 
- partA/c64k/q8/r4k/rep2: 831952.169855 IOPS, 3.41 GB/s, p99 4947.968 us, daemon CPU 108.500s (4.21 us/IO), rss 5405512 KiB, split lunix-q4=27.17(1) lunix-q5=27.12(1) lunix-q6=27.06(1) lunix-q7=27.04(1) 
- partA/c64k/q8/r4k/rep3: 834622.291847 IOPS, 3.42 GB/s, p99 4882.432 us, daemon CPU 108.580s (4.34 us/IO), rss 5405512 KiB, split lunix-q4=27.18(1) lunix-q5=27.11(1) lunix-q6=27.10(1) lunix-q7=27.04(1) lunix-coord=0.06(1) 
- partA/c64k/q8/seqr/rep1: 11389.847445 IOPS, 11.95 GB/s, p99 44302.336 us, daemon CPU 118.870s (336.66 us/IO), rss 5406312 KiB, split lunix-q4=29.83(1) lunix-q5=29.82(1) lunix-q6=29.83(1) lunix-q7=29.26(1) 
- partA/c64k/q8/seqr/rep2: 10748.092875 IOPS, 11.28 GB/s, p99 35913.728 us, daemon CPU 92.140s (285.76 us/IO), rss 5406312 KiB, split lunix-q4=29.98(1) lunix-q5=29.97(1) lunix-q6=30.00(1) lunix-q7=2.09(1) 
- partA/c64k/q8/seqr/rep3: 11050.966023 IOPS, 11.60 GB/s, p99 44826.624 us, daemon CPU 105.530s (318.31 us/IO), rss 5406312 KiB, split lunix-q4=29.97(1) lunix-q6=29.98(1) lunix-q5=16.33(1) lunix-q7=29.14(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

## chunk 16 KiB
- PASS create pa16k
- PASS read_cache_max = 8589934592

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568884 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/r4k/rep1: 154875.645769 IOPS, 0.63 GB/s, p99 80216.064 us, daemon CPU 48.810s (10.17 us/IO), rss 4925984 KiB, split lunix-q3=14.05(1) lunix-q2=14.04(1) lunix-read-0=4.97(1) lunix-read-1=4.91(1) lunix-read-2=4.91(1) lunix-read-3=4.90(1) lunix-coord=0.30(1) iou-wrk-2665417=0.09(1) iou-wrk-2665416=0.10(1) 
- partA/c16k/q4/r4k/rep2: 353272.339433 IOPS, 1.45 GB/s, p99 20316.16 us, daemon CPU 60.020s (5.66 us/IO), rss 4925984 KiB, split lunix-q3=30.00(1) lunix-q2=29.97(1) 
- partA/c16k/q4/r4k/rep3: 355428.676177 IOPS, 1.46 GB/s, p99 19267.584 us, daemon CPU 60.290s (5.47 us/IO), rss 4925984 KiB, split lunix-q3=29.97(1) lunix-q2=30.01(1) lunix-coord=0.27(1) 
- partA/c16k/q4/seqr/rep1: 7567.744945 IOPS, 7.94 GB/s, p99 34865.152 us, daemon CPU 59.750s (263.18 us/IO), rss 4925984 KiB, split lunix-q3=29.87(1) lunix-q2=29.84(1) 
- partA/c16k/q4/seqr/rep2: 7672.861092 IOPS, 8.05 GB/s, p99 34340.864 us, daemon CPU 60.060s (260.92 us/IO), rss 4925984 KiB, split lunix-q3=30.00(1) lunix-q2=30.01(1) 
- partA/c16k/q4/seqr/rep3: 7562.175303 IOPS, 7.94 GB/s, p99 34865.152 us, daemon CPU 60.090s (256.33 us/IO), rss 4925984 KiB, split lunix-q3=30.03(1) lunix-q2=30.02(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

### chunk 16 KiB, 8 queues (queue CPUs 0-7)
- PASS attach with 8 queues
- idle resident set after attach: 1154968 KiB
- PASS write the 4 GiB working set
- partA/c16k/q8/r4k/rep1: 323951.569895 IOPS, 1.33 GB/s, p99 42729.472 us, daemon CPU 68.670s (7.07 us/IO), rss 5482440 KiB, split lunix-q4=12.30(1) lunix-q5=12.35(1) lunix-q7=12.67(1) lunix-q6=12.29(1) lunix-read-2=4.82(1) lunix-read-0=4.74(1) lunix-read-1=4.62(1) lunix-read-3=4.60(1) lunix-coord=0.26(1) 
- partA/c16k/q8/r4k/rep2: 771690.520632 IOPS, 3.16 GB/s, p99 5472.256 us, daemon CPU 109.510s (4.73 us/IO), rss 5482440 KiB, split lunix-q7=28.01(1) lunix-q4=27.12(1) lunix-q5=27.15(1) lunix-q6=27.11(1) 
- partA/c16k/q8/r4k/rep3: 774154.556363 IOPS, 3.17 GB/s, p99 5406.72 us, daemon CPU 109.800s (4.58 us/IO), rss 5482440 KiB, split lunix-q7=28.04(1) lunix-q4=27.14(1) lunix-q5=27.13(1) lunix-q6=27.12(1) lunix-coord=0.27(1) 
- partA/c16k/q8/seqr/rep1: 9159.372502 IOPS, 9.61 GB/s, p99 53215.232 us, daemon CPU 98.190s (357.34 us/IO), rss 5484412 KiB, split lunix-q7=27.73(1) lunix-q5=27.99(1) lunix-q6=22.11(1) lunix-q4=20.24(1) 
- partA/c16k/q8/seqr/rep2: 9110.86696 IOPS, 9.56 GB/s, p99 58982.4 us, daemon CPU 99.430s (363.78 us/IO), rss 5484412 KiB, split lunix-q5=27.37(1) lunix-q6=27.34(1) lunix-q4=27.22(1) lunix-q7=17.41(1) 
- partA/c16k/q8/seqr/rep3: 9183.852918 IOPS, 9.64 GB/s, p99 52690.944 us, daemon CPU 102.940s (361.57 us/IO), rss 5484412 KiB, split lunix-q5=22.83(1) lunix-q6=25.74(1) lunix-q4=27.12(1) lunix-q7=27.14(1) 
- PASS lunix fsck --deep clean after q8 (exit 0)

overall FAIL=0 (noisy host: 0)
