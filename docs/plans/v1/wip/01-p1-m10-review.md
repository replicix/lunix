# Review of M10 — `lunix-ublk` (branch `p1/m10-ublk` at b7f99a4, on main 2f211d1)

Independent review of the ublk device layer against the plan's M10 section
and gate, W1-W9, D1-D20, ADR-16/17/18/20, DESIGN §3, the `lunix_iface::block`
rustdoc, the phase 0 results (`bench/ublk-recovery/RESULTS.md`) and the M11
review for the engine side. I read every file of the diff, libublk 0.4.8
(`io.rs`, `ctrl.rs`) and the upstream v7.0 `drivers/block/ublk_drv.c` for the
kernel side of each claim. No root run, no device, no fio: the root cells
were reviewed from the scripts and their recorded outputs. The non-root
tests were re-run on the remote host; one demonstration test and two
mutations ran in a scratch copy; both the copy and the host directory are
deleted. `rust-skills` ids are cited where they apply; plan and spec win.

## Verdict: **merge after fixes** (one blocker, five majors)

The control side is careful and mostly right: there is no `STOP_DEV` opcode
anywhere, every control command runs on its own ring under a watchdog,
recovery sends no `SET_PARAMS`, sizes are validated before the kernel sees
them, `abort()` is the only way out of a queue thread with work in hand, and
the wake path (level-triggered `POLL_ADD`, drained in `poll`, re-armed
after) has no lost-wakeup window. I found **no path that loses an
acknowledged write and no path that wedges the host in the code as the
daemon uses it**. The root cells and both negative controls support the
kill -9 and handover results the report states.

What is not right is the part the milestone is mostly about: the QUIESCE
"drain". libublk silently discards every commit made after it has seen the
first `ABORT`, so the requests the engine still held at `QUIESCE_DEV` are
never committed; the layer counts them as completed and exits. That is safe
only because `REISSUE` replays them, exactly like a kill -9, and it is the
opposite of what the code comments, the report (finding 1) and the
DECISIONS line "never exits while started requests are not being completed"
say. The same mechanism has a race that stops a queue thread from ever
ending. The other findings are recovery holes around the identity record
(a crash during a grow makes the daemon refuse its own device; nothing ties
a record to the kernel device) and one way to rebuild the phase 0 wedge
through `DEL_DEV`.

## Gate re-run and evidence

| step | result |
|---|---|
| `cargo test -p lunix-ublk` (remote, 4 pinned CPUs, `-j 4`) | **56 library + 2 daemon tests pass**, 6 root tests ignored. Matches the report. |
| scratch copy, extra test: device grown in the kernel, record not yet rewritten, engine at the new size | `begin_recovery` returns `Identity { DevSectors }`: the daemon refuses its own device (major 1) |
| mutation: flags comparison removed from `check_identity` | caught by `identity::tests::a_wrong_queue_count_depth_buffer_flags_or_size_is_refused` |
| mutation: `EBUSY` of `QUIESCE_DEV` no longer typed | caught by `session::tests::a_quiesce_that_times_out_is_a_typed_error` |
| recorded root outputs (read only) | raw 50 PASS / 0 FAIL; mounted 14 / 0; resize 37 / 0 with `errors [0,0]` on all four fio runs; ext4 kill -9 20+10+3+3 cycles all `fio 0, fsck 0, verify 0`; xfs 6+4; QUIESCE 20 of 20 `ok attempt=1`; crash points 4+4; no-`REISSUE` control `fio 4, fsck 4, verify 101` (fsyncer got `EROFS`); drop-writes control `fio 4, fsck 4, verify 21` (3498 and 3585 bad acknowledged blocks); e2e 6 of 6. All equal to the report. |
| stall tables | equal to the report's fourth-round and second-round numbers |

Not verified by me: anything that needs a device. Blocker 1 and major 3 are
derived from reading libublk and the kernel source, not from a run; the
kernel source read is the upstream v7.0 tag, not the host's build.

## Findings

### Blocker

**B1. The QUIESCE path does not complete the requests it waits for, and can
hang** — `src/queue.rs:328-340`, `:434-444`, `:552-565`; `src/device.rs:522-558`.

libublk 0.4.8 `UblkQueue::__queue_io_cmd` (`io.rs:1872-1908`) returns without
queuing anything once the queue state is `STOPPING`, and
`complete_io_cmd_unified` returns `Ok(())` either way. `STOPPING` is set by
`update_state_batch` at the end of the CQE batch that contained the first
`UBLK_IO_RES_ABORT`. `Comp::complete` then does
`counters.completed.fetch_add(1)` (`queue.rs:565`) whether or not a commit
was queued. Three consequences:

