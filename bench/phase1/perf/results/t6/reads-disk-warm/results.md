# M16 Part A (2026-11-20T11:54:55Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 2 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 35.08 141.69 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 141.69 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569248 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/r4k/rep1: 272110.014998 IOPS, 1.11 GB/s, p99 27394.048 us, daemon CPU 176.240s (20.89 us/IO), rss 871252 KiB, split lunix-q2=28.98(1) lunix-read-1=29.51(1) lunix-read-3=29.49(1) lunix-read-0=29.50(1) lunix-read-2=29.49(1) lunix-q3=28.84(1) lunix-coord=0.13(1) 
- partA/c64k/q4/r4k/rep2: 271153.279563 IOPS, 1.11 GB/s, p99 28180.48 us, daemon CPU 176.450s (21.69 us/IO), rss 871700 KiB, split lunix-q2=29.13(1) lunix-read-1=29.53(1) lunix-read-0=29.52(1) lunix-read-3=29.50(1) lunix-read-2=29.49(1) lunix-q3=29.04(1) lunix-coord=0.06(1) 
- partA/c64k/q4/seqr/rep1: 7179.992674 IOPS, 7.54 GB/s, p99 49545.216 us, daemon CPU 130.460s (586.13 us/IO), rss 876056 KiB, split lunix-q2=29.67(1) lunix-q3=29.72(1) lunix-read-0=17.75(1) lunix-read-1=17.72(1) lunix-read-3=17.73(1) lunix-read-2=17.72(1) lunix-coord=0.06(1) 
- partA/c64k/q4/seqr/rep2: 7180.404303 IOPS, 7.54 GB/s, p99 50069.504 us, daemon CPU 130.430s (605.49 us/IO), rss 876152 KiB, split lunix-q2=29.67(1) lunix-q3=29.72(1) lunix-read-0=17.77(1) lunix-read-1=17.73(1) lunix-read-3=17.72(1) lunix-read-2=17.72(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569168 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/r4k/rep1: 228662.37835 IOPS, 0.94 GB/s, p99 30015.488 us, daemon CPU 107.190s (15.63 us/IO), rss 1182464 KiB, split lunix-q2=22.28(1) lunix-q3=22.49(1) lunix-read-1=15.50(1) lunix-read-3=15.44(1) lunix-read-2=15.44(1) lunix-read-0=15.45(1) lunix-coord=0.35(1) 
- partA/c16k/q4/r4k/rep2: 229743.609398 IOPS, 0.94 GB/s, p99 30015.488 us, daemon CPU 108.040s (15.68 us/IO), rss 1179936 KiB, split lunix-q2=22.53(1) lunix-q3=22.47(1) lunix-read-3=15.70(1) lunix-read-1=15.68(1) lunix-read-0=15.66(1) lunix-read-2=15.65(1) lunix-coord=0.26(1) 
- partA/c16k/q4/seqr/rep1: 3914.058602 IOPS, 4.11 GB/s, p99 70778.88 us, daemon CPU 115.040s (948.11 us/IO), rss 1195156 KiB, split lunix-q2=16.77(1) lunix-q3=16.68(1) lunix-read-1=20.28(1) lunix-read-3=20.23(1) lunix-read-0=20.26(1) lunix-read-2=20.25(1) lunix-coord=0.51(1) 
- partA/c16k/q4/seqr/rep2: 3864.507117 IOPS, 4.06 GB/s, p99 70778.88 us, daemon CPU 114.280s (985.72 us/IO), rss 1196524 KiB, split lunix-q2=16.86(1) lunix-q3=16.80(1) lunix-read-1=20.14(1) lunix-read-3=20.16(1) lunix-read-2=20.15(1) lunix-read-0=20.09(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
