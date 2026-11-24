# refsoak cell c6 (2026-11-24T13:05:08Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.98 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create refsoak
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-refill: fio rc 0, errors 0, 1549 IOPS, p99 115868 us
- rep1 discard of 6 GiB at 4 GiB: rc 0, 5 ms
- rep1-w4k: fio rc 0, errors 0, 22915 IOPS, p99 413139 us
- rep2-refill: fio rc 0, errors 0, 210 IOPS, p99 1736442 us
- rep2 discard of 6 GiB at 4 GiB: rc 0, 5 ms
- rep2-w4k: fio rc 0, errors 0, 19333 IOPS, p99 734003 us
- rep3-refill: fio rc 0, errors 0, 340 IOPS, p99 834666 us
- rep3 discard of 6 GiB at 4 GiB: rc 0, 5 ms
- rep3-w4k: fio rc 0, errors 0, 16302 IOPS, p99 985661 us
- rep4-refill: fio rc 0, errors 0, 783 IOPS, p99 283116 us
- rep4 discard of 6 GiB at 4 GiB: rc 0, 5 ms
- rep4-w4k: fio rc 0, errors 0, 18741 IOPS, p99 851444 us
- rep5-refill: fio rc 0, errors 0, 517 IOPS, p99 1702887 us
- rep5 discard of 6 GiB at 4 GiB: rc 0, 5 ms
- rep5-w4k: fio rc 0, errors 0, 19234 IOPS, p99 801112 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
