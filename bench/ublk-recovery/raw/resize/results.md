# P0.4 resize results

## Online grow: ext4 (16 GiB -> 64 GiB) under fio randwrite+verify
- before: sysfs size=33554432 sectors (17179869184 bytes), blockdev --getsize64=17179869184
- df size before: 16729894912
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after UPDATE_SIZE (no remount): sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
- fs grow:  
- df size after: 67452637184
- fio exit code: 0; errors: [null,null]
- fsck/repair/check (ext4) after unmount: rc=0

## Online grow: xfs (16 GiB -> 64 GiB) under fio randwrite+verify
- before: sysfs size=33554432 sectors (17179869184 bytes), blockdev --getsize64=17179869184
- df size before: 17068613632
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after UPDATE_SIZE (no remount): sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
- fs grow: data     =                       bsize=4096   blocks=4194304, imaxpct=25 
- df size after: 68608221184
- fio exit code: 0; errors: [null,null]
- fsck/repair/check (xfs) after unmount: rc=0

## Online grow: btrfs (16 GiB -> 64 GiB) under fio randwrite+verify
- before: sysfs size=33554432 sectors (17179869184 bytes), blockdev --getsize64=17179869184
- df size before: 17179869184
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after UPDATE_SIZE (no remount): sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
- fs grow: Resize device id 1 (/dev/ublkb0) from 16.00GiB to max 
- df size after: 68719476736
- fio exit code: 0; errors: [null,null]
- fsck/repair/check (btrfs) after unmount: rc=0

## ext4 special cases
### grow immediately after kill -9 + recovery
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after resize following recovery: sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
- fs grow:  
- fio exit code: 0; errors: [null,null]
### no-op resize (to the current size)
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after no-op: sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
### non-multiple of the 64 KiB chunk: 64 GiB + 12 KiB (3 x 4 KiB)
UPDATE_SIZE bytes=68719489024 sectors=134217752 result=0 (ok)
- after +12 KiB: sysfs size=134217752 sectors (68719489024 bytes), blockdev --getsize64=68719489024
### not a multiple of the 4096-byte logical block: 64 GiB + 12 KiB + 512 B (raw command, driver behaviour)
UPDATE_SIZE bytes=68719489536 sectors=134217753 result=0 (ok)
- after +512 B: sysfs size=134217753 sectors (68719489536 bytes), blockdev --getsize64=68719489536
- fsck after unmount: rc=0 (note: fs still 64 GiB, device is larger)
### shrink (idle, unmounted device): 64 GiB -> 8 GiB, then back
UPDATE_SIZE bytes=8589934592 sectors=16777216 result=0 (ok)
- after shrink: sysfs size=16777216 sectors (8589934592 bytes), blockdev --getsize64=8589934592
- mount attempt on the shrunk device (fs is 64 GiB): mount: /var/tmp/lunix-spike/mnt/rz: wrong fs type, bad option, bad superblock on /dev/ublkb0, missing codepage or helper program, or other error.        dmesg(1) may have more information after failed mount system call.  rc=0
UPDATE_SIZE bytes=68719476736 sectors=134217728 result=0 (ok)
- after grow back: sysfs size=134217728 sectors (68719476736 bytes), blockdev --getsize64=68719476736
- fsck after shrink+grow-back: rc=0

## Write-cache visibility (sysfs after ADD_DEV)
### default
- write_cache = write back
- fua = 1
- rotational = 0
- discard_granularity = 0
- max_sectors_kb = 1024
- max_hw_sectors_kb = 1024
- logical_block_size = 4096
- physical_block_size = 4096
- optimal_io_size = 4096
- minimum_io_size = 4096
- PARAMS after-add types=0x5 attrs=0xc lbs_shift=12 pbs_shift=12 io_min_shift=12 io_opt_shift=12 max_sectors=2048 dev_sectors=2097152
### no-volatile-cache
- write_cache = write through
- fua = 0
- rotational = 0
- discard_granularity = 0
- max_sectors_kb = 1024
- max_hw_sectors_kb = 1024
- logical_block_size = 4096
- physical_block_size = 4096
- optimal_io_size = 4096
- minimum_io_size = 4096
- PARAMS after-add types=0x5 attrs=0x0 lbs_shift=12 pbs_shift=12 io_min_shift=12 io_opt_shift=12 max_sectors=2048 dev_sectors=2097152
