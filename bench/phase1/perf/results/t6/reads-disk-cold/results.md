# M16 Part A (2026-11-20T11:18:42Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 15.87 60.79 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 60.79 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569260 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/r4k/rep1: 148992.268728 IOPS, 0.61 GB/s, p99 49545.216 us, daemon CPU 106.880s (23.14 us/IO), rss 1177492 KiB, split lunix-read-1=18.30(1) lunix-read-0=18.29(1) lunix-read-3=18.28(1) lunix-read-2=18.27(1) lunix-q2=16.72(1) lunix-q3=16.81(1) lunix-coord=0.10(1) 
- partA/c64k/q4/r4k/rep2: 134326.790416 IOPS, 0.55 GB/s, p99 70778.88 us, daemon CPU 95.490s (22.93 us/IO), rss 1178736 KiB, split lunix-read-1=16.33(1) lunix-read-3=16.34(1) lunix-read-2=16.34(1) lunix-read-0=16.33(1) lunix-q2=15.04(1) lunix-q3=14.96(1) lunix-coord=0.12(1) 
- partA/c64k/q4/r4k/rep3: 166833.611037 IOPS, 0.68 GB/s, p99 43778.048 us, daemon CPU 118.730s (23.72 us/IO), rss 1177744 KiB, split lunix-read-1=20.39(1) lunix-read-3=20.39(1) lunix-read-0=20.38(1) lunix-read-2=20.36(1) lunix-q2=18.56(1) lunix-q3=18.60(1) lunix-coord=0.05(1) 
- partA/c64k/q4/seqr/rep1: 5493.956245 IOPS, 5.77 GB/s, p99 1027604.48 us, daemon CPU 95.710s (580.70 us/IO), rss 1183720 KiB, split lunix-q2=20.30(1) lunix-q3=20.54(1) lunix-read-3=13.72(1) lunix-read-1=13.71(1) lunix-read-0=13.70(1) lunix-read-2=13.70(1) 
- partA/c64k/q4/seqr/rep2: 5400.486164 IOPS, 5.67 GB/s, p99 1019215.872 us, daemon CPU 95.620s (571.16 us/IO), rss 1184584 KiB, split lunix-q2=20.55(1) lunix-q3=20.67(1) lunix-read-3=13.59(1) lunix-read-1=13.59(1) lunix-read-0=13.58(1) lunix-read-2=13.59(1) 
- partA/c64k/q4/seqr/rep3: 5218.044362 IOPS, 5.48 GB/s, p99 1035993.088 us, daemon CPU 93.660s (579.01 us/IO), rss 1186120 KiB, split lunix-q2=20.34(1) lunix-q3=20.40(1) lunix-read-3=13.23(1) lunix-read-1=13.21(1) lunix-read-0=13.22(1) lunix-read-2=13.22(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569376 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/r4k/rep1: 3885.433227 IOPS, 0.02 GB/s, p99 2088763.392 us, daemon CPU 3.650s (30.30 us/IO), rss 1005284 KiB, split lunix-coord=0.51(1) lunix-q3=0.70(1) lunix-q2=0.71(1) lunix-read-3=0.40(1) lunix-read-1=0.40(1) lunix-read-2=0.39(1) lunix-read-0=0.39(1) 
- partA/c16k/q4/r4k/rep2: 3693.339516 IOPS, 0.02 GB/s, p99 2139095.04 us, daemon CPU 3.170s (28.61 us/IO), rss 1005628 KiB, split lunix-coord=0.27(1) lunix-q3=0.67(1) lunix-q2=0.67(1) lunix-read-3=0.39(1) lunix-read-1=0.39(1) lunix-read-2=0.39(1) lunix-read-0=0.39(1) 
- partA/c16k/q4/r4k/rep3: 3608.176496 IOPS, 0.01 GB/s, p99 2139095.04 us, daemon CPU 3.350s (30.95 us/IO), rss 1005628 KiB, split lunix-coord=0.51(1) lunix-q3=0.65(1) lunix-q2=0.66(1) lunix-read-3=0.38(1) lunix-read-1=0.38(1) lunix-read-2=0.38(1) lunix-read-0=0.37(1) 
- partA/c16k/q4/seqr/rep1: 2523.842777 IOPS, 2.66 GB/s, p99 1069547.52 us, daemon CPU 75.190s (961.03 us/IO), rss 1004204 KiB, split lunix-read-3=13.51(1) lunix-read-2=13.51(1) lunix-read-0=13.49(1) lunix-read-1=13.46(1) lunix-q3=10.72(1) lunix-q2=10.47(1) 
- partA/c16k/q4/seqr/rep2: 2497.173262 IOPS, 2.63 GB/s, p99 1069547.52 us, daemon CPU 75.010s (968.97 us/IO), rss 1003204 KiB, split lunix-read-2=13.41(1) lunix-read-0=13.40(1) lunix-read-3=13.39(1) lunix-read-1=13.37(1) lunix-q3=10.48(1) lunix-q2=10.94(1) 
- partA/c16k/q4/seqr/rep3: 2483.475196 IOPS, 2.61 GB/s, p99 1069547.52 us, daemon CPU 74.540s (968.21 us/IO), rss 1004896 KiB, split lunix-read-3=13.36(1) lunix-read-0=13.33(1) lunix-read-2=13.28(1) lunix-read-1=13.27(1) lunix-q3=10.57(1) lunix-q2=10.67(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
