# c5t6 block w4kz (2026-11-26T00:02:08Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.01 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 35952 IOPS, p99 316670 us
- rep2-w4kz: fio rc 0, errors 0, 51059 IOPS, p99 274727 us
- rep3-w4kz: fio rc 0, errors 0, 45678 IOPS, p99 283116 us
- rep4-w4kz: fio rc 0, errors 0, 52065 IOPS, p99 261095 us
- rep5-w4kz: fio rc 0, errors 0, 46649 IOPS, p99 278921 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
