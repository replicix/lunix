# c5t6 block w4kz (2026-11-28T13:38:12Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 114.83 327.44 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 327.44 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kz: fio rc 0, errors 0, 70385 IOPS, p99 139461 us
- rep2-w4kz: fio rc 0, errors 0, 72815 IOPS, p99 123208 us
- rep3-w4kz: fio rc 0, errors 0, 73139 IOPS, p99 112722 us
- rep4-w4kz: fio rc 0, errors 0, 72262 IOPS, p99 109576 us
- rep5-w4kz: fio rc 0, errors 0, 71496 IOPS, p99 125305 us
- verify-w4k: fio rc 0, errors 0, 128496 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
