# M15 crash points (2026-11-18T06:24:33Z, kernel 7.0.0, 16 cpus): 11 of 66 points, run cap 600 s, budget 17000 s
- hit counts: counted

## crashpoint/store.segment.before_fdatasync/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/first-1/ext4 (crashed after 3s, recovery stall 785 ms)

## crashpoint/store.segment.before_fdatasync/mid-430/ext4
- crashed after 67s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/mid-430/ext4 (crashed after 67s, recovery stall 364 ms)

## crashpoint/store.segment.before_fdatasync/last-688/ext4
- crashed after 95s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/last-688/ext4 (crashed after 95s, recovery stall 544 ms)
- UNREACHED store.set_aside.after_dirsync: the counting run never executed it
- UNREACHED store.set_aside.after_rename: the counting run never executed it
- UNREACHED store.set_aside.before_rename: the counting run never executed it

## crashpoint/store.state.after_dirsync/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.state.after_dirsync/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.state.after_dirsync/mid-39/ext4
- crashed after 48s (status 137)
- PASS crashpoint/store.state.after_dirsync/mid-39/ext4 (crashed after 48s, recovery stall 377 ms)

## crashpoint/store.state.after_dirsync/last-62/ext4
- crashed after 94s (status 137)
- PASS crashpoint/store.state.after_dirsync/last-62/ext4 (crashed after 94s, recovery stall 366 ms)

## crashpoint/store.state.after_fdatasync/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.state.after_fdatasync/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.state.after_fdatasync/mid-39/ext4
- crashed after 47s (status 137)
- PASS crashpoint/store.state.after_fdatasync/mid-39/ext4 (crashed after 47s, recovery stall 368 ms)

## crashpoint/store.state.after_fdatasync/last-62/ext4
- crashed after 95s (status 137)
- PASS crashpoint/store.state.after_fdatasync/last-62/ext4 (crashed after 95s, recovery stall 363 ms)

## crashpoint/store.state.after_rename/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.state.after_rename/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.state.after_rename/mid-39/ext4
- crashed after 47s (status 137)
- PASS crashpoint/store.state.after_rename/mid-39/ext4 (crashed after 47s, recovery stall 356 ms)

## crashpoint/store.state.after_rename/last-62/ext4
- crashed after 90s (status 137)
- PASS crashpoint/store.state.after_rename/last-62/ext4 (crashed after 90s, recovery stall 370 ms)

## crashpoint/store.state.before_fdatasync/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.state.before_fdatasync/first-1/ext4 (crashed after 3s, recovery stall ? ms)

## crashpoint/store.state.before_fdatasync/mid-39/ext4
- crashed after 52s (status 137)
- PASS crashpoint/store.state.before_fdatasync/mid-39/ext4 (crashed after 52s, recovery stall 376 ms)

## crashpoint/store.state.before_fdatasync/last-62/ext4
- crashed after 94s (status 137)
- PASS crashpoint/store.state.before_fdatasync/last-62/ext4 (crashed after 94s, recovery stall 369 ms)

## crashpoint/ublk.after_fetch/first-1/ext4
- crashed after 4s (status 137)
- PASS crashpoint/ublk.after_fetch/first-1/ext4 (crashed after 4s, recovery stall 546 ms)

## crashpoint/ublk.after_fetch/mid-176605/ext4
- crashed after 64s (status 137)
- PASS crashpoint/ublk.after_fetch/mid-176605/ext4 (crashed after 64s, recovery stall 362 ms)

## crashpoint/ublk.after_fetch/last-282567/ext4
- crashed after 92s (status 137)
- PASS crashpoint/ublk.after_fetch/last-282567/ext4 (crashed after 92s, recovery stall 406 ms)
- UNREACHED ublk.after_update_size: the counting run never executed it

## crashpoint/ublk.before_commit/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/ublk.before_commit/first-1/ext4 (crashed after 5s, recovery stall 515 ms)

## crashpoint/ublk.before_commit/mid-176605/ext4
- crashed after 64s (status 137)
- PASS crashpoint/ublk.before_commit/mid-176605/ext4 (crashed after 64s, recovery stall 391 ms)

## crashpoint/ublk.before_commit/last-282567/ext4
- crashed after 93s (status 137)
- PASS crashpoint/ublk.before_commit/last-282567/ext4 (crashed after 93s, recovery stall 413 ms)

## end
overall FAIL=0
