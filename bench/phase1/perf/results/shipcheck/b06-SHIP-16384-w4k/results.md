# c5t6 block w4k (2026-11-28T13:16:50Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 116.08 333.60 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 333.60 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 65197 IOPS, p99 115868 us
- rep2-w4k: fio rc 0, errors 0, 66072 IOPS, p99 101188 us
- rep3-w4k: fio rc 0, errors 0, 63090 IOPS, p99 120062 us
- rep4-w4k: fio rc 0, errors 0, 65045 IOPS, p99 110625 us
- rep5-w4k: fio rc 0, errors 0, 63191 IOPS, p99 112722 us
- verify-w4k: fio rc 0, errors 0, 127858 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
