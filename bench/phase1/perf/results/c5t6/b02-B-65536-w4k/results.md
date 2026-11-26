# c5t6 block w4k (2026-11-25T17:16:13Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 34293 IOPS, p99 522191 us
- rep2-w4k: fio rc 0, errors 0, 23848 IOPS, p99 583008 us
- rep3-w4k: fio rc 0, errors 0, 21664 IOPS, p99 650117 us
- rep4-w4k: fio rc 0, errors 0, 21230 IOPS, p99 633340 us
- rep5-w4k: fio rc 0, errors 0, 22636 IOPS, p99 591397 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
