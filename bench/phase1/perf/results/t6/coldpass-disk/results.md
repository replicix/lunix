# M16 Part A (2026-11-20T13:01:32Z, kernel 7.3.0, 16 cpus): chunks 65536 16384, queues 4, 1 reps
- destage_workers: 8 (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)
- daemon CPUs 0-7 (process), queue threads QCPUS_4=0-3 / QCPUS_8=0-7, fio CPUS 8-15; script cap 10800 s
- load average at the start: 25.97 110.43 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 110.43 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set

## chunk 64 KiB
- PASS create pa64k

### chunk 64 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569244 KiB
- PASS write the 4 GiB working set
- coldpass c64k q4: 4096 MiB in 17 s
- PASS lunix fsck --deep clean after q4 (exit 0)

## chunk 16 KiB
- PASS create pa16k

### chunk 16 KiB, 4 queues (queue CPUs 0-3)
- PASS attach with 4 queues
- idle resident set after attach: 569168 KiB
- PASS write the 4 GiB working set
- coldpass c16k q4: 4096 MiB in 17 s
- PASS lunix fsck --deep clean after q4 (exit 0)

overall FAIL=0 (noisy host: 1)
