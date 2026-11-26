# c5t6 block w4kz (2026-11-26T00:33:18Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 41773 IOPS, p99 320864 us
- rep2-w4kz: fio rc 0, errors 0, 49203 IOPS, p99 299893 us
- rep3-w4kz: fio rc 0, errors 0, 46920 IOPS, p99 278921 us
- rep4-w4kz: fio rc 0, errors 0, 52101 IOPS, p99 248513 us
- rep5-w4kz: fio rc 0, errors 0, 47775 IOPS, p99 270533 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
