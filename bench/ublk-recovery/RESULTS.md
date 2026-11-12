# P0.3, P0.4 (and P0.12) results: recovery, feature table, resize, FLUSH counts

Spike: `spikes/ublk-recovery` (`ublk-recovery`, libublk `=0.4.8`, io-uring `=0.7.16`). Host, kernel, tool versions: see
`../ublk-baseline/RESULTS.md` (AMD EPYC Zen 5, 16 vCPUs, SMT off, kernel 7.0.0 Ubuntu, Ubuntu 26.04, fio 3.41; governor not
exposed). Dates 2026-11-11/12 (the host was rebooted between the first and second session, see the deadlock section), spike revision uncommitted. Daemon on CPUs 0-3, fio on 4-7, 4 queues x depth 128, copy mode.

## Status at a glance

| item | status |
|---|---|
| feature table and live probes (step 2) | done |
| kill -9 loop, ext4: 36 cycles in 4 phases | done, all clean (session 1) |
| kill -9 loop, xfs: 10 cycles in 2 phases | done, all clean (session 1) |
| QUIESCE planned handover x20 | session 1: deadlocked the daemon in the kernel (below). Session 2 with the fix: **20 of 20 clean on ext4** |
| BATCH_IO recheck, kill -9 x20 | **clean** (20 cycles, fsck 0) |
| BATCH_IO recheck, QUIESCE handover | **FAILED** (below) |
| negative control (a) USER_RECOVERY without REISSUE | fails as expected |
| negative control (b) acked-but-dropped writes | fails as expected |
| FAIL_IO behaviour | done (below) |
| P0.4 resize, write-cache sysfs, FLUSH counting | done (separate sections) |
| P0.12 | **done in the third session** (the second session's attempt wedged a device; see the P0.12 section) |

## Gate (ADR-17, fixed in advance)

Rule: 30 consecutive kill -9 cycles on ext4 and 10 on xfs with zero fio errors and zero verify failures, fsck clean after every
phase, no stall > 30 s at a 1 s restart delay, both negative controls fail as expected, mandatory flags present.

| clause | measured | result |
|---|---|---|
| 30 kill -9 cycles ext4 | 36 cycles in 4 phases (20 immediate restart, 10 with 1 s delay, 3 with 10 s delay, 3 double kills); fio exit 0, no job error | met |
| 10 kill -9 cycles xfs | 6 immediate + 4 with 1 s delay, same | met |
| zero verify failures | fio `verify_fatal` never fired; sha256 of the data files equal before/after remount; fsyncer: 0 bad blocks of acknowledged fsyncs after every phase | met |
| fsck.ext4 -fn / xfs_repair -n after every phase | rc 0 after all 6 phases | met |
| no stall > 30 s at 1 s restart delay | heartbeat stall p50/p99/max 1128/1141/1141 ms (ext4), 1132/1162/1162 (xfs); fio max clat 1187 / 1238 ms | met |
| negative control (a): USER_RECOVERY without REISSUE | after the first recovery fio fails (error 30, read-only file system after EIO), fsyncer I/O error, `fsck` rc 4 | fails as expected |
| negative control (b): daemon acks but drops writes (SIGHUP) before the kill | fio `verify: bad magic header` (fatal), fsyncer 8887 and 9597 bad blocks of acked fsyncs, fsck rc 4 | fails as expected |
| mandatory flags `CMD_IOCTL_ENCODE`, `USER_RECOVERY`, `QUIESCE`, `UPDATE_SIZE` present | all set in GET_FEATURES (0x7ffff) | met |

**Outcome for the kill -9 recovery design (ADR-17/ADR-16): accepted (gate).** Both negative controls trip the harness, so the clean
runs are meaningful. Two amendments come out of the spike (QUIESCE handover and BATCH_IO, below).

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

## QUIESCE handover (planned upgrade)

**Session 1 finding (deadlock).** The daemon called `quiesce_dev()` (`Ok(0)`) and then returned from libublk's `run_target()`, which
issues `STOP_DEV` after the queue threads exit. The kernel thread executing it sat in `ublk_stop_dev_unlocked` ->
`ublk_wait_tagset_rqs_idle` (msleep loop) waiting for started requests that are only requeued after the daemon closes
`/dev/ublkcN`, which it cannot do while that command blocks its io-wq worker. Mounted ext4, fio, fsyncers and jbd2 went to D state,
`kill -9` of the daemon had no effect and only a reboot cleared it (`raw/quiesce-deadlock-evidence.md`).
Design impact (ADR-16/17/18): **a daemon must never issue `STOP_DEV` after `QUIESCE_DEV`**; libublk's `run_target()` cannot be used
for the planned-upgrade path as is.

**Session 2 (after the reboot) with the fix** (`exit()` right after a successful quiesce, so the cdev is closed and the device goes
QUIESCED; nothing else sends STOP_DEV). Tested first on an unmounted device under raw fio load (daemon exited 2.9 s after SIGUSR2, fio
stalled and continued after `--recover`, rc 0), then mounted, with a watchdog that stops everything if the daemon does not exit:

| run | cycles | result | exec->recovered ms p50/max | kill->ready ms p50/max | heartbeat stall ms p50/p99/max |
|---|---|---|---|---|---|
| ext4, QUIESCE handover, copy mode | 20 | fio 0, fsck 0, sha equal, 0 bad acked blocks | 72 / 182 | 172 / 328 | 180 / 2000 / 2000 |
| ext4, kill -9, **BATCH_IO** | 20 | fio 0, fsck 0, sha equal, 0 bad acked blocks | 170 / 180 | 281 / 882 | 252 / 1230 / 1230 |
| ext4, QUIESCE handover, **BATCH_IO** | 1 clean, **2nd failed** | file system went read-only (error 30), fsck rc 4, fsyncer I/O error | - | - | - |

- One handover in the copy-mode run shows a 2.0 s stall (the quiesce waits for in-flight I/O to drain, up to the 5 s timeout I pass);
  the median is 0.18 s.
- **BATCH_IO + QUIESCE does not work with libublk 0.4.8's batch transport as I used it.** After the first (apparently clean) handover
  the second recovered daemon saw the fs go read-only. A plausible cause (not verified): requests already fetched by the batch queue
  but not yet committed when the process exits are not requeued the way a kill -9 requeues them (REISSUE), so ext4 sees an error.
  With kill -9 the same BATCH_IO setup passed all 20 cycles.
- Consequence for the P0.2 rule: the BATCH_IO clause "recovery cycle passes with BATCH_IO" is **not met** for the QUIESCE path; since the
  performance clause already failed, BATCH_IO stays opt-in.

## FAIL_IO and REISSUE while the server is away (`raw/failio2/result.md`)

- Device created with `USER_RECOVERY_FAIL_IO`: after `kill -9` fio gets EIO within ~150 ms (it exits with an error).
  **Recovering such a device failed** (the device was in state 3, FAIL_IO; `START_USER_RECOVERY` path returned an io_uring error in my
  flow), so FAIL_IO is a "give up and remount" policy, not a pause. This matches ADR-17's wording ("use FAIL_IO plus a remount procedure").
- Device with REISSUE: 8 s after the kill fio is still running without error (I/O queued); after `--recover` (72 ms) it finishes with exit 0.
- The hung-queue watchdog (flip a stuck device to FAIL_IO from the control plane) was not tried; only the creation-time flag was.

## P0.4: UPDATE_SIZE online grow, write-cache attributes, FLUSH counting

### Online grow (`raw/resize/results.md`)

| fs | grow 16 -> 64 GiB under fio randwrite+verify | fs grown online | fio errors | fsck/repair/check |
|---|---|---|---|---|
| ext4 | `UPDATE_SIZE` rc 0, sysfs and `blockdev --getsize64` changed without remount | `resize2fs`, df 16.7 -> 67.5 GB | 0 | rc 0 |
| xfs | same | `xfs_growfs`, df 17.1 -> 68.6 GB | 0 | rc 0 |
| btrfs | same | `btrfs filesystem resize max`, df 17.2 -> 68.7 GB | 0 | rc 0 |

Special cases (ext4): grow right after kill -9 + recovery works (fio 0 errors); resize to the current size is accepted (no-op); 64 GiB + 12 KiB
is accepted and visible; 64 GiB + 12 KiB + 512 B (not a multiple of the 4096-byte logical block) is **also accepted by the driver** (size
134217753 sectors, not a multiple of the block size; libublk's wrapper refuses it, the raw command does not), so the daemon must validate;
shrink 64 -> 8 GiB on an idle unmounted device is accepted (the 64 GiB fs then refuses to mount: wrong fs type/bad superblock) and growing back restores a clean fs (fsck rc 0).

### Write-cache visibility

Default attrs `VOLATILE_CACHE | FUA`: `write_cache = write back`, `fua = 1`, `logical/physical_block_size = 4096`, `max_sectors_kb = 1024`,
`rotational = 0`, `discard_granularity = 0`. Control without `VOLATILE_CACHE`: `write through`, `fua = 0`.

### FLUSH / FUA / writes per fsync (`raw/flush/`, `raw/flush2/`; 10 000 pwrite+sync iterations onto preallocated files, default mkfs and mount options)

| fs | variant | jobs | writes per sync | FUA per sync | FLUSH per sync |
|---|---|---|---|---|---|
| ext4 | fsync | 1 | 3.013 | 1.0 | 1.0 |
| ext4 | fsync | 8 | 1.501 | 0.251 | 0.251 |
| ext4 | fdatasync | 1 | 3.0 | 1.0 | 1.0 |
| ext4 | fdatasync | 8 | 1.502 | 0.252 | 0.251 |
| ext4 | o_sync | 1 | 3.001 | 1.0 | 1.0 |
| ext4 | o_sync | 8 | 1.503 | 0.252 | 0.251 |
| ext4 | o_dsync | 1 | 3.001 | 1.0 | 1.0 |
| ext4 | o_dsync | 8 | 1.505 | 0.253 | 0.253 |
| xfs | fsync | 1 | 2.001 | 1.0 | 1.0 |
| xfs | fsync | 8 | 1.303 | 0.304 | 0.765 |
| xfs | fdatasync | 1 | 2.0 | 1.0 | 1.0 |
| xfs | fdatasync | 8 | 1.298 | 0.299 | 0.775 |
| xfs | o_sync | 1 | 2.0 | 1.0 | 1.0 |
| xfs | o_sync | 8 | 1.302 | 0.302 | 0.731 |
| xfs | o_dsync | 1 | 2.001 | 1.0 | 1.0 |
| xfs | o_dsync | 8 | 1.303 | 0.303 | 0.737 |
| btrfs | fsync | 1 | 11.62 | 1.0 | 1.0 |
| btrfs | fsync | 8 | 6.209 | 0.25 | 0.25 |
| btrfs | fdatasync | 1 | 11.625 | 1.0 | 1.0 |
| btrfs | fdatasync | 8 | 5.91 | 0.25 | 0.25 |
| btrfs | o_sync | 1 | 11.637 | 1.0 | 1.0 |
| btrfs | o_sync | 8 | 5.949 | 0.25 | 0.25 |
| btrfs | o_dsync | 1 | 11.627 | 1.0 | 1.0 |
| btrfs | o_dsync | 8 | 5.293 | 0.25 | 0.25 |

Setup traffic (writes / FUA / FLUSH commands): mkfs ext4 110/0/4, xfs 180/0/2, btrfs 52/0/18; mount ext4 1/1/0, xfs 2/0/3, btrfs 0/0/0; umount ext4 5/4/7, xfs 9/3/4, btrfs 24/1/1.

Reading: every single-job sync costs exactly one FUA write plus one FLUSH on all three file systems (ext4 3 writes, xfs 2, btrfs 11.6 because of its log tree).
With 8 concurrent syncers the kernel coalesces: ext4 and btrfs drop to 0.25 FLUSH per sync, xfs to 0.73-0.78. **No file system issues more than 3 FLUSH
commands per sync** (maximum 1.0). The FUA/FLUSH pair is the unit the journal's group commit (DESIGN section 8/4) has to absorb: one DELTA/journal sync point per
FLUSH, and FUA writes must be durable on completion. ext4's FLUSH counts are from the first run; xfs and btrfs from a second launch (the first launch hit its
20 minute watchdog during xfs; no data from the interrupted part was used).


## P0.12: keeper-held RAM write buffer (ADR-26)

Implementation (`spikes/ublk-recovery`): `run --sink mem --keeper SOCK --mem-mib N`; a memfd ring with a header {magic, device id, generation} and 32-byte record headers;
writes are appended and acknowledged from RAM; FLUSH and FUA drain the ring into the backing file and `fdatasync`; single queue. `keeper/keeper.py` holds the memfd
(`PUT`/`GET` over a Unix socket with SCM_RIGHTS, under the scratch dir). On `--recover` the daemon asks the keeper first; if the keeper has no buffer, is unreachable,
or the magic, device id or generation does not match, it prints `REFUSED`, deletes the device and exits 3, without ever opening the cdev. Otherwise it replays
(drain), bumps the generation and re-registers the buffer with the keeper.

**Ring-full / safety changes made after the second-session wedge** (what I did about the hazard): (1) the sink's backing file is opened *without* `O_DIRECT`, so a drain is
buffered `pwrite`s plus one `fdatasync` instead of one synchronous disk write per record (the O_DIRECT drain took ~3.3 ms per record on this disk, i.e. minutes for a full ring);
(2) the measurement runs are bounded (`--io_size`, never more than the ring); (3) every control command has a `timeout`; (4) no SIGKILL to a stopping daemon and a 30 s wait
instead; (5) any failed start aborts the driver. I did **not** move the drain off the queue thread: when the ring fills, `append()` still drains inline on the queue thread
(requests wait during the drain, which is backpressure by delay, but the thread does block). That path was exercised for real (the first kill in the 3-cycle run had 268 MB pending in a 256 MiB ring)
and cost about 1.4 s of replay/drain, with buffered I/O; the shutdown race that caused the wedges did not occur.

### Acknowledgement latency (raw device, unmounted, 4 KiB random write, no FLUSH issued; `raw/p12b/ramsink.log`)

| sink | QD | IOPS | latency p50 / p99 / p99.9 | io_size |
|---|---|---|---|---|
| file (O_DIRECT write to the backing file, not durable) | 1 | 243 | 3359 / 13697 / 16908 us | 8 MiB |
| file | 32 | 4294 | 6652 / 16581 / 33816 us | 64 MiB |
| **mem (RAM sink)** | 1 | 210 051 | **3.7 / 11.7 / 155 us** | 64 MiB |
| **mem** | 32 | 712 347 | **42 / 52 / 514 us** | 64 MiB |

Against the P0.5 journal (group commit, 4 KiB, ext4 on a loop device, closed loop): durable latency p50 4.6 ms (B=64) to 2.7 ms (B=1, 356 rec/s), flush p50 ~1.8 ms; on the real-disk directory 0.9-1.5 ms.
The RAM sink acknowledges a write ~700x faster than the journal makes it durable (3.7 us vs 2.7 ms at QD1), as expected: nothing in the ack path waits for a medium. The price is
that a FLUSH still costs a drain + `fdatasync` here, and that the data is only as durable as the keeper (see the lost-keeper case).

### Replay of a 256 MiB buffer after kill -9 (no FLUSH before the kill, backing file buffered)

| record size | pending bytes | applied | replay time | exec to recovered |
|---|---|---|---|---|
| 64 KiB | 262 272 000 | 262 144 000 | 3.76 s | 3.89 s |
| 4 KiB | 264 192 000 | 262 144 000 | 23.7 s | 23.8 s |

The replay is dominated by the backing disk (here ~10 K random 4 KiB writes per second plus the final `fdatasync`); in the real design the replay feeds the destage/commit path, not a local file,
so these numbers are an upper bound for a slow local medium. Note the device stalls (I/O queued) for the whole replay.

### Gate runs

| run | result |
|---|---|
| 3 mounted cycles (ext4, fio verify, 2 fsyncers, heartbeat) | clean: fio 0, fsck 0, sha equal, 0 bad acked blocks; replay 1.4 s / 1.1 s / 0.2 s |
| **20 mounted cycles** (kill -9 at 5-12 s) | **clean**: fio exit 0, fsck rc 0, sha equal, 0 bad blocks of acked fsyncs, 0 heartbeat errors. exec to recovered p50/p99/max 314 / 1264 / 1264 ms; kill to ready p50/max 452 / 2492 ms; heartbeat stall p50/p99/max 594 / 1819 / 1819 ms; pending bytes at the kill from 1.4 MB to 203 MB (203 MB replayed in 0.59 s) |
| **lost keeper** (keeper and daemon `kill -9` together, then restart with `--recover`) | **refused and deleted, never resumed**: `REFUSED keeper unreachable ... Connection refused`, `DEL_DEV` rc 0; fio failed with `Input/output error`, the fsyncer got `Errno 5` at block 19, ext4 logged `Buffer I/O error on device ublkb0` and went to `emergency_ro` (read-only) |
| generation-mismatch refusal path | exists in code, not exercised separately |
| systemd FD-store variant | **not run**: needs `sd_notify(FDSTORE=1)` and a `LISTEN_FDS` recovery path in the daemon; not cheap, skipped |

Observation from the lost-keeper run: after the refusal, the refusing process stayed blocked in `DEL_DEV` (the kernel waits for the last reference to the block device, which the still-mounted, now
read-only ext4 held) and ignored the harness's `timeout` SIGTERM (the spike masks SIGTERM until it serves). It completed on its own as soon as I ran `umount` on the dead fs; the device was never wedged (state S, not D).
The driver chain was stopped once the DEL command exceeded the watchdog, per the rules; only the `umount` was issued afterwards. Phase 1 needs the same awareness: a deleted device with a mounted fs keeps the cdev/bdev
alive until the mount is released.

**Gate (P0.12): met.** All 20 cycles finish with fsck clean; no write acknowledged by a FLUSH before a kill was lost (fsyncer: 0 bad blocks, fio verify clean); the lost-keeper case never resumed the device.
Limitations: fio's variable block size + `norandommap` limits its verify coverage ("verify limited"); the single queue; the drain is on the queue thread; the FD-store variant and the replay with a real remote sink were not tested.

## Proposed decision record (for the lead to merge)

ADR-17 gate outcome (2026-11-12): **accepted (gate)** for kill -9 recovery (ext4 36 cycles, xfs 10, RAM-sink ext4 23) and for QUIESCE with the corrected daemon (20/20 clean);
**amended**: (1) after `QUIESCE_DEV` the daemon exits without `STOP_DEV` (libublk `run_target()` unusable for that path); (2) `SET_PARAMS` is retained by the driver; (3) BATCH_IO stays
opt-in (no performance gain, and a QUIESCE handover broke the fs in the one trial); (4) `USER_RECOVERY_FAIL_IO` devices are not recoverable (delete and remount); (5) the daemon
must never let `STOP_DEV` or process exit race with started requests it is not completing (device wedge, observed twice; also: deleting a device with a mounted fs blocks until unmount). Both negative controls trip the harness as designed.
ADR-20: online grow via `UPDATE_SIZE` works on ext4, xfs and btrfs under load, also right after a recovery; the driver accepts sizes that are not a multiple of the logical block size, so the daemon must validate.
ADR-26 gate outcome: **accepted (gate)** for the keeper-held memfd with generation check: 23 mounted recovery cycles clean, lost keeper refuses and deletes (EIO to the fs), RAM-sink ack latency 3.7 us at QD1 /
42 us at QD32. Open points for phase 1: replay time depends on the sink (3.8 s for 256 MiB of 64 KiB records, 24 s for 4 KiB records onto a slow disk), the drain must not run on the queue thread, the systemd FD-store variant is untested.

## Host state

Third-session end state: no ublk device, `ublk_drv` unloaded by me, no mounts, scratch dir empty (`run/created-devices` only), `rec.img` removed. Leftover check clean.
