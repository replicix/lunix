# M16 Part A (2026-11-21T20:45:29Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 3 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 2.53 9.65 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 9.65 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569344 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/w4k/rep1: 31865.313776 IOPS, 0.13 GB/s, p99 450887.68 us, daemon CPU 80.310s (40.65 us/IO), rss 1209972 KiB, split lunix-coord=14.49(1) lunix-q2=6.62(1) lunix-q3=6.60(1) iou-wrk-152033=0.44(1) iou-wrk-152034=0.43(1) lunix-space-lad=0.47(1) lunix-space-tim=0.19(1) 
- partA/c64k/q4/w4k/rep2: 21504.391526 IOPS, 0.09 GB/s, p99 583008.256 us, daemon CPU 60.810s (43.50 us/IO), rss 1249256 KiB, split lunix-coord=11.43(1) lunix-q2=5.01(1) lunix-q3=4.95(1) lunix-space-lad=0.92(1) iou-wrk-152033=0.31(1) iou-wrk-152034=0.30(1) lunix-space-tim=0.25(1) 
- partA/c64k/q4/w4k/rep3: 32729.897181 IOPS, 0.13 GB/s, p99 549453.824 us, daemon CPU 88.110s (44.13 us/IO), rss 1282828 KiB, split lunix-coord=17.29(1) lunix-q2=7.10(1) lunix-q3=6.99(1) iou-wrk-152033=0.45(1) iou-wrk-152034=0.43(1) lunix-space-tim=0.33(1) 
- partA/c64k/q4/seqw/rep1: 608.92823 IOPS, 0.65 GB/s, p99 3841982.464 us, daemon CPU 37.750s (1937.32 us/IO), rss 1519560 KiB, split lunix-coord=7.97(1) lunix-q2=3.65(1) lunix-q3=2.73(1) iou-wrk-152033=0.75(1) iou-wrk-152034=0.64(1) lunix-space-tim=0.07(1) 
- partA/c64k/q4/seqw/rep2: 724.652938 IOPS, 0.77 GB/s, p99 2432696.32 us, daemon CPU 43.980s (1957.78 us/IO), rss 1480884 KiB, split lunix-coord=8.38(1) lunix-q2=4.21(1) lunix-q3=3.18(1) iou-wrk-152033=0.85(1) iou-wrk-152034=0.72(1) lunix-space-tim=0.09(1) 
- partA/c64k/q4/seqw/rep3: 653.878583 IOPS, 0.69 GB/s, p99 2600468.48 us, daemon CPU 41.760s (2060.16 us/IO), rss 1476508 KiB, split lunix-coord=7.71(1) lunix-q2=4.09(1) lunix-q3=3.03(1) iou-wrk-152033=0.77(1) iou-wrk-152034=0.66(1) lunix-space-tim=0.08(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569348 KiB
- PASS write the 2 GiB working set
- partA/c16k/q4/w4k/rep1: 44635.322935 IOPS, 0.18 GB/s, p99 438304.768 us, daemon CPU 95.390s (35.03 us/IO), rss 1425508 KiB, split lunix-coord=24.17(1) lunix-q2=9.01(1) lunix-q3=9.01(1) iou-wrk-182553=0.60(1) iou-wrk-182554=0.58(1) lunix-space-tim=0.34(1) 
- partA/c16k/q4/w4k/rep2: 41495.23635 IOPS, 0.17 GB/s, p99 329252.864 us, daemon CPU 94.560s (37.36 us/IO), rss 1542784 KiB, split lunix-coord=24.90(1) lunix-q2=8.88(1) lunix-q3=8.82(1) iou-wrk-182553=0.56(1) iou-wrk-182554=0.55(1) lunix-space-tim=0.35(1) 
- partA/c16k/q4/w4k/rep3: 45663.479159 IOPS, 0.19 GB/s, p99 299892.736 us, daemon CPU 109.040s (39.80 us/IO), rss 1573816 KiB, split lunix-coord=27.85(1) lunix-q2=9.79(1) lunix-q3=9.66(1) iou-wrk-182553=0.60(1) iou-wrk-182554=0.57(1) lunix-space-tim=0.33(1) 
- partA/c16k/q4/seqw/rep1: 542.941916 IOPS, 0.58 GB/s, p99 1719664.64 us, daemon CPU 51.480s (2963.02 us/IO), rss 1999640 KiB, split lunix-coord=11.81(1) lunix-q2=2.37(1) lunix-q3=2.69(1) iou-wrk-182553=0.61(1) iou-wrk-182554=0.56(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/seqw/rep2: 613.148332 IOPS, 0.65 GB/s, p99 1568669.696 us, daemon CPU 59.620s (3136.64 us/IO), rss 2007448 KiB, split lunix-coord=13.93(1) lunix-q3=2.93(1) lunix-q2=2.61(1) iou-wrk-182553=0.68(1) iou-wrk-182554=0.61(1) lunix-space-tim=0.08(1) 
- partA/c16k/q4/seqw/rep3: 645.856608 IOPS, 0.69 GB/s, p99 1199570.944 us, daemon CPU 65.010s (3247.00 us/IO), rss 1748480 KiB, split lunix-coord=14.36(1) lunix-q3=3.08(1) lunix-q2=2.81(1) iou-wrk-182553=0.72(1) iou-wrk-182554=0.65(1) lunix-space-tim=0.08(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