1. **Silent non-commit (every loaded handover).** After a successful
   `QUIESCE_DEV` the driver cancels the idle commands (kernel
   `ublk_ctrl_quiesce_dev` → `ublk_cancel_dev`), the thread reaps the
   `ABORT`s, libublk reports `QueueIsDown`, and the loop goes to
   `wait_for_engine`. Every request the engine completes from there is
   dropped by libublk. `in_flight()` reaches 0, the thread ends, `quiesce()`
   returns `Ok`, the process exits, and the kernel still owns k started
   requests that were never committed. They are requeued when the char
   device closes and reissued to the successor. No acknowledged write is
   lost (the kernel never saw an acknowledgement, and the engine applied the
   data before its final commit), which is why 23 handovers passed. But:
   - `device.rs:525-531` ("the kernel has seen every commit", "nothing
     exits with started requests in flight (W2)") and the report's finding 1
     are false;
   - the safety checks at `queue.rs:346-352` and `device.rs:550-555` compare
     a counter that no longer means "committed", so they cannot fire;
   - the handover is a kill -9 with a clean engine commit, not the drain
     DECISIONS and plan step 6 describe. With `NoReissueNegativeControl`
     the same path turns into I/O errors with no warning.
2. **A queue thread that never ends.** If a request is completed in the
   *same* CQE batch as the first `ABORT` (an inline completion in
   `on_request`, or a wake CQE whose `poll` completes tags; the M11 engine
   completes inline at `lunix-engine/src/queue.rs:517`), libublk is not yet
   `STOPPING`: the `COMMIT_AND_FETCH` is queued and counted in its
   `cmd_inflight`. The kernel accepts it (`ublk_ch_uring_cmd_local` has no
   canceling check), completes the request, and parks the re-fetch as an
   active command that nothing cancels (`ublk_cancel_dev` already ran).
   libublk never reaches `cmd_inflight == 0`, `flush_and_wake_io_tasks`
   returns `Ok(0)` every 20 s for ever, the thread never ends, and
   `quiesce()` returns `DrainTimeout` after 30 s with the device frozen
   (canceling). A second `QUIESCE_DEV` cancels the parked commands and
   releases it, which `testd` (3 attempts on `SIGUSR2`) and
   `run-crash-loop.sh` (one retry) would do; `run-raw.sh:95-97` would
   declare a wedge. It did not happen in 23 recorded handovers: the test
   engine never completes inline and the backing disk was slow, so queue
   threads were mostly idle. With the M11 engine under load it will.
3. **A 20 s stall on the way out.** If a wake CQE is handled in the batch
   that takes libublk to "done" and `poll` has nothing to commit, `on_wake`
   re-arms (`queue.rs:429`), the submission queue is not empty, libublk
   skips its `QueueIsDown` test and blocks in `submit_with_args` for its
   20 s idle timeout before the loop can leave. Applies to detach too.

Related: the kernel does not cancel an idle command whose request is
already started (`ublk_cancel_cmd`: "this uring_cmd is canceled next
time"), so a `DrainTimeout` can also come from the driver alone and is
cured by a second `QUIESCE_DEV`.

Fix (inside `queue.rs` and `device.rs`):
- Keep the layer's own `down` flag, set in `on_cqe` on the first `ABORT`
  (mid-batch), and leave the loop on `down && in_flight == 0` after one
  final submit of the SQ, without consulting libublk's `cmd_inflight`.
  This removes variants 2 and 3.
- Decide what a completion after `down` does and say so:
  *either* (a) a true drain: push the `UBLK_U_IO_COMMIT_AND_FETCH_REQ`
  yourself (`UringCmd16` through `ublk_submit_sqe_sync`, user data marked
  so that libublk does not count it) and do not wait for its re-fetch CQE,
  which the ring's teardown cancels; the kernel then sees every commit and
  the successor has nothing to replay;
  *or* (b) declared reissue: do not commit, count the tag in a separate
  `left_for_reissue` counter (printed by `testd`, exposed in
  `QueueSnapshot`), keep `completed` for real commits only, and change the
  rustdoc, the report and DECISIONS to "requests the server holds at
  QUIESCE are reissued to the successor".
  I recommend (a); (b) is acceptable because phase 0 proved it, but then
  the claim "never exits with started requests" has to go.
- `quiesce()`: on `DrainTimeout` send `QUIESCE_DEV` once more before
  returning the error.
- Test: `testd` option for inline completion (`--workers 0`), and a raw
  QUIESCE cell with it under load; assert the printed
  `left_for_reissue` / uncommitted count (0 for option a) and that the
  handover time stays under a few seconds.

### Majors

**1. A crash during a grow makes the daemon refuse its own device** —
`src/identity.rs:215`, `src/session.rs:163-208`, `src/device.rs:505-520`,
`testd/main.rs:449-451`.

`grow()` makes the engine durable, sends `UPDATE_SIZE`, and the caller then
rewrites `state/device`. `check_identity` demands `kernel.dev_sectors ==
record.dev_sectors`. A kill -9 between `UPDATE_SIZE` and the record write
leaves record = old, kernel = new, engine = new: recovery answers
`REFUSED identity ... size differs` and the mounted file system hangs on a
QUIESCED device for ever (demonstrated with the fake control plane, see the
table). Writing the record first does not help: record = new, kernel = old
is refused too, and `session.rs` test
`a_wrong_queue_count_or_size_is_refused...` (second case) pins that. The
catch-up at `device.rs:422-429` only covers "engine ahead, record equal to
the kernel". `run-resize.sh` kills after the record is written, so no cell
sees the window.

Fix: the size is not identity, the engine is the authority (it is at least
as large as the device, which the check already enforces). Accept
`record.dev_sectors <= kernel.dev_sectors <= engine_sectors`, refuse only
`kernel < record` or `engine < kernel`; keep the catch-up. Add the unit
test and a root cell with a crash point between `UPDATE_SIZE` and the
record write (`ublk.after_update_size`). Plan step 5 and DESIGN §3 item 2
need the same change.

**2. Nothing ties a record to the kernel device** — `src/identity.rs:174-229`,
`src/session.rs:112-120`, `:176-207`.

The check compares the record with itself (volume UUID from the caller's
config) and the kernel device's geometry. Two Lunix volumes with default
geometry and equal size are indistinguishable. A record that is stale in
the same boot (its device was detached and the id reused), or whose
`dev_id` was mutated, passes every check against another volume's device.
`START_USER_RECOVERY` then answers `EBUSY` only while that device's own
daemon is alive; inside its restart window (which the 30 s `EBUSY` retry
actively waits for) the wrong daemon wins and serves volume A's data under
volume B's mounted file system. `STATE_LIVE` is accepted at
`identity.rs:188`, so a healthy foreign device is not even refused up
front. `mkrec.py` has a `dev_id=` knob that no cell uses.

Fix: `ublksrv_ctrl_dev_info.ublksrv_flags` is a 64-bit field the driver
stores at `ADD_DEV` and returns unchanged (`memcpy(&ub->dev_info, &info,
...)`, never touched again). Set it to a tag derived from the volume UUID
(for example the XOR of its two halves), check it in `check_identity`; no
record format change is needed. Also re-run the whole `check_identity` on
the view read after `START_USER_RECOVERY` (today only state and size are
re-checked, `session.rs:195-206`). Add the unit test and a root cell: two
devices of equal geometry, record of one pointed at the other, refused.
DESIGN §3 "ublk has no owner tag" should be reworded (below).

**3. An in-process `DEL_DEV` of a live device is a `STOP_DEV`** —
`src/device.rs:228-237`, `tests/device_e2e.rs:119-123`,
`tests/root/tdlib.sh:103-104`.

Kernel `ublk_ctrl_del_dev` → `ublk_remove` → `ublk_stop_dev_unlocked` →
`ublk_force_abort_dev`, which for a LIVE device sits in
`ublk_wait_tagset_rqs_idle` (uninterruptible `msleep` loop, under
`ub->mutex` and the global `ublk_ctl_mutex`) until every started request is
completed. That is the phase 0 deadlock: if the process that sent the
command also serves the device and is not completing those requests, it
cannot exit (its io-wq worker is stuck, so `/dev/ublkcN` never closes),
kill -9 does nothing, and every `ADD_DEV`/`DEL_DEV` on the host blocks.
`delete_by_id` is public, documented as "works on devices in any state",
and the e2e `Dev::drop` calls it from the serving process when a test
panics and `detach` fails, which is when an engine or the layer is most
likely to be holding requests. `teardown` calls `del_dev` even when
`stop_daemon_clean` reported that the daemon is still alive (out of
process, so the daemon can still be killed by hand, but it is a plain stop
of a LIVE device).

Fix: document on `delete_by_id` and in `CtlOp::DelDev` that `DEL_DEV` of a
LIVE device is a stop (W1/W2) and must never be sent by the serving
process; make `delete_by_id` refuse `STATE_LIVE` unless a `force` argument
is given (the harness `del` passes it only for devices without a daemon);
in the e2e `Drop`, replace the fallback by `std::process::abort()` (the
device goes QUIESCED and the script's sweep deletes it from another
process, which does not wait); in `teardown`, skip `del_dev` when
`stop_daemon_clean` failed. Add the rule to the W list (wording below).

**4. No way to persist the record before `START_DEV`; failures after the
gate leak an anonymous device** — `src/device.rs:171-178`, `:402-430`,
`testd/main.rs:401-403`.

`record()` exists only on a started `UblkDevice`. A daemon that dies
between `START_DEV` and the record write leaves a LIVE-then-QUIESCED device
nobody can recover; udev's probe then hangs in D state on it (the first
incident of the report). If `START_DEV` or `END_USER_RECOVERY` fails or
times out, `start()` returns `Err` while the queue threads keep their
fetched commands, the `UblkDevice` is dropped, and the error carries no
device id: `device.rs:177` says "the device id is in the error text" but
`UblkError::Ctl` has only `op` and `errno`. The catch-up `UPDATE_SIZE`
failure (`device.rs:424-429`) likewise throws away a device that is
already serving.

Fix (this is the main M12 gap): split the attach so the caller can persist
between `ADD_DEV`/`SET_PARAMS` and `START_DEV` (a `persist:
impl FnOnce(&DeviceRecord) -> Result<()>` argument, or `prepare()` →
`record()` → `start()`); a device that was added but never started is DEAD
and harmless. On a `START_DEV` *error* (not a timeout) of a fresh device,
delete it (state DEAD, the delete cannot block). Put `dev_id` into
`UblkError::Ctl`/`Timeout`. Make the catch-up failure a returned warning
next to the device, not an `Err`.

**5. `exec_buf` frees the command buffer while the kernel may still write
to it** — `src/ctl.rs:259-265`, `:317`, `:355-358`.

Every command except `GET_DEV_INFO*`/`GET_FEATURES` is punted to io-wq
(`ublk_ctrl_uring_cmd_may_sleep`). If `submit_and_wait` returns `EINTR` (a
signal on the helper thread; the crate does not retry), `raw_cmd` returns
`Err`, `Box::from_raw` frees the buffer, and the io-wq worker later does
`copy_to_user` into freed heap (`ADD_DEV`, `GET_PARAMS`), while the caller
is told that a command failed which then succeeds (a device it does not
know about). The `SAFETY` comment ("the command has returned") is wrong on
this path. Queue threads block all signals; helper threads do not, so it
depends on the daemon's mask (`testd` blocks its five signals early, which
is why the cells never saw it). `unsafe-safety-comment`.

