# c5t6 block pin (2026-11-28T13:52:23Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: destage_workers = 8;; log_level debug
- load average at the start: 106.80 329.02 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 329.02 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- PASS snapshot pin on commit 18 written
- rep1-w4k: fio rc 0, errors 0, 68056 IOPS, p99 117965 us
- rep2-w4k: fio rc 0, errors 0, 68768 IOPS, p99 108528 us
- rep3-w4k: fio rc 0, errors 0, 68228 IOPS, p99 103285 us
- snapshot pin removed after repetition 3
- rep4-w4k: fio rc 0, errors 0, 69169 IOPS, p99 114819 us
- rep5-w4k: fio rc 0, errors 0, 69422 IOPS, p99 106430 us
- verify-w4k: fio rc 0, errors 0, 134074 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
