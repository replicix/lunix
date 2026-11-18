# M15 fio verify (2026-11-18T08:17:36Z, kernel 7.0.0, 16 cpus; 3 cells, profiles: 1:1 32:1 128:1 1:8 32:8 128:8, runtime 600 s)
- script cap 19000 s: run through the wrapper with LUNIX_ROOT_TIMEOUT >= 19600

## cell fio-verify/raw/c64k/integrity
- PASS create volume v64rawintegrity (32 GiB, chunk 65536, integrity)
- PASS attach v64rawintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 28 GiB, space used 28 GiB, gc {"state":"deleting","condemned_pending":621,"last_pass":{"pass":2,"dry_run":false,"segments_condemned":290,"segments_deleted":290,"packs_deleted":82,"runs_delet
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/raw/c64k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c64k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 81 GiB, space used 81 GiB, gc {"state":"condemning","condemned_pending":5050,"last_pass":{"pass":3,"dry_run":false,"segments_condemned":663,"segments_deleted":663,"packs_deleted":187,"runs_d
- FAIL fio-verify/raw/c64k/integrity p1: the store is above 80 GiB (GC is not keeping up)
- PASS detach v64rawintegrity
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_raw_c64k_integrity-1]

## cell fio-verify/ext4/c64k/integrity
- PASS create volume v64ext4integrity (32 GiB, chunk 65536, integrity)
- PASS attach v64ext4integrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/ext4/c64k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c64k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 46 GiB, space used 46 GiB, gc {"state":"deleting","condemned_pending":821,"last_pass":{"pass":1,"dry_run":false,"segments_condemned":0,"segments_deleted":0,"packs_deleted":14,"runs_deleted":
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/ext4/c64k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c64k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 86 GiB, space used 86 GiB, gc {"state":"compacting","condemned_pending":0,"last_pass":{"pass":1,"dry_run":false,"segments_condemned":0,"segments_deleted":0,"packs_deleted":14,"runs_deleted":
- FAIL fio-verify/ext4/c64k/integrity p1: the store is above 80 GiB (GC is not keeping up)
- PASS fio-verify/ext4/c64k/integrity: guest fsck clean after the runs
- PASS detach v64ext4integrity
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_ext4_c64k_integrity-1]

## cell fio-verify/xfs/c64k/integrity
- PASS create volume v64xfsintegrity (32 GiB, chunk 65536, integrity)
- PASS attach v64xfsintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/xfs/c64k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c64k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 56 GiB, space used 55 GiB, gc {"state":null,"condemned_pending":0,"last_pass":{"pass":1,"dry_run":false,"segments_condemned":1,"segments_deleted":1,"packs_deleted":9,"runs_deleted":9,"commit
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
- PASS fio-verify/xfs/c64k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c64k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 107 GiB, space used 106 GiB, gc {"state":"condemning","condemned_pending":3000,"last_pass":{"pass":1,"dry_run":false,"segments_condemned":1,"segments_deleted":1,"packs_deleted":9,"runs_deleted
- FAIL fio-verify/xfs/c64k/integrity p1: the store is above 80 GiB (GC is not keeping up)
- PASS fio-verify/xfs/c64k/integrity: guest fsck clean after the runs
- PASS detach v64xfsintegrity
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_xfs_c64k_integrity-1]

## end
- PASS every cell produced a result record (3 of 3)
overall FAIL=1