Fix: block all signals at the top of the helper closure, loop on `EINTR`
in `raw_cmd` (re-enter the wait, do not resubmit), and on any error after
a successful submit leak the box instead of freeing it.

### Minors

6. **Lifecycle is not latched after `QUIESCE_DEV`.** `device.rs:542-558`:
   after `Ok`, `QuiesceTimeout` or `DrainTimeout` the device is canceling
   but `check_serving` still passes, so `grow`, `try_stop` and `detach`
   are accepted. `TRY_STOP_DEV` runs the same stop path; today it is
   protected only by "no openers ⇒ no started requests". Add
   `Lifecycle::Quiescing` and refuse everything except `quiesce` and
   `crash_for_recovery`. `engine.quiesce()` at `:556` has no bound while
   the device is frozen; give it a watchdog that returns the error.
7. **`abort()` is only "like kill -9" when core dumps are off.** With
   `mlockall` and `queues × depth × max_io_buf_bytes` of buffers a core dump
   keeps `/dev/ublkcN` open for a long time and the successor's 30 s
   `EBUSY` wait can expire. `harden_process` should set `RLIMIT_CORE 0` /
   `PR_SET_DUMPABLE 0` (DECISIONS already asks for it), or the layer's doc
   must make it a caller obligation.
8. **`add_device` leaks the device if the read-back fails**,
   `session.rs:127` (no delete, unlike the `SET_PARAMS` branch).
