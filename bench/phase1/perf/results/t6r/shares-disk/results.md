# M16 Part A (2026-11-21T00:10:48Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 10.80 137.19 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 137.19 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569244 KiB
- PASS write the 4 GiB working set
- partA/c64k/q4/w4k/rep1: 3565.811649 IOPS, 0.01 GB/s, p99 7818182.656 us, daemon CPU 22.830s (95.56 us/IO), rss 1216428 KiB, split lunix-coord=5.63(9) lunix-q2=0.98(1) lunix-q3=0.92(1) lunix-space-tim=0.10(1) 
- partA/c64k/q4/seqw/rep1: 52.927428 IOPS, 0.06 GB/s, p99 10938744.832 us, daemon CPU 7.730s (2704.61 us/IO), rss 1626192 KiB, split lunix-coord=2.68(1) lunix-q2=0.31(1) lunix-q3=0.29(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569404 KiB
- PASS write the 4 GiB working set
- partA/c16k/q4/w4k/rep1: 8517.098309 IOPS, 0.03 GB/s, p99 2004877.312 us, daemon CPU 27.990s (53.01 us/IO), rss 1400120 KiB, split lunix-coord=10.39(1) lunix-q2=2.17(1) lunix-q3=2.23(1) lunix-space-tim=0.16(1) 
- partA/c16k/q4/seqw/rep1: 63.985707 IOPS, 0.07 GB/s, p99 6073352.192 us, daemon CPU 11.280s (3264.62 us/IO), rss 1831172 KiB, split lunix-coord=6.72(1) lunix-q2=0.36(1) lunix-q3=0.33(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
