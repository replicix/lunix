# c5t6 block pin (2026-11-26T02:38:04Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.06 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- PASS snapshot pin on commit 17 written
- rep1-w4k: fio rc 0, errors 0, 28902 IOPS, p99 534774 us
- rep2-w4k: fio rc 0, errors 0, 17026 IOPS, p99 884998 us
- rep3-w4k: fio rc 0, errors 0, 21516 IOPS, p99 633340 us
- rep4-w4k: fio rc 0, errors 0, 21808 IOPS, p99 616563 us
- rep5-w4k: fio rc 0, errors 0, 16756 IOPS, p99 926941 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