9. **`STATE_LIVE` is accepted for recovery** (`identity.rs:188`), the plan
   says "QUIESCED or recovering". It is needed for the kill-then-restart
   race and should stay, but say so in the plan, and with major 2 refuse a
   LIVE device whose `ublksrv_pid` is a living process other than a
   previous instance.
10. **Test gaps.**
    - `queue.rs` request decoding and `Comp` (op mapping, FUA only on
      WRITE, length limits, the `EINVAL` reject, double completion) have no
      non-root test. Move the decode into a pure function of
      `(iod, buf_bytes)` and unit-test it (`test-mock-traits`).
    - `tests/device_e2e.rs:361-363`: the comment promises a comparison of
      the engine's snapshot with the oracle; the code checks its length.
      The cycle would pass if FUA were dropped or WRITE_ZEROES became
      DISCARD: assert `fua_writes`, `flushes`, `discards`, `write_zeroes`
      against the generated counts and the engine's own counters.
    - `MemBlockEngine` signals its wake fd from the queue thread, so the
      "completion while blocked in `io_uring_enter`" race is only exercised
      by `testd`'s worker pool, never with inline completions (see B1).
    - The drop-writes control drops everything for 0.5 s (fsck rc 4, 97 %
      of the fsyncer ring bad). It shows the harness trips on gross loss,
      not that one lost acknowledged write is seen; the plan asked for
      "drop the last write". A one-shot "drop the next FUA write or the
      next write before a FLUSH" toggle would show the sensitivity.
    - No identity cell with `dev_id=`, no cell for the grow window.
