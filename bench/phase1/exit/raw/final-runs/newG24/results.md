# M15 fio verify (2026-11-21T02:34:06Z, kernel 7.0.0, 16 cpus; 1 cells, profiles: 128:8, runtime 120 s)
- script cap 2800 s: run through the wrapper with LUNIX_ROOT_TIMEOUT >= 3400

## cell fio-verify/raw/c64k/integrity
- PASS create volume v64rawintegrity (32 GiB, chunk 65536, integrity)
- PASS attach v64rawintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 128, 8 job(s), 4096 MiB, 120 s
  loops 1 over each job's file, trim companion 120 s, write cap 1800 s
- p0 first pass: written per job (min of 8 jobs) 512/512 MiB min..max 512; total_ios per job 8809,8719,8563,8140,8652,8289,8440,8466
- PASS fio-verify/raw/c64k/integrity p0: every job completed its 1 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c64k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 16 GiB, space used 15 GiB, gc {"state":"compacting","condemned_pending":831,"last_pass":{"pass":11,"dry_run":false,"segments_condemned":62,"segments_deleted":14,"packs_deleted":7,"runs_delet
- PASS fio-verify/raw/c64k/integrity p0: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- journal window after the pre-detach verify_only: hits=358269 misses=0
- PASS detach v64rawintegrity (exit 0)
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_raw_c64k_integrity-1]
- PASS fio-verify/raw/c64k/integrity: attach again
- re-attach: replayed=0, 'journal recovery stopped at a gap' lines in the daemon log: 0, detach exit code 0
- dmesg at the re-mount: 0 line(s) with 'recover' ()
- PASS fio-verify/raw/c64k/integrity p0: verify_only AFTER detach/attach: rc 0, errors 0
- error sets (offsets) pre vs post: p0:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post=
- journal window after the post-detach verify_only: hits=0 misses=0
- PASS fio-verify/raw/c64k/integrity: detach (exit 0)
- PASS fio-verify/raw/c64k/integrity: lunix fsck --deep clean (exit 0)
- PASS fio-verify/raw/c64k/integrity: the store directory stayed under cache_max + reserve + journal ring + 1 GiB (16484 MiB of 26368 MiB)
- PASS fio-verify/raw/c64k/integrity: latency data found in 1 of 1 profile reports
- PASS fio-verify/raw/c64k/integrity: fio's longest completion 14652 ms is under full_timeout (30000 ms)
- PASS fio-verify/raw/c64k/integrity: no write returned ENOSPC
- PASS fio-verify/raw/c64k/integrity: no hung-task line in the kernel log
- PASS fio-verify/raw/c64k/integrity: the gate refused no reservation for space (enospc counter 0)
- fio-verify/raw/c64k/integrity: throttled_ms 0, delayed_writes 0, delay_max_ms 0, cache_max 24G

## end
- PASS every cell produced a result record (1 of 1)
overall FAIL=0
