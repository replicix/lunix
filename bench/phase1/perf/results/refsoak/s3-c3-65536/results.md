# refsoak cell c3 (2026-11-24T11:49:02Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create refsoak
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 36441 IOPS, p99 337641 us
- rep2-w4kz: fio rc 0, errors 0, 47999 IOPS, p99 299893 us
- rep3-w4kz: fio rc 0, errors 0, 46503 IOPS, p99 295698 us
- rep4-w4kz: fio rc 0, errors 0, 46803 IOPS, p99 270533 us
- rep5-w4kz: fio rc 0, errors 0, 50840 IOPS, p99 270533 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