11. **`created-devices` is never truncated**, and ids are reused.
    `run-e2e.sh:16-21` deletes every `/dev/ublkbN` ever recorded, and
    `need_clean_start` reports a stale-recorded id as "registered (this
    run)". A foreign device that happens to hold a recorded id would be
    deleted. Truncate the file after the clean-start check of every script
    (or in the wrapper), and sweep only ids recorded since.
12. **Script details.** `run-raw.sh:42` and `run-crash-loop.sh:152` still
    read fio's `total_err` (prints `errors n/a` / `null`; the checks use
    the exit code, so only the message is wrong). `start_daemon`
    (`tdlib.sh:26`) should run `env -u LUNIX_CRASH_AT` when `CRASH_AT` is
    empty, so an inherited variable can never repeat the second incident.
    The scripts state no cap of their own (W8 asks for one next to the
    wrapper's). A failed start after `ADD_DEV` leaves an unrecorded device
    that only the wrapper's leftovers check sees: print `ADDED id=N` before
    the start so the harness can record and delete it.
13. **Errors.** `Errno`'s `Display` prints the number twice ("... (os error
    16) (errno 16)"). `UblkError::Config` is used for lifecycle refusals
    ("the device is stopping"); a `State` variant would let M12 match it
    (`err-custom-type`).

### Nits

14. `raw_cmd` and `raw_cmd_nr` are the same 25 lines; fold them.
15. `Completer::tag_buf` hands out the full buffer of any tag, started or
    not, and `on_request` passes `buf` and a completer that can alias it;
    both are contract-only. A debug assertion on `Meta::Started` is cheap.
16. `UBLK_IO_F_NOUNMAP` and FUA on WRITE_ZEROES are not conveyed
    (`IoRequest` has no field). Fine for a log-structured engine; note it
    in the iface rustdoc.
17. Every control command, including each 100 ms `EBUSY` retry, spawns a
    thread whose stack is locked by `MCL_FUTURE`. Harmless; a single
    control thread with a queue would be tidier.
18. `UblkConfig::new` defaults to 4 queues while `default_queues()` exists;
    recovery ignores `read_only`/`chunk_bytes` without comparing them with
    `GET_PARAMS`.
19. libublk prints `device reload json failed` on every attach; the report
    notes it. Worth a line in the operator notes for M12.

## The five plan-wording findings

1. **`QUIESCE_DEV` success is not "idle".** Correct: `ublk_wait_for_idle_io`
   only waits until every queue has one idle command. The report's
   description of what the layer does about it is not (B1). Plan step 6 and
   DESIGN §3 "Planned upgrade", after B1 option (a):
   > `QUIESCE_DEV` returns once every queue has an idle command; requests
   > the server already holds stay with it. The daemon completes and
   > commits those, then runs the engine's final commit, then exits; no
   > `STOP_DEV`, no `DEL_DEV`. A request it cannot commit is left for
   > `REISSUE`, which is counted and logged.
2. **libublk's `wait_and_handle_io` ends with requests held.** Correct, and
   incomplete: libublk also drops commits and ignores its own exit test in
   the cases of B1. ADR-18, libublk gaps:
   > libublk 0.4.8 assumes handlers complete at once: after the first
   > `ABORT` it queues no commit and ends its loop. The layer keeps its own
   > queue-down flag and exit condition and commits held requests itself.
3. **A failed `QUIESCE_DEV` is not a no-op.** Correct (the driver sets
   canceling before the wait and clears it only when the char device is
   reset). Plan step 6:
   > On timeout (`EBUSY`/`EINTR`) the device stays canceling: new I/O is
   > requeued, not served. The caller retries `quiesce` or calls
   > `crash_for_recovery()` (abort, recovered by `REISSUE`); it never
   > detaches or deletes the device in this state.
4. **`TRY_STOP_DEV` is Busy for any opener.** Correct (`disk_openers > 0`).
   Plan step 7 and DESIGN "Detach":
   > `TRY_STOP_DEV` answers `EBUSY` while the block device has any opener:
   > a mount, an open file descriptor, or udev's probe just after the
   > device appeared. Detach of a new device retries for a bounded time.
5. **`UPDATE_SIZE` on a DEAD device.** The local refusal is right and
   costs nothing; v7.0 returns `ENODEV` itself (`ublk_ctrl_set_size`). I
   could not check which release candidate fixed it. Plan step 8:
   > `UPDATE_SIZE` is sent only to a device that has a disk (state not
   > DEAD); older drivers dereference a null disk.

