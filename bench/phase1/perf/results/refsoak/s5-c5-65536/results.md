# refsoak cell c5 (2026-11-24T12:32:52Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "shadow";destage_workers = 8;; log_level debug
- load average at the start: 0.01 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create refsoak
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-comp: fio rc 0, errors 0, 16373 IOPS, p99 759169 us
- rep2-comp: fio rc 0, errors 0, 14110 IOPS, p99 1010827 us
- rep3-comp: fio rc 0, errors 0, 10717 IOPS, p99 1820328 us
- rep4-comp: fio rc 0, errors 0, 9433 IOPS, p99 1837105 us
- rep5-comp: fio rc 0, errors 0, 10302 IOPS, p99 1786774 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
