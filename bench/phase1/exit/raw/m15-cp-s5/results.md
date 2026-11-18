# M15 crash points (2026-11-18T05:49:04Z, kernel 7.0.0, 16 cpus): 11 of 66 points, run cap 600 s, budget 17000 s
- hit counts: counted
- UNREACHED store.evidence.before_fdatasync: the counting run never executed it

## crashpoint/store.leftover.after_dirsync/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.leftover.after_dirsync/first-1/ext4 (crashed after 3s, recovery stall ? ms)
- UNREACHED store.leftover.after_unlink: the counting run never executed it
- UNREACHED store.leftover.before_unlink: the counting run never executed it

## crashpoint/store.pack.after_dirsync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.after_dirsync/first-1/ext4 (crashed after 6s, recovery stall 1125 ms)

## crashpoint/store.pack.after_dirsync/mid-36/ext4
- crashed after 81s (status 137)
- PASS crashpoint/store.pack.after_dirsync/mid-36/ext4 (crashed after 81s, recovery stall 2066 ms)

## crashpoint/store.pack.after_dirsync/last-57/ext4
- UNREACHED crashpoint/store.pack.after_dirsync/last-57/ext4: the workload finished without executing hit 57 of store.pack.after_dirsync
## crashpoint/store.pack.after_dirsync/last60-42/ext4 (last run did not reach the count)
- crashed after 105s (status 137)
- PASS crashpoint/store.pack.after_dirsync/last60-42/ext4 (crashed after 105s, recovery stall 985 ms)

## crashpoint/store.pack.after_fdatasync/first-1/ext4
- crashed after 9s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/first-1/ext4 (crashed after 9s, recovery stall 2113 ms)

## crashpoint/store.pack.after_fdatasync/mid-36/ext4
- crashed after 58s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/mid-36/ext4 (crashed after 58s, recovery stall 399 ms)

## crashpoint/store.pack.after_fdatasync/last-57/ext4
- crashed after 97s (status 137)
- PASS crashpoint/store.pack.after_fdatasync/last-57/ext4 (crashed after 97s, recovery stall 439 ms)

## crashpoint/store.pack.after_rename/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.after_rename/first-1/ext4 (crashed after 6s, recovery stall 367 ms)

## crashpoint/store.pack.after_rename/mid-36/ext4
- crashed after 55s (status 137)
- PASS crashpoint/store.pack.after_rename/mid-36/ext4 (crashed after 55s, recovery stall 412 ms)

## crashpoint/store.pack.after_rename/last-57/ext4
- crashed after 97s (status 137)
- PASS crashpoint/store.pack.after_rename/last-57/ext4 (crashed after 97s, recovery stall 474 ms)

## crashpoint/store.pack.before_fdatasync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/first-1/ext4 (crashed after 6s, recovery stall 393 ms)

## crashpoint/store.pack.before_fdatasync/mid-36/ext4
- crashed after 56s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/mid-36/ext4 (crashed after 56s, recovery stall 411 ms)

## crashpoint/store.pack.before_fdatasync/last-57/ext4
- crashed after 92s (status 137)
- PASS crashpoint/store.pack.before_fdatasync/last-57/ext4 (crashed after 92s, recovery stall 437 ms)

## crashpoint/store.segment.after_dirsync/first-1/ext4
- crashed after 6s (status 137)
- PASS crashpoint/store.segment.after_dirsync/first-1/ext4 (crashed after 6s, recovery stall 338 ms)

## crashpoint/store.segment.after_dirsync/mid-430/ext4
- crashed after 68s (status 137)
- PASS crashpoint/store.segment.after_dirsync/mid-430/ext4 (crashed after 68s, recovery stall 679 ms)

## crashpoint/store.segment.after_dirsync/last-688/ext4
- crashed after 102s (status 137)
- PASS crashpoint/store.segment.after_dirsync/last-688/ext4 (crashed after 102s, recovery stall 383 ms)

## crashpoint/store.segment.after_fdatasync/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/first-1/ext4 (crashed after 5s, recovery stall 358 ms)

## crashpoint/store.segment.after_fdatasync/mid-430/ext4
- crashed after 71s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/mid-430/ext4 (crashed after 71s, recovery stall 349 ms)

## crashpoint/store.segment.after_fdatasync/last-688/ext4
- crashed after 97s (status 137)
- PASS crashpoint/store.segment.after_fdatasync/last-688/ext4 (crashed after 97s, recovery stall 1100 ms)

## crashpoint/store.segment.after_rename/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/store.segment.after_rename/first-1/ext4 (crashed after 5s, recovery stall 334 ms)

## crashpoint/store.segment.after_rename/mid-430/ext4
- crashed after 64s (status 137)
- PASS crashpoint/store.segment.after_rename/mid-430/ext4 (crashed after 64s, recovery stall 405 ms)

## crashpoint/store.segment.after_rename/last-688/ext4
- crashed after 99s (status 137)
- PASS crashpoint/store.segment.after_rename/last-688/ext4 (crashed after 99s, recovery stall 364 ms)

## end
overall FAIL=0
