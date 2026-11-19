# M15 crash points (2026-11-19T19:38:19Z, kernel 7.0.0, 16 cpus): 10 of 79 points, run cap 600 s, budget 9500 s
- hit counts: counted

## crashpoint/store.delete.before_unlink/first-1/ext4
- crashed after 79s (status 137)
- PASS crashpoint/store.delete.before_unlink/first-1/ext4 (crashed after 79s, recovery stall 411 ms)

## crashpoint/store.delete.before_unlink/mid-9/ext4
- crashed after 81s (status 137)
- PASS crashpoint/store.delete.before_unlink/mid-9/ext4 (crashed after 81s, recovery stall 418 ms)

## crashpoint/store.delete.before_unlink/last-14/ext4
- crashed after 82s (status 137)
- PASS crashpoint/store.delete.before_unlink/last-14/ext4 (crashed after 82s, recovery stall 395 ms)

## crashpoint/store.evidence.after_dirsync/first-1/ext4
- crashed after 87s (status 137)
- PASS crashpoint/store.evidence.after_dirsync/first-1/ext4 (crashed after 87s, recovery stall 514 ms)

## crashpoint/store.evidence.after_fdatasync/first-1/ext4
- crashed after 88s (status 137)
- PASS crashpoint/store.evidence.after_fdatasync/first-1/ext4 (crashed after 88s, recovery stall 1091 ms)

## crashpoint/store.evidence.after_rename/first-1/ext4
- crashed after 85s (status 137)
- PASS crashpoint/store.evidence.after_rename/first-1/ext4 (crashed after 85s, recovery stall 392 ms)

## crashpoint/store.evidence.before_fdatasync/first-1/ext4
- crashed after 86s (status 137)
- PASS crashpoint/store.evidence.before_fdatasync/first-1/ext4 (crashed after 86s, recovery stall 393 ms)

## crashpoint/store.leftover.after_dirsync/first-1/ext4
- crashed after 4s (status 137)
- PASS crashpoint/store.leftover.after_dirsync/first-1/ext4 (crashed after 4s, recovery stall ? ms)
- UNREACHED store.leftover.after_unlink: the counting run never executed it
- UNREACHED store.leftover.before_unlink: the counting run never executed it

## crashpoint/store.pack.after_dirsync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.after_dirsync/first-1/ext4 (crashed after 6s, recovery stall 349 ms)

## crashpoint/store.pack.after_dirsync/mid-34/ext4
- UNREACHED crashpoint/store.pack.after_dirsync/mid-34/ext4: the workload finished without executing hit 34 of store.pack.after_dirsync

## crashpoint/store.pack.after_dirsync/last-53/ext4
- crashed after 84s (status 137)
- PASS crashpoint/store.pack.after_dirsync/last-53/ext4 (crashed after 84s, recovery stall 513 ms)

## crashpoint/store.pack.after_fdatasync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/first-1/ext4 (crashed after 6s, recovery stall 385 ms)

## crashpoint/store.pack.after_fdatasync/mid-34/ext4
- crashed after 49s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/mid-34/ext4 (crashed after 49s, recovery stall 441 ms)

## crashpoint/store.pack.after_fdatasync/last-53/ext4
- crashed after 90s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/last-53/ext4 (crashed after 90s, recovery stall 478 ms)

## end
overall FAIL=0
