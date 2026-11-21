# M15 fio verify (2026-11-21T02:40:34Z, kernel 7.0.0, 16 cpus; 3 cells, profiles: 1:1 32:1 128:1 1:8 32:8 128:8, runtime 600 s)
- script cap 19000 s: run through the wrapper with LUNIX_ROOT_TIMEOUT >= 19600

## cell fio-verify/raw/c16k/integrity
- PASS create volume v16rawintegrity (32 GiB, chunk 16384, integrity)
- PASS attach v16rawintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p0 first pass: written per job (min of 1 jobs) 16384/4096 MiB min..max 16384; total_ios per job 278198
- PASS fio-verify/raw/c16k/integrity p0: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 14 GiB, space used 14 GiB, gc {"state":null,"condemned_pending":575,"last_pass":{"pass":40,"dry_run":false,"segments_condemned":98,"segments_deleted":0,"packs_deleted":0,"runs_deleted":0,"co
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
  loops 10 over each job's file, trim companion 600 s, write cap 1800 s
- p1 first pass: written per job (min of 1 jobs) 40960/4096 MiB min..max 40960; total_ios per job 690338
- PASS fio-verify/raw/c16k/integrity p1: every job completed its 10 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 28 GiB, space used 28 GiB, gc {"state":"deleting","condemned_pending":762,"last_pass":{"pass":56,"dry_run":false,"segments_condemned":459,"segments_deleted":417,"packs_deleted":77,"runs_dele
- profile 2: QD 128, 1 job(s), 4096 MiB, 600 s
  loops 9 over each job's file, trim companion 600 s, write cap 1800 s
- p2 first pass: written per job (min of 1 jobs) 36864/4096 MiB min..max 36864; total_ios per job 624372
- PASS fio-verify/raw/c16k/integrity p2: every job completed its 9 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p2: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p2: trimcheck (discard, write-zeroes) rc 0
- p2: store 32 GiB, space used 32 GiB, gc {"state":"compacting","condemned_pending":907,"last_pass":{"pass":62,"dry_run":false,"segments_condemned":1012,"segments_deleted":449,"packs_deleted":123,"runs_
- profile 3: QD 1, 8 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p3 first pass: written per job (min of 8 jobs) 2048/512 MiB min..max 2048; total_ios per job 34643,34247,35941,35041,35546,35846,35140,34084
- PASS fio-verify/raw/c16k/integrity p3: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p3: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p3: trimcheck (discard, write-zeroes) rc 0
- p3: store 45 GiB, space used 44 GiB, gc {"state":null,"condemned_pending":1212,"last_pass":{"pass":67,"dry_run":false,"segments_condemned":811,"segments_deleted":298,"packs_deleted":52,"runs_deleted":
- profile 4: QD 32, 8 job(s), 4096 MiB, 600 s
  loops 2 over each job's file, trim companion 600 s, write cap 1800 s
- p4 first pass: written per job (min of 8 jobs) 1024/512 MiB min..max 1024; total_ios per job 17156,17007,16514,17524,17009,17978,16302,17172
- PASS fio-verify/raw/c16k/integrity p4: every job completed its 2 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p4: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p4: trimcheck (discard, write-zeroes) rc 0
- p4: store 47 GiB, space used 47 GiB, gc {"state":"marking","condemned_pending":1090,"last_pass":{"pass":70,"dry_run":false,"segments_condemned":748,"segments_deleted":262,"packs_deleted":137,"runs_del
- profile 5: QD 128, 8 job(s), 4096 MiB, 600 s
  loops 1 over each job's file, trim companion 600 s, write cap 1800 s
- p5 first pass: written per job (min of 8 jobs) 512/512 MiB min..max 512; total_ios per job 8801,7694,8355,8836,8350,8639,8607,8886
- PASS fio-verify/raw/c16k/integrity p5: every job completed its 1 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/raw/c16k/integrity p5: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/raw/c16k/integrity p5: trimcheck (discard, write-zeroes) rc 0
- p5: store 47 GiB, space used 46 GiB, gc {"state":null,"condemned_pending":719,"last_pass":{"pass":73,"dry_run":false,"segments_condemned":432,"segments_deleted":344,"packs_deleted":139,"runs_deleted":
- PASS fio-verify/raw/c16k/integrity p0: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p1: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p2: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p3: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p4: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p5: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- journal window after the pre-detach verify_only: hits=17352329 misses=0
- PASS detach v16rawintegrity (exit 0)
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_raw_c16k_integrity-1]
- PASS fio-verify/raw/c16k/integrity: attach again
- re-attach: replayed=0, 'journal recovery stopped at a gap' lines in the daemon log: 0, detach exit code 0
- dmesg at the re-mount: 0 line(s) with 'recover' ()
- PASS fio-verify/raw/c16k/integrity p0: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p1: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p2: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p3: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p4: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/raw/c16k/integrity p5: verify_only AFTER detach/attach: rc 0, errors 0
- error sets (offsets) pre vs post: p0:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post= p1:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post= p2:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post= p3:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post= p4:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post= p5:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=,in_unwritten_post=
- journal window after the post-detach verify_only: hits=0 misses=0
- PASS fio-verify/raw/c16k/integrity: detach (exit 0)
- PASS fio-verify/raw/c16k/integrity: lunix fsck --deep clean (exit 0)
- PASS fio-verify/raw/c16k/integrity: the store directory stayed under cache_max + reserve + journal ring + 1 GiB (49003 MiB of 50944 MiB)
- PASS fio-verify/raw/c16k/integrity: latency data found in 6 of 6 profile reports
- FAIL fio-verify/raw/c16k/integrity: fio's longest completion 67024 ms is under full_timeout (30000 ms)
- PASS fio-verify/raw/c16k/integrity: no write returned ENOSPC
- PASS fio-verify/raw/c16k/integrity: no hung-task line in the kernel log
- PASS fio-verify/raw/c16k/integrity: the gate refused no reservation for space (enospc counter 0)
- fio-verify/raw/c16k/integrity: throttled_ms 328090, delayed_writes 0, delay_max_ms 0, cache_max 48G

## cell fio-verify/ext4/c16k/integrity
- PASS create volume v16ext4integrity (32 GiB, chunk 16384, integrity)
- PASS attach v16ext4integrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p0 first pass: written per job (min of 1 jobs) 16384/4096 MiB min..max 16384; total_ios per job 278198
- PASS fio-verify/ext4/c16k/integrity p0: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 20 GiB, space used 19 GiB, gc {"state":"deleting","condemned_pending":1015,"last_pass":{"pass":33,"dry_run":false,"segments_condemned":98,"segments_deleted":114,"packs_deleted":19,"runs_dele
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
  loops 10 over each job's file, trim companion 600 s, write cap 1800 s
- p1 first pass: written per job (min of 1 jobs) 40960/4096 MiB min..max 40960; total_ios per job 690338
- PASS fio-verify/ext4/c16k/integrity p1: every job completed its 10 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 31 GiB, space used 30 GiB, gc {"state":"marking","condemned_pending":688,"last_pass":{"pass":43,"dry_run":false,"segments_condemned":477,"segments_deleted":536,"packs_deleted":121,"runs_dele
- profile 2: QD 128, 1 job(s), 4096 MiB, 600 s
  loops 9 over each job's file, trim companion 600 s, write cap 1800 s
- p2 first pass: written per job (min of 1 jobs) 36864/4096 MiB min..max 36864; total_ios per job 624372
- PASS fio-verify/ext4/c16k/integrity p2: every job completed its 9 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p2: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p2: trimcheck (discard, write-zeroes) rc 0
- p2: store 44 GiB, space used 43 GiB, gc {"state":null,"condemned_pending":1105,"last_pass":{"pass":49,"dry_run":false,"segments_condemned":782,"segments_deleted":528,"packs_deleted":131,"runs_deleted"
- profile 3: QD 1, 8 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p3 first pass: written per job (min of 8 jobs) 2048/512 MiB min..max 2048; total_ios per job 34643,34247,35941,35041,35546,35846,35140,34084
- PASS fio-verify/ext4/c16k/integrity p3: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p3: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p3: trimcheck (discard, write-zeroes) rc 0
- p3: store 44 GiB, space used 44 GiB, gc {"state":null,"condemned_pending":503,"last_pass":{"pass":53,"dry_run":false,"segments_condemned":365,"segments_deleted":552,"packs_deleted":123,"runs_deleted":
- profile 4: QD 32, 8 job(s), 4096 MiB, 600 s
  loops 2 over each job's file, trim companion 600 s, write cap 1800 s
- p4 first pass: written per job (min of 8 jobs) 1024/512 MiB min..max 1024; total_ios per job 17156,17007,16514,17524,17009,17978,16302,17172
- PASS fio-verify/ext4/c16k/integrity p4: every job completed its 2 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p4: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p4: trimcheck (discard, write-zeroes) rc 0
- p4: store 47 GiB, space used 47 GiB, gc {"state":null,"condemned_pending":560,"last_pass":{"pass":56,"dry_run":false,"segments_condemned":350,"segments_deleted":205,"packs_deleted":67,"runs_deleted":9
- profile 5: QD 128, 8 job(s), 4096 MiB, 600 s
  loops 1 over each job's file, trim companion 600 s, write cap 1800 s
- p5 first pass: written per job (min of 8 jobs) 512/512 MiB min..max 512; total_ios per job 8801,7694,8355,8836,8350,8639,8607,8886
- PASS fio-verify/ext4/c16k/integrity p5: every job completed its 1 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/ext4/c16k/integrity p5: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/ext4/c16k/integrity p5: trimcheck (discard, write-zeroes) rc 0
- p5: store 47 GiB, space used 47 GiB, gc {"state":null,"condemned_pending":260,"last_pass":{"pass":59,"dry_run":false,"segments_condemned":127,"segments_deleted":156,"packs_deleted":56,"runs_deleted":2
- audit pre p0: snap 1 files, 1048576 blocks, 6 extents
- audit pre p1: snap 1 files, 1048576 blocks, 7 extents
- audit pre p2: snap 1 files, 1048576 blocks, 3 extents
- audit pre p3: snap 8 files, 1048576 blocks, 10 extents
- audit pre p4: snap 8 files, 1048576 blocks, 12 extents
- audit pre p5: snap 8 files, 1048576 blocks, 611 extents
- PASS fio-verify/ext4/c16k/integrity p0: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p1: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p2: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p3: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p4: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p5: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- journal window after the pre-detach verify_only: hits=16369181 misses=0
- PASS fio-verify/ext4/c16k/integrity: guest fsck clean after the runs
- PASS detach v16ext4integrity (exit 0)
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_ext4_c16k_integrity-1]
- PASS fio-verify/ext4/c16k/integrity: attach again
- re-attach: replayed=0, 'journal recovery stopped at a gap' lines in the daemon log: 0, detach exit code 0
- audit post p0: snap 1 files, 1048576 blocks, 133 extents
- audit post p1: snap 1 files, 1048576 blocks, 150 extents
- audit post p2: snap 1 files, 1048576 blocks, 143 extents
- audit post p3: snap 8 files, 1048576 blocks, 157 extents
- audit post p4: snap 8 files, 1048576 blocks, 135 extents
- audit post p5: snap 8 files, 1048576 blocks, 736 extents
- audit cmp p0 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=6 extents_post=133 unwritten_blocks_pre=0 post=0
- audit cmp p1 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=7 extents_post=150 unwritten_blocks_pre=0 post=0
- audit cmp p2 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=3 extents_post=143 unwritten_blocks_pre=0 post=0
- audit cmp p3 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=10 extents_post=157 unwritten_blocks_pre=0 post=0
- audit cmp p4 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=12 extents_post=135 unwritten_blocks_pre=0 post=0
- audit cmp p5 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=611 extents_post=736 unwritten_blocks_pre=0 post=0
- dmesg at the re-mount: 0 line(s) with 'recover' ()
- PASS fio-verify/ext4/c16k/integrity p0: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p1: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p2: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p3: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p4: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/ext4/c16k/integrity p5: verify_only AFTER detach/attach: rc 0, errors 0
- error sets (offsets) pre vs post: p0:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p1:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p2:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p3:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p4:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p5:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0
- journal window after the post-detach verify_only: hits=5 misses=0
- PASS fio-verify/ext4/c16k/integrity: guest fsck clean after verify_only
- PASS fio-verify/ext4/c16k/integrity: detach (exit 0)
- PASS fio-verify/ext4/c16k/integrity: lunix fsck --deep clean (exit 0)
- PASS fio-verify/ext4/c16k/integrity: the store directory stayed under cache_max + reserve + journal ring + 1 GiB (49385 MiB of 50944 MiB)
- PASS fio-verify/ext4/c16k/integrity: latency data found in 6 of 6 profile reports
- FAIL fio-verify/ext4/c16k/integrity: fio's longest completion 80483 ms is under full_timeout (30000 ms)
- PASS fio-verify/ext4/c16k/integrity: no write returned ENOSPC
- PASS fio-verify/ext4/c16k/integrity: no hung-task line in the kernel log
- PASS fio-verify/ext4/c16k/integrity: the gate refused no reservation for space (enospc counter 0)
- fio-verify/ext4/c16k/integrity: throttled_ms 412808, delayed_writes 0, delay_max_ms 0, cache_max 48G

## cell fio-verify/xfs/c16k/integrity
- PASS create volume v16xfsintegrity (32 GiB, chunk 16384, integrity)
- PASS attach v16xfsintegrity (4 queues)
- device /dev/ublkb0
- profile 0: QD 1, 1 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p0 first pass: written per job (min of 1 jobs) 16384/4096 MiB min..max 16384; total_ios per job 278198
- PASS fio-verify/xfs/c16k/integrity p0: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p0: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p0: trimcheck (discard, write-zeroes) rc 0
- p0: store 18 GiB, space used 18 GiB, gc {"state":"deleting","condemned_pending":725,"last_pass":{"pass":33,"dry_run":false,"segments_condemned":98,"segments_deleted":134,"packs_deleted":17,"runs_delet
- profile 1: QD 32, 1 job(s), 4096 MiB, 600 s
  loops 10 over each job's file, trim companion 600 s, write cap 1800 s
- p1 first pass: written per job (min of 1 jobs) 40960/4096 MiB min..max 40960; total_ios per job 690338
- PASS fio-verify/xfs/c16k/integrity p1: every job completed its 10 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p1: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p1: trimcheck (discard, write-zeroes) rc 0
- p1: store 34 GiB, space used 33 GiB, gc {"state":null,"condemned_pending":752,"last_pass":{"pass":43,"dry_run":false,"segments_condemned":580,"segments_deleted":789,"packs_deleted":203,"runs_deleted":
- profile 2: QD 128, 1 job(s), 4096 MiB, 600 s
  loops 9 over each job's file, trim companion 600 s, write cap 1800 s
- p2 first pass: written per job (min of 1 jobs) 36864/4096 MiB min..max 36864; total_ios per job 624372
- PASS fio-verify/xfs/c16k/integrity p2: every job completed its 9 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p2: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p2: trimcheck (discard, write-zeroes) rc 0
- p2: store 46 GiB, space used 45 GiB, gc {"state":null,"condemned_pending":708,"last_pass":{"pass":48,"dry_run":false,"segments_condemned":450,"segments_deleted":739,"packs_deleted":115,"runs_deleted":
- profile 3: QD 1, 8 job(s), 4096 MiB, 600 s
  loops 4 over each job's file, trim companion 600 s, write cap 1800 s
- p3 first pass: written per job (min of 8 jobs) 2048/512 MiB min..max 2048; total_ios per job 34643,34247,35941,35041,35546,35846,35140,34084
- PASS fio-verify/xfs/c16k/integrity p3: every job completed its 4 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p3: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p3: trimcheck (discard, write-zeroes) rc 0
- p3: store 46 GiB, space used 45 GiB, gc {"state":null,"condemned_pending":584,"last_pass":{"pass":53,"dry_run":false,"segments_condemned":404,"segments_deleted":301,"packs_deleted":83,"runs_deleted":2
- profile 4: QD 32, 8 job(s), 4096 MiB, 600 s
  loops 2 over each job's file, trim companion 600 s, write cap 1800 s
- p4 first pass: written per job (min of 8 jobs) 1024/512 MiB min..max 1024; total_ios per job 17156,17007,16514,17524,17009,17978,16302,17172
- PASS fio-verify/xfs/c16k/integrity p4: every job completed its 2 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p4: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p4: trimcheck (discard, write-zeroes) rc 0
- p4: store 47 GiB, space used 47 GiB, gc {"state":null,"condemned_pending":288,"last_pass":{"pass":56,"dry_run":false,"segments_condemned":117,"segments_deleted":319,"packs_deleted":73,"runs_deleted":2
- profile 5: QD 128, 8 job(s), 4096 MiB, 600 s
  loops 1 over each job's file, trim companion 600 s, write cap 1800 s
- p5 first pass: written per job (min of 8 jobs) 512/512 MiB min..max 512; total_ios per job 8801,7694,8355,8836,8350,8639,8607,8886
- PASS fio-verify/xfs/c16k/integrity p5: every job completed its 1 loop(s) (write run complete; fio rc 0)
- PASS fio-verify/xfs/c16k/integrity p5: fio rc 0, job errors 0 (zero verify failures)
- PASS fio-verify/xfs/c16k/integrity p5: trimcheck (discard, write-zeroes) rc 0
- p5: store 47 GiB, space used 47 GiB, gc {"state":null,"condemned_pending":212,"last_pass":{"pass":59,"dry_run":false,"segments_condemned":38,"segments_deleted":157,"packs_deleted":59,"runs_deleted":24
- audit pre p0: snap 1 files, 1048576 blocks, 2 extents
- audit pre p1: snap 1 files, 1048576 blocks, 2 extents
- audit pre p2: snap 1 files, 1048576 blocks, 2 extents
- audit pre p3: snap 8 files, 1048576 blocks, 8 extents
- audit pre p4: snap 8 files, 1048576 blocks, 8 extents
- audit pre p5: snap 8 files, 1048576 blocks, 7642 extents
- PASS fio-verify/xfs/c16k/integrity p0: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p1: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p2: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p3: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p4: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p5: verify_only BEFORE the detach (same attach, caches dropped): rc 0, errors 0
- journal window after the pre-detach verify_only: hits=16521452 misses=0
- PASS fio-verify/xfs/c16k/integrity: guest fsck clean after the runs
- PASS detach v16xfsintegrity (exit 0)
- PASS lunix fsck --deep clean after detach (exit 0) [fio-verify_xfs_c16k_integrity-1]
- PASS fio-verify/xfs/c16k/integrity: attach again
- re-attach: replayed=0, 'journal recovery stopped at a gap' lines in the daemon log: 0, detach exit code 0
- audit post p0: snap 1 files, 1048576 blocks, 2 extents
- audit post p1: snap 1 files, 1048576 blocks, 2 extents
- audit post p2: snap 1 files, 1048576 blocks, 2 extents
- audit post p3: snap 8 files, 1048576 blocks, 8 extents
- audit post p4: snap 8 files, 1048576 blocks, 8 extents
- audit post p5: snap 8 files, 1048576 blocks, 7642 extents
- audit cmp p0 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=2 extents_post=2 unwritten_blocks_pre=0 post=0
- audit cmp p1 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=2 extents_post=2 unwritten_blocks_pre=0 post=0
- audit cmp p2 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=2 extents_post=2 unwritten_blocks_pre=0 post=0
- audit cmp p3 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=8 extents_post=8 unwritten_blocks_pre=0 post=0
- audit cmp p4 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=8 extents_post=8 unwritten_blocks_pre=0 post=0
- audit cmp p5 (before the detach vs after the attach): content_changed=0 written_to_unwritten=0 unwritten_to_written=0 hole_changes=0 extents_pre=7642 extents_post=7642 unwritten_blocks_pre=0 post=0
- dmesg at the re-mount: 0 line(s) with 'recover' ()
- PASS fio-verify/xfs/c16k/integrity p0: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p1: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p2: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p3: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p4: verify_only AFTER detach/attach: rc 0, errors 0
- PASS fio-verify/xfs/c16k/integrity p5: verify_only AFTER detach/attach: rc 0, errors 0
- error sets (offsets) pre vs post: p0:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p1:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p2:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p3:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p4:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0 p5:pre=0,post=0,only_pre=0,only_post=0,in_unwritten_pre=0,in_unwritten_post=0
- journal window after the post-detach verify_only: hits=128 misses=0
- PASS fio-verify/xfs/c16k/integrity: guest fsck clean after verify_only
- PASS fio-verify/xfs/c16k/integrity: detach (exit 0)
- PASS fio-verify/xfs/c16k/integrity: lunix fsck --deep clean (exit 0)
- PASS fio-verify/xfs/c16k/integrity: the store directory stayed under cache_max + reserve + journal ring + 1 GiB (49428 MiB of 50944 MiB)
- PASS fio-verify/xfs/c16k/integrity: latency data found in 6 of 6 profile reports
- FAIL fio-verify/xfs/c16k/integrity: fio's longest completion 102403 ms is under full_timeout (30000 ms)
- PASS fio-verify/xfs/c16k/integrity: no write returned ENOSPC
- PASS fio-verify/xfs/c16k/integrity: no hung-task line in the kernel log
- PASS fio-verify/xfs/c16k/integrity: the gate refused no reservation for space (enospc counter 0)
- fio-verify/xfs/c16k/integrity: throttled_ms 404255, delayed_writes 1, delay_max_ms 95, cache_max 48G

## end
- PASS every cell produced a result record (3 of 3)
overall FAIL=1
