# c5t6 block w4k (2026-11-28T12:40:47Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 1.98 0.65 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 70485 IOPS, p99 105382 us
- rep2-w4k: fio rc 0, errors 0, 71710 IOPS, p99 92799 us
- rep3-w4k: fio rc 0, errors 0, 69255 IOPS, p99 117965 us
- rep4-w4k: fio rc 0, errors 0, 67645 IOPS, p99 106430 us
- rep5-w4k: fio rc 0, errors 0, 67816 IOPS, p99 111673 us
- verify-w4k: fio rc 0, errors 0, 139062 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
