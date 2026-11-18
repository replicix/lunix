# M15 crash points (2026-11-18T05:03:58Z, kernel 7.0.0, 16 cpus): 11 of 66 points, run cap 600 s, budget 17000 s
- hit counts: counted

## crashpoint/store.checkpoint.before_write/first-1/ext4
- crashed after 4s (status 137)
- PASS crashpoint/store.checkpoint.before_write/first-1/ext4 (crashed after 4s, recovery stall 1226 ms)

## crashpoint/store.checkpoint.before_write/mid-37/ext4
- crashed after 71s (status 137)
- PASS crashpoint/store.checkpoint.before_write/mid-37/ext4 (crashed after 71s, recovery stall 387 ms)

## crashpoint/store.checkpoint.before_write/last-58/ext4
- crashed after 112s (status 137)
- PASS crashpoint/store.checkpoint.before_write/last-58/ext4 (crashed after 112s, recovery stall 695 ms)

## crashpoint/store.commit.after_dirsync/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.commit.after_dirsync/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.commit.after_dirsync/mid-37/ext4
- crashed after 60s (status 137)
- PASS crashpoint/store.commit.after_dirsync/mid-37/ext4 (crashed after 60s, recovery stall 693 ms)

## crashpoint/store.commit.after_dirsync/last-59/ext4
- crashed after 103s (status 137)
- PASS crashpoint/store.commit.after_dirsync/last-59/ext4 (crashed after 103s, recovery stall 538 ms)

## crashpoint/store.commit.after_fdatasync/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.commit.after_fdatasync/first-1/ext4 (crashed after 3s, recovery stall ? ms)

## crashpoint/store.commit.after_fdatasync/mid-37/ext4
- crashed after 62s (status 137)
- PASS crashpoint/store.commit.after_fdatasync/mid-37/ext4 (crashed after 62s, recovery stall 1284 ms)

## crashpoint/store.commit.after_fdatasync/last-59/ext4
- crashed after 109s (status 137)
- PASS crashpoint/store.commit.after_fdatasync/last-59/ext4 (crashed after 109s, recovery stall 409 ms)

## crashpoint/store.commit.after_rename/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.commit.after_rename/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.commit.after_rename/mid-37/ext4
- crashed after 62s (status 137)
- PASS crashpoint/store.commit.after_rename/mid-37/ext4 (crashed after 62s, recovery stall 1784 ms)

## crashpoint/store.commit.after_rename/last-59/ext4
- crashed after 99s (status 137)
- PASS crashpoint/store.commit.after_rename/last-59/ext4 (crashed after 99s, recovery stall 364 ms)

## crashpoint/store.commit.before_fdatasync/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.commit.before_fdatasync/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.commit.before_fdatasync/mid-37/ext4
- crashed after 58s (status 137)
- PASS crashpoint/store.commit.before_fdatasync/mid-37/ext4 (crashed after 58s, recovery stall 394 ms)

## crashpoint/store.commit.before_fdatasync/last-59/ext4
- UNREACHED crashpoint/store.commit.before_fdatasync/last-59/ext4: the workload finished without executing hit 59 of store.commit.before_fdatasync
## crashpoint/store.commit.before_fdatasync/last60-44/ext4 (last run did not reach the count)
- crashed after 102s (status 137)
- PASS crashpoint/store.commit.before_fdatasync/last60-44/ext4 (crashed after 102s, recovery stall 1501 ms)

## crashpoint/store.delete.after_dirsync/first-1/ext4
- FAIL crashpoint/store.delete.after_dirsync/first-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.after_dirsync/mid-1/ext4
- FAIL crashpoint/store.delete.after_dirsync/mid-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.after_dirsync/last-0/ext4
- UNREACHED crashpoint/store.delete.after_dirsync/last-0/ext4: the workload finished without executing hit 0 of store.delete.after_dirsync

## crashpoint/store.delete.after_unlink/first-1/ext4
- FAIL crashpoint/store.delete.after_unlink/first-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.after_unlink/mid-1/ext4
- FAIL crashpoint/store.delete.after_unlink/mid-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.after_unlink/last-0/ext4
- UNREACHED crashpoint/store.delete.after_unlink/last-0/ext4: the workload finished without executing hit 0 of store.delete.after_unlink

## crashpoint/store.delete.before_unlink/first-1/ext4
- FAIL crashpoint/store.delete.before_unlink/first-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.before_unlink/mid-1/ext4
- FAIL crashpoint/store.delete.before_unlink/mid-1/ext4 (fio rc 124 errors 0, guest fsck 0, fsyncer 0, lunix fsck 0;)

## crashpoint/store.delete.before_unlink/last-0/ext4
- UNREACHED crashpoint/store.delete.before_unlink/last-0/ext4: the workload finished without executing hit 0 of store.delete.before_unlink
- UNREACHED store.evidence.after_dirsync: the counting run never executed it
- UNREACHED store.evidence.after_fdatasync: the counting run never executed it
- UNREACHED store.evidence.after_rename: the counting run never executed it

## end
overall FAIL=1