Further wording the review needs:
- Plan step 5 / DESIGN §3 identity: "the queue count, depth, buffer size and
  flags equal the recorded values; `record.dev_sectors <= dev_sectors <=`
  the engine's size; `ublksrv_flags` equals the volume's tag; the state is
  QUIESCED, or LIVE with no living server." And: "ublk has no owner
  concept; the daemon stores a volume tag in `ublksrv_flags`."
- New rule next to W1: "**`DEL_DEV` of a LIVE device is a `STOP_DEV`.** It
  is sent only after a successful `TRY_STOP_DEV`, or by another process
  for a device without a server. Never by the serving process as a
  fallback."
- DECISIONS ADR-16: "never exits while started requests are not being
  completed" should name the two sanctioned exits with started requests:
  `abort()` and (if B1 option b is chosen) the exit after `QUIESCE_DEV`.

## Open items of the report

- **`tools/ci.sh` lines for `--features crashpoints`:** add both (clippy
  and test); the feature gates code in `queue.rs` that CI does not build.
- **Mounted cells ran before the rebase:** acceptable, the rebase changed
  no file of the crate or of `lunix-iface`, `lunix-format`,
  `lunix-testkit`. The QUIESCE cells (raw and ext4 ×20) must be re-run
  after the B1 fix, with the inline-completion variant added.
