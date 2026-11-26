# c5t6 block w4k (2026-11-25T19:21:00Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 32668 IOPS, p99 541065 us
- rep2-w4k: fio rc 0, errors 0, 22152 IOPS, p99 624951 us
- rep3-w4k: fio rc 0, errors 0, 24341 IOPS, p99 583008 us
- rep4-w4k: fio rc 0, errors 0, 21902 IOPS, p99 658506 us
- rep5-w4k: fio rc 0, errors 0, 21536 IOPS, p99 650117 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
