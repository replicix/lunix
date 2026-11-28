# c5t6 block w4k (2026-11-28T13:02:36Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 114.38 330.62 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 330.62 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 67798 IOPS, p99 131596 us
- rep2-w4k: fio rc 0, errors 0, 71926 IOPS, p99 95945 us
- rep3-w4k: fio rc 0, errors 0, 68490 IOPS, p99 119013 us
- rep4-w4k: fio rc 0, errors 0, 67283 IOPS, p99 139461 us
- rep5-w4k: fio rc 0, errors 0, 66864 IOPS, p99 123208 us
- verify-w4k: fio rc 0, errors 0, 131587 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
