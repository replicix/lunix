# M15 crash points (2026-11-19T20:02:22Z, kernel 7.0.0, 16 cpus): 10 of 79 points, run cap 600 s, budget 9500 s
- hit counts: counted

## crashpoint/store.pack.after_rename/first-1/ext4
- crashed after 4s (status 137)
- PASS crashpoint/store.pack.after_rename/first-1/ext4 (crashed after 4s, recovery stall 364 ms)

## crashpoint/store.pack.after_rename/mid-34/ext4
- crashed after 54s (status 137)
- PASS crashpoint/store.pack.after_rename/mid-34/ext4 (crashed after 54s, recovery stall 404 ms)

## crashpoint/store.pack.after_rename/last-53/ext4
- crashed after 113s (status 137)
- PASS crashpoint/store.pack.after_rename/last-53/ext4 (crashed after 113s, recovery stall 1038 ms)

## crashpoint/store.pack.before_fdatasync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/first-1/ext4 (crashed after 6s, recovery stall 423 ms)

## crashpoint/store.pack.before_fdatasync/mid-34/ext4
- crashed after 62s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/mid-34/ext4 (crashed after 62s, recovery stall 409 ms)

## crashpoint/store.pack.before_fdatasync/last-53/ext4
- crashed after 112s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/last-53/ext4 (crashed after 112s, recovery stall 1359 ms)

## crashpoint/store.segment.after_dirsync/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/store.segment.after_dirsync/first-1/ext4 (crashed after 5s, recovery stall 331 ms)

## crashpoint/store.segment.after_dirsync/mid-419/ext4
- crashed after 69s (status 137)
- PASS crashpoint/store.segment.after_dirsync/mid-419/ext4 (crashed after 69s, recovery stall 504 ms)

## crashpoint/store.segment.after_dirsync/last-670/ext4
- crashed after 105s (status 137)
- PASS crashpoint/store.segment.after_dirsync/last-670/ext4 (crashed after 105s, recovery stall 444 ms)

## crashpoint/store.segment.after_fdatasync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/first-1/ext4 (crashed after 6s, recovery stall 407 ms)

## crashpoint/store.segment.after_fdatasync/mid-419/ext4
- crashed after 67s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/mid-419/ext4 (crashed after 67s, recovery stall 422 ms)

## crashpoint/store.segment.after_fdatasync/last-670/ext4
- crashed after 118s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/last-670/ext4 (crashed after 118s, recovery stall 1063 ms)

## crashpoint/store.segment.after_rename/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/store.segment.after_rename/first-1/ext4 (crashed after 5s, recovery stall 335 ms)

## crashpoint/store.segment.after_rename/mid-419/ext4
- crashed after 65s (status 137)
- PASS crashpoint/store.segment.after_rename/mid-419/ext4 (crashed after 65s, recovery stall 397 ms)

## crashpoint/store.segment.after_rename/last-670/ext4
- crashed after 105s (status 137)
- PASS crashpoint/store.segment.after_rename/last-670/ext4 (crashed after 105s, recovery stall 376 ms)

## crashpoint/store.segment.before_fdatasync/first-1/ext4
- crashed after 8s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/first-1/ext4 (crashed after 8s, recovery stall 1392 ms)

## crashpoint/store.segment.before_fdatasync/mid-419/ext4
- crashed after 72s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/mid-419/ext4 (crashed after 72s, recovery stall 420 ms)

## crashpoint/store.segment.before_fdatasync/last-670/ext4
- crashed after 102s (status 137)
- PASS crashpoint/store.segment.before_fdatasync/last-670/ext4 (crashed after 102s, recovery stall 427 ms)
- UNREACHED store.segment_delete.after_dirsync: the counting run never executed it
- UNREACHED store.segment_delete.after_unlink: the counting run never executed it
- UNREACHED store.segment_delete.before_unlink: the counting run never executed it
- UNREACHED store.set_aside.after_dirsync: the counting run never executed it

## end
overall FAIL=0