- **`ublk-leftovers.sh`** does not know `lunix-ublk-testd` as a process
  name (lead's file): add it.
- **`DEL_DEV` inside the control watchdog** instead of a separate helper:
  fine, the watchdog is the helper.
- **Internal `df_guard` twin, `sudo` environment, `--io_size` with
  `time_based`:** fine as described.
- **Gaps for M12** (focus 5): record before start and device id in errors
  (major 4); a typed lifecycle error (minor 13); the `left_for_reissue`
  counter (B1); `RLIMIT_CORE` (minor 7); who persists the record after an
  engine-initiated grow through the resize hook (with major 1 fixed the
  order no longer matters, but the daemon still has to do it); completion
  model, flush/FUA, discard/write-zeroes and the errno mapping
  (`IoStatus::raw` passed straight to the driver, short counts requeued by
  the kernel) match what the M11 engine does.

## W1-W9 (scripts)

W1 no stop command anywhere, verified by source and by the opcode test. W2
see B1. W3 every fio run is bounded (`--runtime` plus an outer `timeout`,
or `--io_size`). W4 watchdogs on every control command; no script sends
KILL to a stopping daemon (the kill -9 fallback of `handover` hits a daemon
whose quiesce failed twice, which is the sanctioned crash). W5 every script
unmounts before detach/delete; teardown recovers a daemon first and stops
at a failed `umount`; exception: major 3 / `tdlib.sh:103-104`. W6 no
`BATCH_IO`. W7 `mlockall`, `PR_SET_IO_FLUSHER`, private mount namespace.
W8 wrapper timeout with a 180 s grace; scripts have no own cap (minor 12).
W9 `wedged` marks the run and teardown then touches only its load
processes. The crash-point fixes are right: `CRASH_AT` is set only for the
daemon meant to die, cleared for the last cycle, after the loop and in
`teardown`, and the last crash-point daemon is replaced before the wait
for fio. df-guard runs before every `mkfs`. Leftover check: minor 11.

## rust-skills notes

`unsafe-safety-comment`: one wrong comment (major 5); the others hold.
`err-custom-type`: minor 13. `test-mock-traits`: the `ControlPlane` fake
is a good use; the queue path lacks the equivalent (minor 10).
`num-cast-try-from`: the remaining `as` casts are bounded FFI lengths and
offsets, as the report says. No conflict with CONVENTIONS.

## What I verified by running or reading outputs

- Ran: `cargo test -p lunix-ublk` (56 + 2 pass); the scratch-copy test for
  major 1 (passes, i.e. the refusal happens); two mutations (both caught).
- Read on the host (read only, no `mnt/`): every `results.md`,
  `phases.csv`, `stall-table.md`, `result.txt`, the quiesce daemon logs
  (23 × `QUIESCE ok attempt=1`), the control cells' fsyncer outputs, the
  e2e log, teardown logs.
- Read, not run: all root scripts; libublk 0.4.8 `io.rs`/`ctrl.rs`
  (stopping state, commit path, loop exit, control drop); upstream v7.0
  `ublk_drv.c` (quiesce, cancel, try-stop, stop, delete, recovery,
  set-size, io command path, char release, `ADD_DEV` copy of `dev_info`).
- Not run: any device, mount, fio or root command. B1, major 2 and major 3
  have no run behind them.
- Cleaned up: the scratch copy and the review directory on the host.

## Re-review (branch `p1/m10-ublk` at a289508, rebased onto main 9d892a7)

Targeted at the fix diff (`b7f99a4..a289508`, crate and report), read against
libublk 0.4.8 and upstream v7.0 `ublk_drv.c` again. Same limits: no root run;
the root cells were read from their recorded outputs.

### Verdict: **merge**

The blocker and the five majors are fixed, and the fixes are sound. One new
finding (R1) should be closed in a small follow-up before M12 calls
`recover`; the rest are minors.

### The original findings

| item | status |
|---|---|
| **B1** drain | **fixed, sound.** Own `down` flag, set mid-batch on the first `ABORT` (`queue.rs:499-502`), on libublk's `QueueIsDown`, and through a per-queue stop fd that `quiesce()` signals only after `QUIESCE_DEV` returned `Ok` and `try_stop()` only after `TRY_STOP_DEV` succeeded (`device.rs`, both sites). Once down the loop never blocks in libublk (`to_wait = 0`, 50 ms engine poll) and leaves on `down && in_flight == 0`. Held requests are committed with the layer's own `COMMIT_AND_FETCH_REQ` (`queue.rs:614-641`; `ublksrv_io_cmd` layout and `FIXED_FILE` index are right, `addr` is the tag buffer in copy mode and 0 with USER_COPY, as `ublk_check_commit_and_fetch` wants). |
| | *Kernel side:* the commit is executed inline at submit (`ublk_ch_uring_cmd_local`, no canceling check), so it is done before `io_uring_enter` returns; the re-fetch half parks as an active command and is cancelled by a later `ublk_cancel_dev` or the ring's teardown. *No double commit:* a tag leaves `Meta::Started` before either commit path and each completion takes exactly one path. *No commit to a reissued tag:* `down` is only ever set when the device is canceling (quiesce) or its disk is gone (try-stop), so `ublk_queue_rq` dispatches nothing to a parked command; reissue happens only after the char device closes, when this process no longer commits. A request already started when canceling was set is delivered normally and drained (its CQE is in the ring before the thread can observe `down`, and libublk cannot report "done" while it is unreaped). The fully-busy queue that gets no `ABORT` (the implementer's own find, c76bbac) is real (`ubq_has_idle_io` counts un-started requests) and the stop fd covers it. |
| | *Evidence:* held-request cells, final tree: 10 mounted handovers with 49 to 131 drain commits each and 3 raw ones (12 to 16), every one with sysfs `inflight` `0,0` read before the exit (started requests held by the server do count there, so this does show the commits landed), 0.4 to 0.8 s; inline engine 10 + 3 cycles, 50 to 220 ms; default 20 of 20. The pre-c76bbac run shows the fully-busy case as three 30 s handovers, gone afterwards. `quiesce()` re-sends `QUIESCE_DEV` once on a drain timeout. |
| **M1** grow window | fixed: `record <= kernel <= engine`; my scratch scenario is now a unit test that recovers; root cell B kills at `ublk.after_update_size` and recovers. |
| **M2** identity | fixed: `volume_tag` in `ublksrv_flags` at `ADD_DEV`, checked before any mutating command and again (whole check) after `START_USER_RECOVERY`; root cell D refuses A's record on B's device and both stay unchanged. See R1 for the LIVE part. |
| **M3** `DEL_DEV` | fixed: `delete_by_id(.., force)` refuses LIVE; e2e `Drop` aborts; `teardown` deletes only after the daemon is gone; cell A. |
| **M4** persist/leaks | fixed: `add_persisting` persists between `SET_PARAMS` and `START_DEV` and deletes the DEAD device if the closure fails; a non-timeout `START_DEV`/`END_USER_RECOVERY` error releases the queues through the stop fd, closes the char device and deletes a fresh device; a timeout touches nothing; `UblkError::Device` carries the id; the catch-up failure is a warning. Cell C. See R4. |
| **M5** `EINTR` | fixed: helper threads block all signals, `EINTR` re-enters the wait without resubmitting, the buffer is leaked on any error, the comment is right. |
| minors 6-13, nits | done as the report lists (lifecycle latch, core dumps off, read-back delete, pure `decode_request` with tests, e2e counter assertions, `created-devices` truncated, script caps, `env -u`). `run-raw.sh` still prints `errors n/a` for the raw fio runs (message only). |

Re-run: `cargo test -p lunix-ublk` and the same with `--features
crashpoints`: **70 + 2 pass** both ways, as reported. Recorded final cells:
raw 59/0, lifecycle 22/0, mounted 14/0, resize 37/0, e2e 6/6, kill -9 short
8+4+2 clean, crash points 4+4 clean. The 36-cycle ext4 / 10-cycle xfs runs
and the two original negative controls are from the pre-fix tree, as the
report says; acceptable, the kill path changed only in loop structure.

