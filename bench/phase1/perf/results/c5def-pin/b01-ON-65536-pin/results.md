# c5t6 block pin (2026-11-28T08:03:03Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_condemn_on_cut = "on";destage_workers = 8;; log_level debug
- load average at the start: 1.36 0.72 (1 and 5 min), 16 cpus
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- PASS snapshot pin on commit 17 written
- rep1-w4k: fio rc 0, errors 0, 64714 IOPS, p99 196084 us
- rep2-w4k: fio rc 0, errors 0, 61530 IOPS, p99 217055 us
- rep3-w4k: fio rc 0, errors 0, 60706 IOPS, p99 200278 us
- snapshot pin removed after repetition 3
- rep4-w4k: fio rc 0, errors 0, 60170 IOPS, p99 240124 us
- rep5-w4k: fio rc 0, errors 0, 60968 IOPS, p99 177209 us
- verify-w4k: fio rc 0, errors 0, 109698 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 0)
