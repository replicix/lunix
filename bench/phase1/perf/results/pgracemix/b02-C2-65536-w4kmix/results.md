# c5t6 block w4kmix (2026-11-27T10:37:13Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";gc_pressure_grace_ms = 2000;destage_workers = 8;; log_level debug
- load average at the start: 111.63 284.14 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 284.14 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4kmix: fio rc 0, errors 0, 65418 IOPS, p99 158335 us
- rep2-w4kmix: fio rc 0, errors 0, 78943 IOPS, p99 88605 us
- rep3-w4kmix: fio rc 0, errors 0, 70101 IOPS, p99 137363 us
- rep4-w4kmix: fio rc 0, errors 0, 72318 IOPS, p99 128451 us
- rep5-w4kmix: fio rc 0, errors 0, 73245 IOPS, p99 112722 us
- verify-w4k: fio rc 0, errors 0, 105685 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
