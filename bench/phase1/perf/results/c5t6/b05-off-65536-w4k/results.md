# c5t6 block w4k (2026-11-25T18:49:49Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 0.02 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 28390 IOPS, p99 517997 us
- rep2-w4k: fio rc 0, errors 0, 20082 IOPS, p99 734003 us
- rep3-w4k: fio rc 0, errors 0, 23484 IOPS, p99 616563 us
- rep4-w4k: fio rc 0, errors 0, 20993 IOPS, p99 700449 us
- rep5-w4k: fio rc 0, errors 0, 24516 IOPS, p99 616563 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
