# M15 offline repair crash points (2026-11-20T00:16:51Z): store.set_aside.before_rename store.set_aside.after_rename store.set_aside.after_dirsync store.segment_delete.before_unlink store.segment_delete.after_unlink store.segment_delete.after_dirsync

## fsck-repair/store.set_aside.before_rename
- PASS create volume reptoresetasidebeforerename (4 GiB, chunk 65536, integrity)
- PASS attach reptoresetasidebeforerename (4 queues)
- device /dev/ublkb0
- PASS fsck-repair/store.set_aside.before_rename: 1 GiB written (fio rc 0)
- first repair with the crash setting: exit 137
- PASS fsck-repair/store.set_aside.before_rename: the repair run again without the crash setting finishes (exit 1)
- PASS fsck-repair/store.set_aside.before_rename: fsck --deep after the repair clean (exit 1)
- PASS fsck-repair/store.set_aside.before_rename: the volume attaches after the repair

## fsck-repair/store.set_aside.after_rename
- PASS create volume repstoresetasideafterrename (4 GiB, chunk 65536, integrity)
- PASS attach repstoresetasideafterrename (4 queues)
- device /dev/ublkb0
- PASS fsck-repair/store.set_aside.after_rename: 1 GiB written (fio rc 0)
- first repair with the crash setting: exit 137
- the same repair again refuses (exit 2: repair_refused             --truncate-tip: the chain below the previous commit does not verify); trying the plain --repair
- PASS fsck-repair/store.set_aside.after_rename: the repair run again without the crash setting finishes (exit 1)
- PASS fsck-repair/store.set_aside.after_rename: fsck --deep after the repair clean (exit 1)
- PASS fsck-repair/store.set_aside.after_rename: the volume attaches after the repair

## fsck-repair/store.set_aside.after_dirsync
- PASS create volume reptoresetasideafterdirsync (4 GiB, chunk 65536, integrity)
- PASS attach reptoresetasideafterdirsync (4 queues)
- device /dev/ublkb0
- PASS fsck-repair/store.set_aside.after_dirsync: 1 GiB written (fio rc 0)
- first repair with the crash setting: exit 137
- the same repair again refuses (exit 2: repair_refused             --truncate-tip: the chain below the previous commit does not verify); trying the plain --repair
- PASS fsck-repair/store.set_aside.after_dirsync: the repair run again without the crash setting finishes (exit 1)
- PASS fsck-repair/store.set_aside.after_dirsync: fsck --deep after the repair clean (exit 1)
- PASS fsck-repair/store.set_aside.after_dirsync: the volume attaches after the repair

## fsck-repair/store.segment_delete.before_unlink
- PASS create volume repegmentdeletebeforeunlink (4 GiB, chunk 65536, integrity)
- PASS attach repegmentdeletebeforeunlink (4 queues)
- device /dev/ublkb0
- daemon killed at destage.after_segment_seal:20
- device of the dead daemon deleted (rc 0)
- first repair with the crash setting: exit 137
- PASS fsck-repair/store.segment_delete.before_unlink: the repair run again without the crash setting finishes (exit 0)
- PASS fsck-repair/store.segment_delete.before_unlink: fsck --deep after the repair clean (exit 0)
- FAIL fsck-repair/store.segment_delete.before_unlink: the volume attaches after the repair

## fsck-repair/store.segment_delete.after_unlink
- PASS create volume repsegmentdeleteafterunlink (4 GiB, chunk 65536, integrity)
- PASS attach repsegmentdeleteafterunlink (4 queues)
- device /dev/ublkb0
- daemon killed at destage.after_segment_seal:20
- device of the dead daemon deleted (rc 0)
- first repair with the crash setting: exit 137
- PASS fsck-repair/store.segment_delete.after_unlink: the repair run again without the crash setting finishes (exit 0)
- PASS fsck-repair/store.segment_delete.after_unlink: fsck --deep after the repair clean (exit 0)
- FAIL fsck-repair/store.segment_delete.after_unlink: the volume attaches after the repair

## fsck-repair/store.segment_delete.after_dirsync
- PASS create volume repegmentdeleteafterdirsync (4 GiB, chunk 65536, integrity)
- PASS attach repegmentdeleteafterdirsync (4 queues)
- device /dev/ublkb0
- daemon killed at destage.after_segment_seal:20
- device of the dead daemon deleted (rc 0)
- first repair with the crash setting: exit 137
- PASS fsck-repair/store.segment_delete.after_dirsync: the repair run again without the crash setting finishes (exit 0)
- PASS fsck-repair/store.segment_delete.after_dirsync: fsck --deep after the repair clean (exit 0)
- FAIL fsck-repair/store.segment_delete.after_dirsync: the volume attaches after the repair

overall FAIL=1
