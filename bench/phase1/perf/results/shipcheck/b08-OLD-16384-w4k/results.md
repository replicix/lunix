# c5t6 block w4k (2026-11-28T13:31:05Z, kernel 7.0.0, 16 cpus): chunk 16384, 5 reps, restart rep 0
- daemon settings: gc_pressure_grace_ms = 0;gc_fast_recheck = false;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 115.60 333.61 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 333.61 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 35266 IOPS, p99 346030 us
- rep2-w4k: fio rc 0, errors 0, 44961 IOPS, p99 291504 us
- rep3-w4k: fio rc 0, errors 0, 45919 IOPS, p99 252707 us
- rep4-w4k: fio rc 0, errors 0, 43901 IOPS, p99 267387 us
- rep5-w4k: fio rc 0, errors 0, 43283 IOPS, p99 248513 us
- verify-w4k: fio rc 0, errors 0, 98020 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
