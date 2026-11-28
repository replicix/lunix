# c5t6 block w4k (2026-11-28T12:47:53Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: gc_pressure_grace_ms = 0;gc_fast_recheck = false;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 119.29 254.99 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 254.99 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 30444 IOPS, p99 421528 us
- rep2-w4k: fio rc 0, errors 0, 27321 IOPS, p99 471859 us
- rep3-w4k: fio rc 0, errors 0, 28408 IOPS, p99 534774 us
- rep4-w4k: fio rc 0, errors 0, 27646 IOPS, p99 541065 us
- rep5-w4k: fio rc 0, errors 0, 28775 IOPS, p99 530579 us
- verify-w4k: fio rc 0, errors 0, 51354 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
