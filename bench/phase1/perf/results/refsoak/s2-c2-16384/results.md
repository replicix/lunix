# refsoak cell c2 (2026-11-24T11:17:47Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.07 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create refsoak
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 44554 IOPS, p99 408945 us
- rep2-w4k: fio rc 0, errors 0, 46963 IOPS, p99 304087 us
- rep3-w4k: fio rc 0, errors 0, 48181 IOPS, p99 248513 us
- rep4-w4k: fio rc 0, errors 0, 48491 IOPS, p99 219152 us
- rep5-w4k: fio rc 0, errors 0, 47435 IOPS, p99 214958 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
