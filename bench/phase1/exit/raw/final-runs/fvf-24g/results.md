# M15 fio verify (2026-11-20T21:51:16Z, kernel 7.0.0, 16 cpus; 1 cells, profiles: 1:1 32:1 128:1 1:8 32:8 128:8, runtime 600 s)
- script cap 5500 s: run through the wrapper with LUNIX_ROOT_TIMEOUT >= 6100

## cell fio-verify/raw/c64k/integrity
- PASS create volume v64rawintegrity (32 GiB, chunk 65536, integrity)
- PASS attach v64rawintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 19 GiB, space used 18 GiB, gc {"state":"compacting","condemned_pending":510,"last_pass":{"pass":42,"dry_run":false,"segments_condemned":111,"segments_deleted":86,"packs_deleted":18,"runs_del
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 21 GiB, space used 21 GiB, gc {"state":null,"condemned_pending":402,"last_pass":{"pass":91,"dry_run":false,"segments_condemned":56,"segments_deleted":245,"packs_deleted":8,"runs_deleted":9,"
- profile 2: QD 128, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p2: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p2: trimcheck (discard, write-zeroes) rc 0
- p2: store 22 GiB, space used 22 GiB, gc {"state":null,"condemned_pending":220,"last_pass":{"pass":149,"dry_run":false,"segments_condemned":38,"segments_deleted":47,"packs_deleted":7,"runs_deleted":0,"
- profile 3: QD 1, 8 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p3: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p3: trimcheck (discard, write-zeroes) rc 0
- p3: store 23 GiB, space used 23 GiB, gc {"state":null,"condemned_pending":132,"last_pass":{"pass":213,"dry_run":false,"segments_condemned":39,"segments_deleted":13,"packs_deleted":3,"runs_deleted":0,"
- profile 4: QD 32, 8 job(s), 4096 MiB, 600 s
- FAIL fio-verify/raw/c64k/integrity p4: fio rc 9, job errors 252 (zero verify failures)
- FAIL fio-verify/raw/c64k/integrity p4: trimcheck (discard, write-zeroes) rc 2
- PASS detach v64rawintegrity
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_raw_c64k_integrity-1]
- PASS fio-verify/raw/c64k/integrity: the store directory stayed under cache_max + reserve + journal ring + 1 GiB (25114 MiB of 26368 MiB)
- PASS fio-verify/raw/c64k/integrity: fio's longest completion 0 ms is under full_timeout (30000 ms)
- FAIL fio-verify/raw/c64k/integrity: no write returned ENOSPC
- PASS fio-verify/raw/c64k/integrity: no hung-task line in the kernel log
- PASS fio-verify/raw/c64k/integrity: the gate refused no reservation for space (enospc counter )
- fio-verify/raw/c64k/integrity: throttled_ms , delayed_writes , delay_max_ms , cache_max 24G

## end
- PASS every cell produced a result record (1 of 1)
overall FAIL=1
