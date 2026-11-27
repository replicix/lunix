# c5t6 block w4kmix (2026-11-27T10:51:26Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";gc_pressure_grace_ms = 5000;destage_workers = 8;; log_level debug
- load average at the start: 114.92 330.68 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 330.68 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kmix: fio rc 0, errors 0, 65306 IOPS, p99 160432 us
- rep2-w4kmix: fio rc 0, errors 0, 60705 IOPS, p99 187695 us
- rep3-w4kmix: fio rc 0, errors 0, 66157 IOPS, p99 139461 us
- rep4-w4kmix: fio rc 0, errors 0, 62349 IOPS, p99 177209 us
- rep5-w4kmix: fio rc 0, errors 0, 66632 IOPS, p99 141558 us
- verify-w4k: fio rc 0, errors 0, 91319 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
