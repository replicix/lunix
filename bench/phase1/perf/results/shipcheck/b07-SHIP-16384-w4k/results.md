# c5t6 block w4k (2026-11-28T13:23:58Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 116.06 331.61 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 331.61 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 66968 IOPS, p99 110625 us
- rep2-w4k: fio rc 0, errors 0, 69093 IOPS, p99 91750 us
- rep3-w4k: fio rc 0, errors 0, 66082 IOPS, p99 101188 us
- rep4-w4k: fio rc 0, errors 0, 64954 IOPS, p99 107479 us
- rep5-w4k: fio rc 0, errors 0, 63035 IOPS, p99 108528 us
- verify-w4k: fio rc 0, errors 0, 120435 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
