# c5t6 block w4kz (2026-11-26T01:35:39Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 35888 IOPS, p99 333447 us
- rep2-w4kz: fio rc 0, errors 0, 50409 IOPS, p99 283116 us
- rep3-w4kz: fio rc 0, errors 0, 40666 IOPS, p99 308281 us
- rep4-w4kz: fio rc 0, errors 0, 49627 IOPS, p99 278921 us
- rep5-w4kz: fio rc 0, errors 0, 50210 IOPS, p99 246415 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
