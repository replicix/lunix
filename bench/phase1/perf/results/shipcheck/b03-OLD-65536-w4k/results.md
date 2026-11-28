# c5t6 block w4k (2026-11-28T12:55:13Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: gc_pressure_grace_ms = 0;gc_fast_recheck = false;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 114.39 316.47 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 316.47 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 31793 IOPS, p99 442499 us
- rep2-w4k: fio rc 0, errors 0, 22944 IOPS, p99 480248 us
- rep3-w4k: fio rc 0, errors 0, 36997 IOPS, p99 488636 us
- rep4-w4k: fio rc 0, errors 0, 19253 IOPS, p99 759169 us
- rep5-w4k: fio rc 0, errors 0, 31874 IOPS, p99 549454 us
- verify-w4k: fio rc 0, errors 0, 32480 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
