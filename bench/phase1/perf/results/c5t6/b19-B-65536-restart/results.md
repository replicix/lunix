# c5t6 block w4k (2026-11-26T02:06:49Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 3
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.02 1.99 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 33413 IOPS, p99 549454 us
- rep2-w4k: fio rc 0, errors 0, 23440 IOPS, p99 608174 us
- restart in rep3-w4k: stop 2436 ms, start 1330 ms, state 'attached' after 3848 ms
- rep3-w4k: fio rc 0, errors 0, 9688 IOPS, p99 1035993 us
- rep4-w4k: fio rc 0, errors 0, 27047 IOPS, p99 574620 us
- rep5-w4k: fio rc 0, errors 0, 22972 IOPS, p99 641729 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
