# M16 Part A (2026-11-19T01:02:13Z, kernel 7.3, 16 cpus): chunks 65536, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 2400 s
- load average at the start: 2.18 108.75 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 108.75 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k
- PASS read_cache_max = 4294967296

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 568836 KiB
- PASS write the 2 GiB working set
- partA/c64k/q4/r4k/rep1: 372901.009899 IOPS, 1.53 GB/s, p99 18743.296 us, daemon CPU 59.780s (5.34 us/IO), rss 2709516 KiB, split lunix-q3=29.84(1) lunix-q2=29.84(1) 
- partA/c64k/q4/w4k/rep1: 4481.773832 IOPS, 0.02 GB/s, p99 17112760.32 us, daemon CPU 26.760s (61.56 us/IO), rss 633776 KiB, split lunix-q3=1.02(1) lunix-q2=1.02(1) lunix-space=11.45(1) lunix-coord=2.70(1) lunixd-trim=0.07(1) 
- partA/c64k/q4/seqw/rep1: 354.109211 IOPS, 0.38 GB/s, p99 17112760.32 us, daemon CPU 27.500s (1725.77 us/IO), rss 647836 KiB, split lunix-q3=2.41(1) lunix-q2=2.17(1) lunix-space=5.87(1) lunix-coord=4.21(1) iou-wrk-2745626=0.11(1) iou-wrk-2745625=0.11(1) 
- partA/c64k/q4/seqr/rep1: 8934.612438 IOPS, 9.38 GB/s, p99 30539.776 us, daemon CPU 59.800s (223.10 us/IO), rss 2794972 KiB, split lunix-q3=29.86(1) lunix-q2=29.84(1) lunix-coord=0.05(1) 
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=1 (noisy host: 1)
