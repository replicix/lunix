# refsoak cell c1 (2026-11-24T10:46:21Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 3
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.03 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create refsoak
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 26745 IOPS, p99 574620 us
- rep2-w4k: fio rc 0, errors 0, 22381 IOPS, p99 658506 us
- restart in rep3-w4k: stop 2334 ms, start 724 ms, state 'attached' after 3066 ms
- rep3-w4k: fio rc 0, errors 0, 15305 IOPS, p99 918553 us
- rep4-w4k: fio rc 0, errors 0, 28942 IOPS, p99 549454 us
- rep5-w4k: fio rc 0, errors 0, 20081 IOPS, p99 759169 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
