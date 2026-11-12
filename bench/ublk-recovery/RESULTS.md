# P0.3 results: recovery under kill -9, feature table (partial); P0.4 and P0.12 not run

Spike: `spikes/ublk-recovery` (`ublk-recovery`, libublk `=0.4.8`, io-uring `=0.7.16`). Host, kernel, tool versions: see
`../ublk-baseline/RESULTS.md` (AMD EPYC Zen 5, 16 vCPUs, SMT off, kernel 7.0.0 Ubuntu, Ubuntu 26.04, fio 3.41; governor not
exposed). Date 2026-11-11/12, spike revision uncommitted. Daemon on CPUs 0-3, fio on 4-7, 4 queues x depth 128, copy mode.

## Status at a glance

| item | status |
|---|---|
| feature table and live probes (step 2) | done |
| kill -9 loop, ext4: 36 cycles in 4 phases | done, all clean |
| kill -9 loop, xfs: 10 cycles in 2 phases | done, all clean |
| QUIESCE planned handover x20 (step 4) | **FAILED: the first handover deadlocked the daemon in the kernel (below); the host has a stuck device. Everything after it was stopped per CONVENTIONS rule 6** |
| BATCH_IO recheck (step 5), negative controls (step 6), FAIL_IO check | **not run** (stopped after the hang) |
| P0.4 resize / FLUSH counting | **not run**; scripts written (`run-resize.sh`, `run-flush-count.sh`), never executed |
| P0.12 RAM write buffer | **not run**; code written and compiled, never executed |

## Gate (ADR-17, fixed in advance): partial

Rule: 30 consecutive kill -9 cycles on ext4 and 10 on xfs with zero fio errors and zero verify failures, fsck clean after every
phase, no stall > 30 s at a 1 s restart delay, **both negative controls fail as expected**, mandatory flags present.

| clause | measured | result |
|---|---|---|
| 30 kill -9 cycles ext4 (plan) | 36 cycles in 4 phases (20 immediate restart, 10 with 1 s delay, 3 with 10 s delay, 3 double kills); fio exit 0, no job error | met |
| 10 kill -9 cycles xfs | 6 immediate + 4 with 1 s delay, same | met |
| zero verify failures | fio `verify_fatal` never fired; sha256 of the data files equal before/after remount; fsyncer: 0 bad blocks of acknowledged fsyncs after every phase | met |
| fsck.ext4 -fn / xfs_repair -n after every phase | rc 0 after all 6 phases | met |
| no stall > 30 s at 1 s restart delay | heartbeat stall p50/p99/max 1128/1141/1141 ms (ext4), 1132/1162/1162 (xfs); fio max clat 1187 / 1238 ms | met |
| both negative controls fail as expected | **not run** | **not evaluated: the gate cannot be called green** |
| mandatory flags `CMD_IOCTL_ENCODE`, `USER_RECOVERY`, `QUIESCE`, `UPDATE_SIZE` present | all set in GET_FEATURES (0x7ffff) | met |

**Outcome: ADR-17 not closed.** The kill -9 half of the recovery design is confirmed with margin. The QUIESCE half found a defect
(below) that must be understood first, and the negative controls remain to be run.

## What the harness does

