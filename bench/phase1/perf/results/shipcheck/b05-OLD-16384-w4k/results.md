# c5t6 block w4k (2026-11-28T13:09:42Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: gc_pressure_grace_ms = 0;gc_fast_recheck = false;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 116.91 334.84 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 334.84 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 35217 IOPS, p99 446693 us
- rep2-w4k: fio rc 0, errors 0, 45025 IOPS, p99 256901 us
- rep3-w4k: fio rc 0, errors 0, 41200 IOPS, p99 312476 us
- rep4-w4k: fio rc 0, errors 0, 44572 IOPS, p99 254804 us
- rep5-w4k: fio rc 0, errors 0, 41594 IOPS, p99 256901 us
- verify-w4k: fio rc 0, errors 0, 94346 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
