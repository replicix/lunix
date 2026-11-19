# M15 crash points (2026-11-19T20:31:28Z, kernel 7.0.0, 16 cpus): 9 of 79 points, run cap 600 s, budget 9500 s
- hit counts: counted
- UNREACHED store.set_aside.after_rename: the counting run never executed it
- UNREACHED store.set_aside.before_rename: the counting run never executed it

## crashpoint/store.state.after_dirsync/first-1/ext4
- crashed after 1s (status 137)
- PASS crashpoint/store.state.after_dirsync/first-1/ext4 (crashed after 1s, recovery stall ? ms)

## crashpoint/store.state.after_dirsync/mid-39/ext4
- crashed after 50s (status 137)
- PASS crashpoint/store.state.after_dirsync/mid-39/ext4 (crashed after 50s, recovery stall 638 ms)

## crashpoint/store.state.after_dirsync/last-61/ext4
- crashed after 95s (status 137)
- PASS crashpoint/store.state.after_dirsync/last-61/ext4 (crashed after 95s, recovery stall 834 ms)

## crashpoint/store.state.after_fdatasync/first-1/ext4
- crashed after 2s (status 137)
- PASS crashpoint/store.state.after_fdatasync/first-1/ext4 (crashed after 2s, recovery stall ? ms)

## crashpoint/store.state.after_fdatasync/mid-39/ext4
- crashed after 65s (status 137)
- PASS crashpoint/store.state.after_fdatasync/mid-39/ext4 (crashed after 65s, recovery stall 354 ms)

## crashpoint/store.state.after_fdatasync/last-61/ext4
- crashed after 94s (status 137)
- PASS crashpoint/store.state.after_fdatasync/last-61/ext4 (crashed after 94s, recovery stall 352 ms)

## crashpoint/store.state.after_rename/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.state.after_rename/first-1/ext4 (crashed after 3s, recovery stall ? ms)

## crashpoint/store.state.after_rename/mid-39/ext4
- crashed after 52s (status 137)
- PASS crashpoint/store.state.after_rename/mid-39/ext4 (crashed after 52s, recovery stall 336 ms)

## crashpoint/store.state.after_rename/last-61/ext4
- crashed after 94s (status 137)
- PASS crashpoint/store.state.after_rename/last-61/ext4 (crashed after 94s, recovery stall 394 ms)

## crashpoint/store.state.before_fdatasync/first-1/ext4
- crashed after 3s (status 137)
- PASS crashpoint/store.state.before_fdatasync/first-1/ext4 (crashed after 3s, recovery stall ? ms)

## crashpoint/store.state.before_fdatasync/mid-39/ext4
- crashed after 53s (status 137)
- PASS crashpoint/store.state.before_fdatasync/mid-39/ext4 (crashed after 53s, recovery stall 389 ms)

## crashpoint/store.state.before_fdatasync/last-61/ext4
- crashed after 89s (status 137)
- PASS crashpoint/store.state.before_fdatasync/last-61/ext4 (crashed after 89s, recovery stall 366 ms)

## crashpoint/ublk.after_fetch/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/ublk.after_fetch/first-1/ext4 (crashed after 5s, recovery stall 530 ms)

## crashpoint/ublk.after_fetch/mid-171431/ext4
- crashed after 64s (status 137)
- PASS crashpoint/ublk.after_fetch/mid-171431/ext4 (crashed after 64s, recovery stall 349 ms)

## crashpoint/ublk.after_fetch/last-274288/ext4
- crashed after 91s (status 137)
- PASS crashpoint/ublk.after_fetch/last-274288/ext4 (crashed after 91s, recovery stall 367 ms)
- UNREACHED ublk.after_update_size: the counting run never executed it

## crashpoint/ublk.before_commit/first-1/ext4
- crashed after 5s (status 137)
- PASS crashpoint/ublk.before_commit/first-1/ext4 (crashed after 5s, recovery stall 782 ms)

## crashpoint/ublk.before_commit/mid-171431/ext4
- crashed after 62s (status 137)
- PASS crashpoint/ublk.before_commit/mid-171431/ext4 (crashed after 62s, recovery stall 389 ms)

## crashpoint/ublk.before_commit/last-274288/ext4
- crashed after 93s (status 137)
- PASS crashpoint/ublk.before_commit/last-274288/ext4 (crashed after 93s, recovery stall 632 ms)

## end
overall FAIL=0
