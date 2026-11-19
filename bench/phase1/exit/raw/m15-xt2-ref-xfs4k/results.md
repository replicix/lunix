# M15 xfstests TARGET=loop FS=xfs chunk=65536 group=xfs/129 xfs/234 xfs/253 (2026-11-18T23:08:38Z, kernel 7.0.0, 16 cpus)
- xfstests: d07dca72 2026-10-15; fsck tools: e2fsck 1.47.2 (1-Jan-2025), xfs_repair version 6.18.0
- TEST_DEV /dev/loop8, SCRATCH_DEV /dev/loop9
- ./check ended with status 1 after 6s
    +xfs_mdrestore: Invalid superblock disk address/length
    +mount: <scratch>/mnt/m15-xfstests-loop-xfs-scratch: can't read superblock on /dev/loop10.
    +       dmesg(1) may have more information after failed mount system call.
    +mount -o nosuid,nodev /dev/loop10 <scratch>/mnt/m15-xfstests-loop-xfs-scratch failed
    +(see <scratch>/m15-xt2-ref-xfs4k/results/xfs/253.full for details)
    ...
    (Run 'diff -u <m15>/xfstests/tests/xfs/253.out <scratch>/m15-xt2-ref-xfs4k/results/xfs/253.out.bad'  to see the entire diff)
check: possible mount failures (see <scratch>/m15-xt2-ref-xfs4k/results/xfs/253.mountfail)
Ran: xfs/129 xfs/234 xfs/253
Failures: xfs/129 xfs/234 xfs/253
Failed 3 of 3 tests

- PASS guest fsck of the TEST device clean after the run
- compare with: python3 bench/phase1/exit/xfstests-compare.py <reference dir> <lunix dir>...
overall FAIL=0
