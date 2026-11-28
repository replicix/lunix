# c5t6 block w4kz (2026-11-28T13:45:17Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: gc_pressure_grace_ms = 0;gc_fast_recheck = false;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 116.70 331.71 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 331.71 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 37391 IOPS, p99 287310 us
- rep2-w4kz: fio rc 0, errors 0, 42901 IOPS, p99 316670 us
- rep3-w4kz: fio rc 0, errors 0, 45288 IOPS, p99 278921 us
- rep4-w4kz: fio rc 0, errors 0, 42117 IOPS, p99 270533 us
- rep5-w4kz: fio rc 0, errors 0, 45913 IOPS, p99 252707 us
- verify-w4k: fio rc 0, errors 0, 49691 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
