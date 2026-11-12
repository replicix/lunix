# Feature probe

GET_FEATURES mask = 0x7ffff

| flag | bit | set on this kernel | ADR-17 class | first kernel (fact-check §E, not re-verified) | note |
|---|---|---|---|---|---|
| UBLK_F_SUPPORT_ZERO_COPY | 0 | yes | never | 6.15 functional (6.0 stub) | Server cannot read the data (ADR-17: never) |
| UBLK_F_URING_CMD_COMP_IN_TASK | 1 | yes | ignored | 6.0 | Set by the driver itself on every device |
| UBLK_F_NEED_GET_DATA | 2 | yes | never | 6.0 | Extra round trip per write |
| UBLK_F_USER_RECOVERY | 3 | yes | mandatory | 6.1 |  |
| UBLK_F_USER_RECOVERY_REISSUE | 4 | yes | mandatory | 6.1 |  |
| UBLK_F_UNPRIVILEGED_DEV | 5 | yes | not used | 6.3 | Forbids USER_COPY/ZC |
| UBLK_F_CMD_IOCTL_ENCODE | 6 | yes | mandatory | 6.4 | Legacy opcodes are compiled out on this kernel |
| UBLK_F_USER_COPY | 7 | yes | used if the spike wins | 6.5 | P0.2 decides |
| UBLK_F_ZONED | 8 | yes | never | 6.6 |  |
| UBLK_F_USER_RECOVERY_FAIL_IO | 9 | yes | optional policy | 6.13 | fail instead of queue while the server is away |
| UBLK_F_UPDATE_SIZE | 10 | yes | mandatory | 6.16 |  |
| UBLK_F_AUTO_BUF_REG | 11 | yes | never | 6.16 |  |
| UBLK_F_QUIESCE | 12 | yes | mandatory | 6.16 | needs USER_RECOVERY |
| UBLK_F_PER_IO_DAEMON | 13 | yes | not used | 6.16 | Set by the driver itself on every device |
| UBLK_F_BUF_REG_OFF_DAEMON | 14 | yes | never | 6.17 | Set by the driver itself on every device |
| UBLK_F_BATCH_IO | 15 | yes | used when present | 7.0 | P0.2/P0.3 decide the default |
| UBLK_F_INTEGRITY | 16 | yes | not used | 7.0 | Requires USER_COPY |
| UBLK_F_SAFE_STOP_DEV | 17 | yes | used when present | 7.0 | Enables TRY_STOP_DEV; set by the driver itself |
| UBLK_F_NO_AUTO_PART_SCAN | 18 | yes | used when present | 7.0 |  |

## ADD_DEV acceptance (device added with no server, then deleted)

| flags tried (always with CMD_IOCTL_ENCODE) | hex | result |
|---|---|---|
| NO_AUTO_PART_SCAN | 0x40040 | accepted  |
| SAFE_STOP_DEV | 0x20040 | accepted  |
| USER_RECOVERY + USER_RECOVERY_FAIL_IO | 0x248 | accepted  |
| USER_RECOVERY + REISSUE + FAIL_IO (both) | 0x258 | rejected: Invalid argument (os error 22) |
| QUIESCE without USER_RECOVERY | 0x1040 | rejected: Invalid argument (os error 22) |
| USER_RECOVERY + QUIESCE | 0x1048 | accepted  |
| UPDATE_SIZE | 0x440 | accepted  |
| BATCH_IO | 0x8040 | accepted  |
| USER_COPY | 0xc0 | accepted  |
| BATCH_IO + USER_COPY | 0x80c0 | accepted  |
| PER_IO_DAEMON | 0x2040 | accepted  |
| INTEGRITY without USER_COPY | 0x10040 | rejected: Invalid argument (os error 22) |
| UNPRIVILEGED_DEV + USER_COPY | 0xe0 | accepted  |
| without CMD_IOCTL_ENCODE (legacy opcodes) | 0x0 | accepted  |

## NO_AUTO_PART_SCAN: GPT-labelled backing file

- plain: after start (2 s) /dev/ublkb0p1 is present
- plain: dev_info flags = DEVINFO after-add id=0 state=0 queues=4 depth=128 max_io_buf=1048576 flags=0x2745a
- no_part_scan: after start (2 s) /dev/ublkb0p1 is absent
- no_part_scan: after `partx -a` /dev/ublkb0p1 is present
- no_part_scan: dev_info flags = DEVINFO after-add id=0 state=0 queues=4 depth=128 max_io_buf=1048576 flags=0x6745a

## SAFE_STOP_DEV: TRY_STOP_DEV with a mounted file system

- TRY_STOP_DEV while mounted: TRY_STOP_DEV result=-16 (Device or resource busy (os error 16))
- device still present afterwards: yes; file readable: data; write works: yes
- TRY_STOP_DEV after umount: TRY_STOP_DEV result=0 (ok)
- daemon exited after TRY_STOP_DEV: no; bdev present: no

## Plain STOP_DEV while a file system is mounted (observation only)

- STOP_DEV (raw, via timeout 20): STOP_DEV result=0  rc=0
- bdev present after STOP_DEV: no; still mounted: 1
- read of the mounted file: cat: /var/tmp/lunix-spike/mnt/feat/f: Input/output error
- write+sync on the mounted fs: bash: line 1: /var/tmp/lunix-spike/mnt/feat/f: Input/output error rc=0
note: module ublk_drv loaded
ublk-leftovers: clean