`run-crash-loop.sh`: mkfs, mount (`nosuid,nodev`) under the scratch dir; fio job `mixed` (randrw 70 % write, bssplit
4k/50:16k/30:64k/20, iodepth 32, 4 jobs x 1 GiB, `verify=crc32c verify_backlog=64 verify_fatal=1`, `direct=1`); two
`fsyncer.py` writers (4 KiB write + fsync, ack recorded on the host fs only after fsync returns; every acked block must hold its
pattern after the run); a 1 ms O_DIRECT `heartbeat.py` writer for stall measurement; `kill -9` at random 5-20 s intervals (fixed
seed); the restart is `run --recover`. After each phase: stop writers, `umount`, fsck, remount, compare sha256 of the data files,
verify the fsyncer blocks. Phases: `immediate`, `delay1s`, `delay10s` (I/O queues during the outage), `double` (a second kill
0.1-0.4 s after the recovery daemon's exec, i.e. inside the recovery window).

Deviations and limits: (1) fio's worst clat is the whole-phase maximum, not per kill window; the per-kill stall is the heartbeat
gap. (2) The plan's `--size=4g` is 4 jobs x 1 GiB; `norandommap` with variable block sizes makes fio print "verify limited", i.e. the
in-run verify covers fewer blocks than a full map would; the fsyncer and sha256 checks are independent of that. (3) `fio
--verify_only` replay of a saved verify state was not used (fio does not support it for `time_based` jobs). (4) The 36 ext4 cycles
were split over four phases so that fsck runs after every phase.

## Stall statistics (kill to serving again)

Columns: daemon time from exec to `END_USER_RECOVERY` complete; supervisor time from the kill to the new daemon's READY; heartbeat =
largest gap between two 1 ms writes in the window [kill, ready + 2 s]. ms.

ext4:

| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|
| immediate | 20 | 167 | 172 | 172 | 259 | 268 | 221 | 238 | 238 |
| delay1s | 10 | 68 | 70 | 70 | 1161 | 1174 | 1128 | 1141 | 1141 |
| delay10s | 3 | 66 | 66 | 66 | 10146 | 10162 | 10118 | 10124 | 10124 |
| double | 3 | 163 | 164 | 164 | 597 | 609 | 229 | 230 | 230 |

xfs:

| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|
| immediate | 6 | 166 | 167 | 167 | 266 | 277 | 233 | 245 | 245 |
| delay1s | 4 | 66 | 68 | 68 | 1164 | 1176 | 1132 | 1162 | 1162 |

Findings from the logs (`raw/ext4-kill9/daemon.*.log`):

- **The driver retains `SET_PARAMS` across recovery.** `GET_PARAMS` after recovery equals the pre-kill value (attrs `0xc`, 4096-byte
  blocks, `max_sectors` 2048, `dev_sectors` unchanged) without re-sending; libublk's recover path (no `SET_PARAMS`) is right. This
  resolves the open question in the ublk research note. `dev_info` (queues, depth, max_io_buf, flags `0x2745a`) and per-queue
  affinity are retained too.
- After a `kill -9` the device is still `LIVE` for a moment: the first `START_USER_RECOVERY` sees state 1 and `EBUSY`; libublk retries
  every 100 ms until the kernel has quiesced it (`state=2`). Exec-to-recovered is ~167 ms with an immediate restart (about 100 ms of it
  waiting for the quiesce) and ~66 ms with a delayed one.
- Kill to serving: ~0.26 s with an immediate restart, 1.16 s with a 1 s delay.
- The kernel sets `URING_CMD_COMP_IN_TASK`, `PER_IO_DAEMON`, `BUF_REG_OFF_DAEMON` and `SAFE_STOP_DEV` on every device by itself.

## Feature table and live probes (step 2)

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

Notes: "accepted" only means `ADD_DEV` succeeded with no server attached. `BATCH_IO + USER_COPY` and `UNPRIVILEGED_DEV + USER_COPY` are
accepted by the root-run `ADD_DEV`, although libublk 0.4.8 refuses both in its own validation; the last row uses encoded opcodes
either way, so it only shows the driver sets `CMD_IOCTL_ENCODE` itself. The partition-scan probe uses a GPT written with 4096-byte
sectors (a 512-byte GPT is invisible to a 4096-byte-block device; the first attempt used one and was discarded).
`NO_AUTO_PART_SCAN` works as documented: `/dev/ublkbNp1` is absent until `partx -a`. `TRY_STOP_DEV` (SAFE_STOP_DEV) refuses with EBUSY
while mounted and succeeds after `umount`; plain `STOP_DEV` yanks the device from under a mounted fs (the fs then returns EIO), so
SAFE_STOP_DEV is the right command for `detach`. ("daemon exited after TRY_STOP_DEV: no" is an artefact: the spike's main thread
sits in `sigwait`.) The "first kernel release" column is copied from the fact-check note (section E) and **not re-verified here** (no
kernel git tree); ADR-17's 6.16 floor stays provisional. libublk 0.4.8 cannot set `NO_AUTO_PART_SCAN`, `SAFE_STOP_DEV`, `FAIL_IO` or
`PER_IO_DAEMON` (its `ctrl_flags` validation rejects them); `src/rawctl.rs` adds the device through the control ring directly and
libublk attaches afterwards (this works).

## Finding: QUIESCE handover deadlocks when the daemon uses libublk's `run_target()`

Evidence: `raw/quiesce-deadlock-evidence.md`. `quiesce_dev()` returned `Ok(0)`; the daemon then returned from `run_target()`, which
issues `STOP_DEV`; the kernel thread executing it sits in `ublk_stop_dev_unlocked` -> `ublk_wait_tagset_rqs_idle` (msleep loop)
waiting for started requests that can only be requeued after the daemon closes `/dev/ublkcN`, which it cannot do while that command
blocks its io-wq worker. The mounted ext4, fio, the fsyncers and jbd2 are in D state; `kill -9` of the daemon does not help.

Design impact (ADR-16/17/18): **a daemon must never issue `STOP_DEV` after `QUIESCE_DEV`**; after a successful quiesce it must exit (or
close the cdev) directly. libublk's `run_target()` cannot be used for the planned-upgrade path as is: phase 1 needs its own queue
lifecycle (libublk's `ramdisk` example ends with `disown()` instead of a stop) or an upstream fix. The spike was changed
(`std::process::exit` right after a successful quiesce), but **that fix is untested**: the run was stopped per rule 6.

## Host state left behind (needs the lead)

Device `ublk0` is stuck, with the mounted ext4 at the scratch mount `mnt/rec`, the daemon process, 4 fio workers, 2 fsyncers, the
heartbeat writer and jbd2/flush kworkers in uninterruptible sleep, and the 16 GiB backing file still allocated (held open).
`ublk_drv` has a reference count of 3 and cannot be unloaded. Nothing was forced (no reboot, no `rmmod -f`, no truncation of the
backing file). A bare `sync(1)` on this host hangs on the stuck ext4 (a `sync` process is already in D state); use `sync -f <file>`.
Only a reboot or a driver-side release clears it.

## Proposed decision record (for the lead to merge)

ADR-17 gate outcome (2026-11-12): **open**. kill -9 recovery: met on ext4 (36 cycles) and xfs (10), see above. QUIESCE planned handover:
failed with the libublk `run_target()` flow (kernel deadlock, needs a reboot); mandatory flags present; `SET_PARAMS` is retained by
the driver; negative controls not yet run. Amend ADR-16/18: after `QUIESCE_DEV` the daemon exits without `STOP_DEV`; `run_target()` is
not usable for that path.
