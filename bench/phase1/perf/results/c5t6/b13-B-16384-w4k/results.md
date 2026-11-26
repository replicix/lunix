# c5t6 block w4k (2026-11-25T22:59:38Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 51478 IOPS, p99 291504 us
- rep2-w4k: fio rc 0, errors 0, 48732 IOPS, p99 223347 us
- rep3-w4k: fio rc 0, errors 0, 48507 IOPS, p99 238027 us
- rep4-w4k: fio rc 0, errors 0, 48509 IOPS, p99 246415 us
- rep5-w4k: fio rc 0, errors 0, 51089 IOPS, p99 204472 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
