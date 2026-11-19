# M15 xfstests TARGET=lunix FS=xfs chunk=65536 group=xfs/129 xfs/234 xfs/253 (2026-11-18T23:08:46Z, kernel 7.0.0, 16 cpus)
- xfstests: d07dca72 2026-10-15; fsck tools: e2fsck 1.47.2 (1-Jan-2025), xfs_repair version 6.18.0
- PASS create xt (16 GiB, chunk 65536)
- PASS attach xt
- PASS create xs (16 GiB, chunk 65536)
- PASS attach xs
- TEST_DEV /dev/ublkb0, SCRATCH_DEV /dev/ublkb1
- ./check ended with status 1 after 10s
    +xfs_mdrestore: Invalid superblock disk address/length
    +mount: <scratch>/mnt/m15-xfstests-lunix-xfs-64k-scratch: can't read superblock on /dev/loop8.
    +       dmesg(1) may have more information after failed mount system call.
    +mount -o nosuid,nodev /dev/loop8 <scratch>/mnt/m15-xfstests-lunix-xfs-64k-scratch failed
    +(see <scratch>/m15-xt2-l64-xfs3/results/xfs/253.full for details)
    ...
    (Run 'diff -u <m15>/xfstests/tests/xfs/253.out <scratch>/m15-xt2-l64-xfs3/results/xfs/253.out.bad'  to see the entire diff)
check: possible mount failures (see <scratch>/m15-xt2-l64-xfs3/results/xfs/253.mountfail)
Ran: xfs/129 xfs/234 xfs/253
Failures: xfs/129 xfs/234 xfs/253
Failed 3 of 3 tests

- PASS guest fsck of the TEST device clean after the run
- PASS detach xt
- PASS lunix fsck --deep xt clean (exit 0)
- PASS detach xs
- PASS lunix fsck --deep xs clean (exit 0)
- compare with: python3 bench/phase1/exit/xfstests-compare.py <reference dir> <lunix dir>...
overall FAIL=0