### New findings

**R1 (fix before M12 uses `recover`). A live server is a transient state but
is reported as an identity refusal** — `identity.rs` (`LiveServer`),
`session.rs` `begin_recovery`. `check_identity` now returns
`IdentityMismatch::LiveServer` when the device is LIVE and `/proc/<pid>` is
not a zombie, and `begin_recovery` maps it to `UblkError::Identity` at once,
outside the 30 s `EBUSY` retry. The documented reaction to `Identity` is "add
a new device", which would orphan a mounted file system on the old one.
The state is LIVE only for the moment between a server's death and the
driver's release work, so this needs a successor that looks while the old
process is not yet a zombie (a supervisor that does not reap first, or an
exec-in-place upgrade, where the recorded pid is the caller's own), or a
reused pid. The harness always `wait`s for the old process, so no cell can
see it. Fix: keep the check, but treat `LiveServer` like `EBUSY`: retry it
inside the `recovery_wait` loop and end with `UblkError::Busy`; treat
`pid == getpid()` as not alive. Unit test with a fake whose server "dies"
after n polls.

**R2 (minor). The result of a drain commit is never looked at** —
`queue.rs:488-492`, `:408-412`. A successful drain commit produces no CQE
until it is cancelled (`ABORT`); a CQE with any other negative result means
the driver rejected the commit and the request was not completed, yet it is
only counted in `drain_cqes`. Likewise the two `flush_and_wake_io_tasks(.., 0)`
calls discard every error, including a failed submit of the last commits.
Either case ends in a silent reissue, which is safe but is exactly what the
drain claims not to do. Make a drain CQE with a result other than `ABORT`,
and a flush error other than `QueueIsDown`, `fatal` (abort, recovered by
`REISSUE`), and have the held cells assert it did not happen.

**R3 (minor). The resize hook outlives the device** — `device.rs`
`ResizeBridge`. After `detach()` (and after a failed attach) the engine
still holds the hook with the old `dev_id`. A later `engine.grow` would send
`GET_DEV_INFO`/`UPDATE_SIZE` to whatever device has that id by then. Make
the bridge refuse unless the lifecycle is `Serving`, and install a no-op
hook on the failure paths.

**R4 (minor). A crash between `persist` and `START_DEV` leaves a DEAD device
that nothing deletes.** The next start gets `Identity { State { 0 } }` and
adds a new device, as documented; the old one stays until reboot. It is
harmless, but it is the layer's own device (the tag matches): either have
`recover` report this case as a distinct "never started" refusal and delete
it, or tell M12 to call `delete_by_id(id, .., false)` on it.

**R5 (minor). `quiesce()` does not latch `Quiescing` when `QUIESCE_DEV` hits
its watchdog** (`UblkError::Timeout`): the command may still be pending and
set canceling later, while `grow`, `try_stop` and `detach` stay allowed.
Latch on `Timeout` too. Also: a retry after an engine-quiesce timeout calls
`engine.quiesce()` a second time while the first call may still run.

**R6 (the drop-one-FUA control).** It cannot trip, by construction, and
that is not a defect of the layer. A daemon kill is not a power loss: the
file system keeps running and is unmounted cleanly, so a lost journal block
(what ext4 sends with FUA) is never read back; the fsyncer's data goes out
as plain writes followed by a FLUSH. The log shows the fault armed three
times, not that a write was actually dropped. I agree with the report's
conclusion (gross loss is detected, no claim of single-write sensitivity).
Two cheap ways to get the missing sensitivity where it can exist:
(a) an e2e negative control on the raw device, where `DeviceOracle`
compares every byte: a `MemBlockEngine` fault that acknowledges and drops
one write must fail the cycle; (b) in the mounted loop, drop the next plain
4 KiB write to the fsyncer's file range rather than a FUA write. Durability
of FUA/FLUSH against a crash with volatile state belongs to M15, and the
plan's M10 gate line "with the drop-last-write toggle, verify fails" should
say "drop-all" for the mounted cells and point at (a).

### Wording

The report's eight wording items are right and match the code. Add to
item 6: "a LIVE device with a living server is `Busy` after the recovery
wait, not an identity refusal" once R1 is fixed.

### What I ran or read

Ran the non-root tests twice (default and `crashpoints`). Read the whole
fix diff of `queue.rs`, `device.rs`, `ctl.rs`, `watchdog.rs`,
`identity.rs`, `session.rs`, `harden.rs`, the script and e2e diffs, and on
the host (read only) the final `results.md`, `phases.csv`, `handover.csv`
and quiesce reply files. Nothing with a device was run by me. The review
directory on the host is deleted; no scratch copy was needed this round.
