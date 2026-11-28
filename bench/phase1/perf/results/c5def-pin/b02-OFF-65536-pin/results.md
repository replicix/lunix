# c5t6 block pin (2026-11-28T08:10:20Z, kernel 7.0.0, 16 cpus): chunk 65536, 5 reps, restart rep 0
- daemon settings: throttle_pace_store_bytes = true;gc_condemn_on_cut = "off";destage_workers = 8;; log_level debug
- load average at the start: 116.06 253.11 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 253.11 is above 2: the host is not quiet
- governor: not exposed (no cpufreq on this machine), nothing set
- PASS create c5t6
- PASS attach with 4 queues
- PASS write the 2 GiB working set
- PASS snapshot pin on commit 17 written
- rep1-w4k: fio rc 0, errors 0, 61195 IOPS, p99 225444 us
- rep2-w4k: fio rc 0, errors 0, 64272 IOPS, p99 164626 us
- rep3-w4k: fio rc 0, errors 0, 57499 IOPS, p99 261095 us
- snapshot pin removed after repetition 3
- rep4-w4k: fio rc 0, errors 0, 59467 IOPS, p99 204472 us
- rep5-w4k: fio rc 0, errors 0, 64317 IOPS, p99 168821 us
- verify-w4k: fio rc 0, errors 0, 120560 IOPS
- PASS fio verify clean (rc 0, errors 0)
- PASS detach
- PASS lunix fsck --deep clean (exit 0)

reference table log lines: 0
EIO/ENOSPC log lines: 0
overall FAIL=0 (noisy host: 1)
