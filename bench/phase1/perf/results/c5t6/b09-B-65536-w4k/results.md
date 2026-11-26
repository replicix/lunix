# c5t6 block w4k (2026-11-25T20:54:41Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_fast_recheck = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 0.00 1.97 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- rep1-w4k: fio rc 0, errors 0, 34660 IOPS, p99 509608 us
- rep2-w4k: fio rc 0, errors 0, 25223 IOPS, p99 616563 us
- rep3-w4k: fio rc 0, errors 0, 19227 IOPS, p99 734003 us
- rep4-w4k: fio rc 0, errors 0, 23452 IOPS, p99 666894 us
- rep5-w4k: fio rc 0, errors 0, 19764 IOPS, p99 717226 us
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
