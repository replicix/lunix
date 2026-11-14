# Review: phase 1 milestone M12a (`lunixd`)

- Reviewed: branch `p1/m12a` at `0aa46f1`, base `30e4329`, `crates/lunixd` only, plus the report
  `docs/plans/v1/reports/p1-m12a.md`.
- Method: reading only. Nothing was compiled or run, locally or on any host. Claims that only a run can settle are
  in "Needs a run".
- Read against: the M12 section and "M12 inputs from the M9-M11 reviews" of the phase 1 plan, rules W1-W9 and
  cross-cutting rule 13, DESIGN §3 and §4.4, on-disk-formats §20, and the code of `lunix-ublk`, `lunix-engine`
  (recovery, C6-C7), `lunix-store` (pins, sweep), `lunix-crypto` (`check_attach`), `lunix-journal` (ring open) and
  `lunix-iface::ctl`.

## Verdict: merge after fixes

No blocker. I found no path that loses acknowledged data and no path that sends a stop command to a served device.
The recovery order, the device record ordering, the R4 placement and the destage adapter are right.

Two majors are in the shutdown path (`QuiesceAll` and the signals), which is the upgrade path the harness and M15
will lean on. Both end in a daemon that holds frozen devices and can only be killed. That is recoverable, but it
is not the behaviour the log reports. Fix both before M15 starts.

The plan's control gate is met only in part: the distinct `Rollback`/`Fork` codes, the floor in `VolumeStatus`
and the first-attach notice wait on iface requests A-C. The report says so. Land A-C, then switch the mapping and
the tests.

## Blockers

None.

## Majors

### M1. A partly failed `QuiesceAll` strands the devices it already quiesced

`crates/lunixd/src/daemon.rs:870-899`.

`quiesce_all` walks the slots in `HashMap` order and returns on the first failure with `After::Continue`. Every
device quiesced before that one stays quiesced: its queue threads have ended and the engine has made its final
commit. Nothing serves it again in this process. The log line says "the daemon keeps serving", and `VolumeStatus`
still says `attached`.

Two ways to get there:

- A real `QuiesceTimeout` or `DrainTimeout` on one busy device, with others before it in the walk.
- A volume whose earlier detach half-failed. After a `DrainTimeout` or a `DEL_DEV` timeout the slot stays
  `Attached` with the layer's lifecycle at `Stopping`. `UblkDevice::quiesce` then answers
  `State("the device is stopping")` on every call (`lunix-ublk/src/device.rs:754-760`). From then on no
  `QuiesceAll` and no `SIGTERM` can ever succeed, and each attempt freezes the healthy volumes walked first.

Consequence: mounted file systems on the healthy volumes hang until someone sends `SIGKILL`. No data is lost
(the journal and `REISSUE` cover it), but the upgrade path silently turns into a crash, and the operator is told
the opposite.

Suggested fix:

- Walk every device and collect the failures instead of returning on the first.
- Treat a device that is already `Stopping` or `Deleted` as nothing to quiesce (no started requests exist).
- If any device was quiesced and another failed after a bounded retry, do what DESIGN §3 names: retry, then
  `abort()` for recovery. Never return to "serving" with quiesced devices.
- With a single device the plan's "keep serving and log" is still wrong in wording: M10 documents that the device
  refuses new I/O after a timeout. Say that in the log.

### M2. `QuiesceAll` over the socket loses its exit when the call outlives `call_timeout`

`crates/lunixd/src/server.rs:157-192`, `daemon.rs:890-898`.

`run_call` answers `internal` after `call_timeout` (600 s by default, 120 s in the tests) and drops the receiver.
The call thread keeps running. When `quiesce_all` then finishes, its `After::Exit(code)` goes into a closed
channel. The daemon is left with `exiting = true`, every slot mutex locked for good (`mem::forget`), every device
quiesced, and no exit.

A later `SIGTERM` runs `quiesce_all` again and blocks for ever on the forgotten guards
(`server.rs:218-233`). Only `SIGKILL` ends the process.

The worst case per device is about 240 s (two `QUIESCE_DEV` waits of 30 s, two drains of 30 s, the engine watchdog
of 120 s), so three slow volumes pass 600 s. A lower `call_timeout_ms` in a config reaches it sooner.

Suggested fix: the exit must not depend on the reply. Have `quiesce_all` hand the exit code to the server through
a channel the `Daemon` owns, and exempt `QuiesceAll` from the per-call timeout (its own watchdogs bound it).

## Minors

### m1. `Attach` can race a running quiesce and leave a live, unquiesced device at exit

`daemon.rs:871-890`, `daemon.rs:317-319`.

Slots are created lazily and `exiting` is set only after the last device is quiesced. An `Attach` of a volume with
no slot yet, arriving during the quiesce, creates its slot after the snapshot, brings a device up, and the
process then exits with that device live and never quiesced (rule W2; recoverable like a kill).

Fix: set a "quiescing" flag before the snapshot that refuses mutating calls (clear it on failure), and create the
slots of all registry entries at start.

### m2. Recovery runs before the socket is bound and before the signals are handled

`crates/lunixd/src/main.rs:111-112`, `server.rs:210-215`.

- If `bind` fails after `recover_on_start` (another daemon answers, the path is not a socket, the directory cannot
  be made), `serve` returns an error and the process exits by the normal path with the devices it has just
  recovered still live. They go back to QUIESCED. `UblkDevice` has no `Drop`, so no stop is sent, but W2 is not
  kept.
- A `SIGTERM` during `recover_on_start` takes the default action for the same result. With several volumes the
  window is long, because the post-recovery commit runs inline (m6).

Fix: probe and bind the socket first (do not accept yet), install the signal handling, then recover.

### m3. After `UblkError::Device` with the device left in the kernel, the daemon keeps running

`daemon.rs:639-648`, `lunix-ublk/src/device.rs:598-610`.

M10's contract for a `START_DEV` timeout is "the caller reports and exits". The daemon logs and serves on. The
orphan queue threads keep the engine and so `LOCK`: the volume shows `detached`, every new attach answers `busy`,
and if the pending `START_DEV` completes later the device is live with no slot. `QuiesceAll` then does not know
it and exits past it.

I checked the worse case, a retried attach reaching `delete_dead_own_device` on that device: the held `LOCK`
stops it before `start_ublk`, so no `DEL_DEV` is sent by the serving process.

Fix: keep a tombstone (volume, device id) for such a device, show it in `VolumeStatus`, refuse `QuiesceAll`
success while one exists, and consider the exit M10 asks for. The log line also says "left in the kernel" for the
cases where M10 already deleted the fresh device (`Cleanup::Delete`).

### m4. A stale `state/device` re-attaches a volume the operator detached

`daemon.rs:738-740`, `daemon.rs:905-925`.

`clear_device_record` runs after `DEL_DEV`, and its failure is only a warning. A crash between the two, or a
failed unlink, leaves the record. At the next start in the same boot `recover` answers `Identity { NotFound }` and
`start_ublk` adds a new device: the volume is attached again without anyone asking.

Fix: in `recover_on_start`, treat `NotFound` in the same boot as "detach had completed": clear the record and
leave the volume detached.

### m5. Files are written before R4 passes

`stack/mod.rs:674-681`; `lunix-journal/src/ring.rs:194-262`; `lunix-index/src/lib.rs:336`.

DESIGN §4.4 step 3 says nothing is written before the R4 check. `open_volume` opens the journal and the index
before `precheck_fork` has an authenticated chain and before `Engine::open`:

- `open_rings` creates and preallocates missing ring files (an attach with more queues than lanes creates
  64 MiB files) and deletes `*.j.tmp`.
- `LsmIndex::open` deletes its temporary files.

Nothing referenced is touched, so a refused volume loses nothing. It is still a deviation, and with
`on_journal_damage = refuse` the directory handed to `fsck` can hold new ring files.

Fix: split the journal open into "scan" and "create missing", or document the exception in §4.4.

### m6. The post-recovery commit is inline, and its condition misses the case it was meant for

`daemon.rs:654-660`.

- With `replayed > 0` the commit destages the whole replayed journal inside `bring_up`. `recover_on_start` is
  sequential, so the devices of the following volumes stay QUIESCED for that long and the socket is not bound.
  The coordinator's own triggers would do the same work in the background.
- The M11 note is about a crash between C6 and C9. There every record is at or below the tip's cut, so
  `replayed == 0` and nothing is called. A call would not help either: `commit_now(false)` returns `Idle` without
  reclaiming (`lunix-engine/src/coord.rs:271-274`). `fill()` stays overstated, which also feeds the adapter's
  pressure choice for the first cut.

Fix: drop the inline commit or move it after all volumes are recovered; ask the engine for a reclaim-only call
for the `replayed == 0` case (or accept the overstatement and say so).

### m7. The interim credential rule: fail-closed, but one gate line is untested and one volume kind is stranded

`stack/mod.rs:562-582`.

I looked for a downgrade or confusion path and found none. The cases:

| descriptor | pin | request | result |
|---|---|---|---|
| swapped to plain | any | key material | `Downgrade` (rule 1) |
| swapped to plain | integrity or encrypted | none | `Downgrade` (rule 2) |
| swapped to integrity, pin rewritten to integrity | integrity | passphrase | `WrongCredential`: the attacker cannot make a slot for a secret they do not have |
| integrity | encrypted | none, own key present | `Downgrade` (rule 2) |
| encrypted | integrity | key material | `ModeMismatch` |

The pin is unauthenticated (CRC only) and sits in the volume directory, so it is a weak source for "what the
caller expects". It never widens what attaches, because every keyed path still has to unwrap a slot.

What remains:

- The plan's gate line "an unencrypted volume with an encryption credential returns `Downgrade`" holds for plain
  only. An integrity volume given a passphrase on its own host answers `wrong_kmk`. The test covers plain only
  (`tests/control.rs:434-455`).
- An integrity volume with a passphrase slot cannot be attached on a host without its pin (moved volume, lost
  `state/pins`): `Downgrade`, with no way to re-pin. The report says so (decision 1).

Both go away with request D. Until then the README should say it.

### m8. A corrupt registry is overwritten with whatever a rebuild finds

`daemon.rs:289-302`.

Any load error (an I/O error too, not only a format error) leads to `rebuild` and `save`. With no `volume_roots`
the saved registry is empty. The old file is gone, `recover_on_start` then finds nothing, and devices of mounted
volumes stay QUIESCED with no hint why.

Fix: rebuild only on a format error, rename the bad file aside first, and refuse to start when the file exists,
does not parse and no root is configured.

### m9. The control server has one bounded pool for connections and for the signal's quiesce

`server.rs:200-205`, `server.rs:222`, `server.rs:254`.

Each connection holds a blocking thread with no read timeout; the pool has 64. Sixty-four idle clients (root or
the socket group) block new connections and also the `spawn_blocking` that a `SIGTERM` needs.

Fix: run the signal's quiesce on its own `std::thread`, and give connections a read timeout.

### m10. Attach geometry has no product bound, and a bad one repeats at every start

`lunix-iface/src/ctl/msg.rs` (`Attach`), `daemon.rs:579-605`.

`queues <= 16`, `depth <= 4096` and `max_io_buf_bytes <= 32 MiB` are each checked, but 16 × 4096 × 1 MiB is
64 GiB of locked buffers. The record is persisted before the queues allocate, and `bring_up` takes the geometry
from the record of the same boot even when the device is not recoverable. An out-of-memory kill on such an attach
therefore repeats at every daemon start until someone removes `state/device`.

Fix: bound the product against a config cap before `ADD_DEV`; use the record's geometry only for a device that
is actually recovered.

### m11. `lunixd ctl` takes secrets on the command line

`main.rs:52-58`, `main.rs:67-84`.

The request JSON, passphrase included, is an argument (visible in the process list) and the reply buffer is not
zeroized. It is a stand-in until M13a, and the smoke uses no passphrase. Read the request from stdin, or say in
the README that `ctl` must not carry secrets.

### m12. A new volume has no floor, so its first attach reports "first attach"

`stack/mod.rs:388-394`, `daemon.rs:612-614`.

`create_in` writes the pin but no floor. The notice (and request C's `first_attach`) is then true for every newly
created volume, where it carries no information. Record the floor at the genesis commit in `create_in`.

### m13. Lanes can never shrink, even after a clean detach

`daemon.rs:584-592`.

A volume once attached with 8 queues refuses fewer for ever, also when the journal is empty after a clean
detach. Acceptable for phase 1, but say it in the README, or let a clean journal be re-laid.

### m14. The adapter's log line for a failed cut overstates what the sweep does

`stack/destage.rs:133-149`.

- It names every segment of the failed cut, including those M9 deleted, so every such failure warns.
- "a survivor is swept at open" holds only while no later commit names a higher id: the sweep deletes only above
  the high-water mark (`VolumeDir::sweep_orphans`). A survivor below the mark waits for GC and is outside the
  space ledger until the next open.

Fix until request I lands: intersect the ids with `ctx.segments.list()` and log only what still exists.

## Nits

- `daemon.rs:468-471`: `VolumeList` silently drops a registered volume whose descriptor cannot be read. List it
  with an error state.
- `lunix-iface/src/ctl/msg.rs` `valid_name`: `.` and `..` pass. `make_link` fails harmlessly on them; refuse them.
- `server.rs:58-63`: the socket is bound, then `chmod`ed. With a permissive umask there is a window in which
  others can connect, and a connection made then is served later. Set the umask around `bind`, or bind inside a
  0700 directory.
- `server.rs:102-105`: a malformed payload is parsed into a `serde_json::Value` that is not zeroized.
- `audit.rs:127-132`: the audit line is appended without `fdatasync`, and requests refused by `validate` are not
  audited.
- `daemon.rs:376-431`: the registry mutex is held across Argon2 and the whole create, so every other call waits.
  On a registry failure the family keyring directory is left behind.
- `stack/mod.rs:613-619`: `precheck_fork` passes on any read error of the floor commit. The engine fails on the
  same read, so it is sound; match `NotFound` only, for clarity.
- `stack/mod.rs:536-539` against `lunix-store/src/volume.rs:515-520`: the daemon treats a rejected `state/pins`
  as absent (as §20 says), the store as an error. The attach then fails closed in `precheck_fork`, which is the
  better outcome; make the two agree and say which one the spec means.
- `daemon.rs:579-583`: `queues`, `depth` and `max_io_buf_bytes` of a request are ignored without a word when a
  record of the same boot exists.
- `daemon.rs:106`: journal damage under `refuse` is code `io`. The CLI cannot tell the operator to run `fsck`
  except from the message; add a code with request A.
- `Cargo.toml`: `crashpoints` does not forward to `lunix-index` (it has no such feature). See "Needs a run".

## Focus areas

### 1. Recovery and attach order

Right. Start: harden, registry, `recover_on_start`, then the socket. Per volume: unlocked reads, `check_attach`,
`LOCK`, fork pre-check, journal and index open, `Engine::open` (chain, R4, sweep, catch-up, journal, floor), pins,
then the device.

- M10 R4 is done as asked: `delete_dead_own_device` re-reads the device, requires DEAD and this volume's tag, and
  calls `delete_by_id(.., false)`, which itself refuses a LIVE device (`device.rs:312-340`). A device that is DEAD
  because a detach crashed after `TRY_STOP_DEV` is deleted the same way, which is correct.
- The record is written through `add_persisting`, between `SET_PARAMS` and `START_DEV`, durably.
- A grow persists `record()`; the size is not identity in M10, so a crash between the two is recoverable.
- Signals quiesce and exit through `process::exit` with no destructors. No stop command exists on any path.
- Problems: m2, m3, m4, m6, and M1/M2 for the exit.

### 2. R4 and ADR-31

- **Rollback and equal-height fork**: the engine's, after `verify_chain`, before any write of its own. Tested at
  the stack level and over the socket in all three modes.
- **The pre-check is sound.** It uses the same tip the engine will use (`CommitStore::tip`, no fallback),
  compares the raw bytes at the floor's height with the recorded hash, and leaves every other case to the
  engine. Because the engine then verifies the chain from the tip down to `chain_floor`, and the pre-check only
  applies when the floor's height is at or above `chain_floor`, a tip that passes is a descendant of the floor
  commit. Deleting the commit at the floor's height does not bypass it: the engine reports a broken chain.
- **It closes the engine gap fully, but the gap the spec leaves is wide.** §20 lets the attach proceed when the
  floor's height is below the tip's `chain_floor`. With the default `retain_last = 2`, `chain_floor = tip - 1`,
  so the fork check exists only for a tip at the floor or one above it. A diverged copy that is two or more
  commits ahead of the floor attaches silently and the floor is raised to it. In plain mode anyone can write
  such a chain; in the keyed modes it is the "copy advanced elsewhere and was copied back" case the fork check
  exists for. The test stops at tip 4 over floor 3, exactly the last detectable height
  (`tests/wiring.rs:279-290`).
  This is a spec matter, not a code defect; see "Spec gaps" for a proposal.
- **Floor raise ordering**: after the journal recovery in `recover` (never ahead of a durable commit), and after
  C6 and the checkpoint in the coordinator. A failed raise after C6 is a warning, which is safe. `record_pins`
  cannot lose a concurrent raise: the store serialises on `pins_lock` and re-reads the floor.

### 3. Credentials and downgrade

See m7. Secrets: the frame is zeroizing, `AttachCredential` has a redacting `Debug`, the audit line carries only
method, volume, outcome and the error message, and no error string formats a secret. The key file is opened with
`O_NOFOLLOW | O_NONBLOCK` and must be a regular file of 32 bytes. Exceptions: m11 and the unzeroized `Value` nit.

### 4. The destage adapter

- The class mapping is one exhaustive `match` with no wildcard, and `Retry::classify` is not called again.
- `freed_bytes` is not released. `index_inserts` are flattened in segment order. The cut is borrowed.
- Pressure comes from `Journal::fill()` per cut; the thresholds (0.75, 0.9) are the implementer's and need the
  maintainer's decision (R7).
- Undeleted segments: m14 and request I.
- `map_fanout` comes from the descriptor. `bound_cut` against `dirty_leaf_count` is untouched (the engine's).

### 5. Control plane

Framing is bounded before allocation, the version and `validate` run before dispatch, the socket is 0600 or
0660 with a group, detach answers `Busy` and changes nothing. Names are restricted to a safe charset, so the
symlink path cannot be steered (apart from the `.`/`..` nit). `dir` and `key_file` are arbitrary absolute paths
read or created as root: acceptable for a root socket, worth one line in the README for `socket_group`.
Problems: M2, m9, m10.

### 6. Journal damage

The policy reaches the engine, which logs before any lane opens and returns before the floor raise under
`refuse`. Existing journal files are not modified (`open_rings` says so and the test compares bytes). The orphan
sweep and the index catch-up have already run by then, and new ring files may have been created (m5).

## Requests A-I

| | request | recommendation |
|---|---|---|
| A | `Rollback`, `Fork` in `ErrorCode` | **Accept.** The plan's gate needs them. Add a code for journal damage under `refuse` in the same change. Old clients see `unknown` (exit 1), which is acceptable. |
| B | The floor in `VolumeStatusBody` | **Accept, modified.** Show it for a detached volume too: `state/pins` is readable without keys. `Daemon::rollback_floor` returns `None` when detached today. |
| C | `first_attach` in `Attached` | **Accept, modified.** Take it from the engine's `RecoveryReport::first_attach`, and fix m12 first, or it is true for every new volume. |
| D | Expected mode in `Credential` | **Accept.** Then delete the pin-derived rule, default key material to `encrypted`, and add the missing gate case (integrity volume, encryption credential, `Downgrade`). |
| E | Per-attach `on_journal_damage` | **Accept.** The plan calls it a mount option. Keep the config value as the default; `recover_on_start` has no request to read it from. |
| F | Engine fork check above the floor | **Accept.** The diff is right and better placed than the pre-check (after `verify_chain`). Remove `precheck_fork` when it lands and keep its test. |
| G | The tip's `created_at` | **Accept.** |
| H | `Engine::run_gc`, `GcControl` | **Defer to M17.B** as the plan orders. `Gc` answering `invalid` until then is fine. |
| I | `Partial` lists undeleted segments | **Accept** (an M9 change). Until then, m14's fix. |

## Spec gaps

- **`journal_max >= queues × 128 MiB`.** Confirmed in `lunix-journal` (a lane needs two files). The plan's
  256 MiB for M18b allows 2 queues and its 512 MiB for M15 allows 4. Fix the plan's numbers or the per-lane
  minimum, and have `VolumeCreate` and `Attach` refuse with a message that names the rule.
- **The memory gate.** The report's reading ("fresh daemon") passes at 598 MiB; the same daemon after the smoke
  and crash cells is at 734 MiB, above the 662 MiB limit. Both are measured on an empty volume. The lead should
  decide the wording: I recommend counting the journal's locked batch buffers with the queue buffers, keeping
  the limit for a fresh daemon, and adding a stated bound for a daemon after recovery, since that is the state a
  production daemon is in after any restart under load.
- **Size of a detached encrypted volume.** Make the field optional or document 0 as "unknown"; do not put the
  size in `volume.json` (it is unauthenticated).
- **`chain_floor` with non-FULL commits** and **the floor after an attach-time EPOCH commit**: both are phase 2,
  correctly noted.
- **Added by this review: the fork window.** With `retain_last = 2` the fork check covers one commit above the
  floor. In phase 1 the floor follows every C6, so an honest tip is at most a few commits above it. Proposal: when
  the floor's height is below the tip's `chain_floor`, do not proceed silently; return a notice like the
  first-attach one (history between the floor and the tip cannot be checked), and log it. Whether to refuse
  instead is the maintainer's call.
- **Added: a rejected `state/pins`.** §20 says absent; the store says error. Pick one.

## Test quality

- **Tests can pass without running.** Every test that needs a real file system returns early with a `SKIP` line
  when the scratch directory is tmpfs (`tests/common/mod.rs:188-202`). The count of 1 376 passed does not show
  it. See "Needs a run" for the check.
- **Journal damage** (`tests/wiring.rs:368-425`): `l.contains("damaged")` is satisfied by the field name even for
  an empty list, and the report assertion accepts `ignored_after_gap` alone. Assert the damaged range and its
  sequence bounds, which is what the gate asks for.
- **Fork**: no case pins the limit (a fork two commits above the floor attaches). Add it, so a later change of
  `retain_last` or of §20 shows up.
- **Downgrade**: the integrity volume with an encryption credential is missing (m7).
- **Shutdown**: nothing covers a failed quiesce, two volumes, or a `QuiesceAll` that outlives the call timeout.
  M1 and M2 need tests over a fake control plane.
- **Control gate**: the floor in `VolumeStatus` and the notice are checked through the log only (gaps B and C).
- **Smoke**: the crash cell uses a rate-limited random write, not "the same load" as the 1 GiB cell (stated in
  the report); the memory cell uses an empty volume; `lunixd check` stands in for `lunix fsck`. The script keeps
  W3, W4, W5, W8 and W9: bounded fio, no kill of a stopping daemon, unmount before detach, `df-guard` before
  `mkfs`, stop on a wedge.

## Needs a run

None of these was run. Commands are for the lead, through `tools/remote.sh`, when a host is free.

1. **No test was skipped in the recorded CI run.**
   `tools/remote.sh run bash -c 'cargo test -p lunixd -- --nocapture 2>&1 | grep -c "^SKIP"'` must print 0.
2. **The smoke at the final head.** Run 4 used binaries from `a1d3bfa`; the report says the difference is
   formatting only. The command is the one in the report's "Device smoke" row.
3. **A second `QUIESCE_DEV` on a device this process already quiesced** (the retry M1 depends on): what the
   driver answers, and whether `Engine::quiesce` is idempotent. A root cell with two volumes, one held busy.
4. **The crash-point list.** `cargo build -p lunixd --features crashpoints`, then `lunixd --list-crashpoints`
   must include `index.*` names (the feature is not forwarded to `lunix-index`).
5. **More queues than lanes on an existing volume.** Attach with 2 queues, write, detach, attach with 4 at the
   same `journal_max`: the per-lane file count halves while lanes 0 and 1 keep their files.
6. **`on_journal_damage = refuse` after a plain kill.** Confirm that a torn tail alone is not reported as damage,
   or `refuse` would keep every crashed volume offline:
   `cargo test -p lunixd --test wiring journal_damage` covers mid-ring damage only.

---

# Re-review (2026-11-14)

- Reviewed: `lead/p1-iface2` at `59251e6` (base `30e4329`) and `p1/m12a` at `043b55d` (rebased onto `59251e6`),
  with both reports.
- Scope: targeted. The fork walk and its override, the torn-tail rule, M1/M2 and the shutdown tests, the memory
  trimming, and the minors of the first review.
- Run: `cargo test -p lunixd -p lunix-engine` from a scratch copy of `p1/m12a` on the host the lead named, one
  test thread, no root. Result: exit 0, 89 tests passed, 0 failed, no `SKIP` line.

## Verdicts

| branch | verdict |
|---|---|
| `lead/p1-iface2` | **Merge after fixes**: R1 (the gap half of the `refuse` rule) needs a decision, R2 is a small code change. Everything else is right. |
| `p1/m12a` | **Merge after fixes**: R3 and R4. Both are small. M1 and M2 are fixed. |

## What I checked and found right

- **The fork walk is correct and complete.** `check_floor_on_path` runs `verify_chain` from the tip down to the
  floor's height, which authenticates every commit and checks every `parent_hash` link, then compares the commit
  at the floor's height with `tip_hash`. With the three cases together (tip below the floor: `Rollback`; equal
  height: hash compare; above: the walk), a chain attaches only if the recorded commit is the tip or an ancestor
  of it. I found no way for a forked chain to attach silently while a floor exists. What remains is what
  ADR-31 already states: no floor (first attach, deleted `state/pins`) means no check.
- **No honest lock-out today.** Nothing in the engine deletes commits, so the floor's commit exists until M17's
  collector, and M17 rule 6 now requires it to keep commits at or above the floor. A crash between C6 and the
  floor raise leaves the tip one above the floor with the link present.
- **The override is scoped to one attach.** It is a field of the request, defaults to false, is false for
  `recover_on_start`, and the engine raises the floor to the accepted tip, so it is needed once. It is audited
  (`"overrides":["allow_unverifiable_fork"]`) and logged at warn before the open.
- **The torn-tail argument holds.** I traced it in `lunix-journal`:
  - a record's sequence number is in its lane's `pending` set from `try_reserve` until its batch write is reaped;
  - the flush coordinator waits until no lane has a pending number at or below `upto`, then `fdatasync`s every
    file with reaped writes, on all lanes, and only then completes the flush;
  - an aborted or failed record poisons at its number, which fails every flush at or above it;
  - batches are padded to 4 KiB and appended, so a later write never rewrites a block of an earlier record.

  So every record at or below an acknowledged `upto` (including `upto = last_issued()`) is on stable storage
  before the acknowledgement, on every lane, and no later write can tear it. A FLUSH- or FUA-acknowledged record
  cannot be the torn last record of a lane after a crash.
- **M1 and M2 are fixed**, and `tests/shutdown.rs` exercises what it claims: two volumes, a lone failure that
  keeps running with an honest message, a failure after another device was quiesced (retry, then `SIGABRT`), a
  `QuiesceAll` longer than `call_timeout` that still replies and exits, an attach refused during the quiesce.
- **Trimming never runs on a queue thread.** `malloc_trim` is called only from `lunixd-trim`, and only after
  every slot could be inspected and showed no I/O or commit for `idle_trim_ms`.
- **The minors**: m1, m2, m6, m7 (through request D), m8, m9, m10, m11, m12, m13, m14 and the nits listed as
  done are done as described. m5 is documented in DESIGN §4.4. m3 and m4 are done, with the two remarks below
  (R4, R5).

## New findings

### R1 (`lead/p1-iface2`, major, a decision). The same argument makes "records beyond a gap" crash residue too, so `refuse` still refuses ordinary crashes

`crates/lunix-engine/src/recover.rs` (`refuse_worthy`), `crates/lunix-journal/src/scan.rs` (the merge),
`crates/lunix-journal/src/config.rs:117` (`max_inflight: 2`).

The rule refuses on `ignored_after_gap` and on a damaged range with a later valid record in its lane. The reason
given is "lost ordering of acknowledged writes". The durability argument above says otherwise: a flush with
`upto` at or above the later record waits for **every** lower number on every lane. If a number is missing or
torn after a crash, no flush covering anything above it was ever acknowledged. The later records are exactly as
unflushed as a torn tail.

Both shapes are what an ordinary crash under load leaves:

- **Gap across lanes (also after `kill -9`).** Lane 0 has reserved number 10 and holds it in its open batch
  because a batch is in flight. Lane 1 writes number 11 and it completes. The daemon dies. Number 10 was never
  submitted, 11 is on disk: `gap_at = 10`, `ignored_after_gap = 11..11`, refused. With four queues at any depth
  this is the normal state at the instant of a kill. The journal's own module comment calls these "records of an
  unflushed tail that happen to be intact".
- **Two batches of one lane (power loss).** `max_inflight` is 2, so batches A and B of a lane are in flight
  together. A is torn, B lands: a damaged range with `next_seq` set, refused.

Consequence: with `on_journal_damage = refuse`, a multi-queue volume stays offline after most crashes under
write load, which is the problem the decision set out to remove. The new tests do not show it because they use
one lane and flush after every write.

What recovery cannot know is the flushed watermark: damage or a gap **at or below** the highest acknowledged
`upto` is real damage, anything above it is residue. Options for the lead:

1. Record the watermark. Each batch (or record header) carries the highest `upto` whose flush had completed when
   it was built. Recovery takes the maximum over valid records as a lower bound, refuses damage or a gap at or
   below it, and treats the rest as residue. This is exact up to the last flush that a later record witnessed. It
   is a format change (freeze item).
2. Keep the rule and document that `refuse` is for single-queue volumes or for operators who accept an `fsck`
   after every crash.
3. Treat every gap and every damage above the tip's cut as residue under `refuse` as well. Then `refuse` refuses
   nothing after a crash and only option 1 gives it a meaning.

I recommend 1, and until it lands 2, stated in the README and in ADR-4.

Also note the other direction, which the rule accepts by design: a flushed last record of a lane that is damaged
by the medium, with nothing later in any lane, is wiped as a torn tail. `a_torn_last_record_is_crash_residue_under_refuse`
damages exactly such a record (it was flushed). Option 1 closes this too.

### R2 (`lead/p1-iface2`, minor). The override also accepts positive evidence of a fork

`crates/lunix-engine/src/recover.rs:118-146`.

Every failure of the walk becomes `ForkUnverifiable`: a missing commit (`NotFound`), but also a commit that is
present and does not link (`parent_hash` mismatch) or does not authenticate. The last two are not "the link is
gone". Example: the fork's commits 4 and 5 copied over a directory that still holds the honest 1 to 3. The walk
finds commit 3, whose hash is not what commit 4 names. That is a fork, shown by the files, yet it is reported as
unverifiable and `allow_unverifiable_fork` attaches it.

Fix: map only `NotFound` to `ForkUnverifiable`. A broken link or an unauthenticated commit on the walk is `Fork`
(or the broken-chain error) and is never overridable.

### R3 (`p1/m12a`, minor, fix before M15). `stopping` is set by any detach error, so a serving device can be skipped by the quiesce

`crates/lunixd/src/daemon.rs:923-941` and `1118-1121`.

`detach` sets `live.stopping = true` for every error except `Busy` and the final-commit error. Several of those
errors happen **before** `TRY_STOP_DEV` succeeded, with the device still live and serving:

- `Unsupported` (no `SAFE_STOP_DEV`);
- a control error or watchdog timeout of `TRY_STOP_DEV` itself;
- `State` when the device is latched quiescing.

`quiesce_all` then logs "nothing to quiesce" for that device and the daemon exits with it live, with started
requests and no final commit. That is the unquiesced exit W2 forbids (recoverable, like a kill), reached through
the fix for M1.

Fix: take the state from the layer, not from the error. `lunix-ublk` needs a small accessor for its lifecycle
(serving, quiescing, stopping, deleted); skip the quiesce only for stopping or deleted.

### R4 (`p1/m12a`, minor). A signal during startup recovery can leave the daemon quiesced and alive

`crates/lunixd/src/server.rs:253-256`, `daemon.rs:1215-1250`.

`run` awaits the startup task before it enters the loop that reads the exit channel. `recover_on_start` checks
`quiescing` at the top of each iteration, then probes the device, then takes the slot lock. If a signal's
`quiesce_all` sets the flag and takes every slot lock inside that window, recovery blocks on a lock that is never
released, the startup task never ends, and the exit request is never read. The window is the probe (a control
command per volume).

Fix: select on the exit channel while startup runs, or have recovery re-check the flag with `try_lock`.

### R5 (`p1/m12a`, minor, for the lead to confirm). After a reboot nothing is re-attached

`daemon.rs:1223-1245`.

`recover_on_start` now treats a record from another boot as "gone": the record is cleared and the volume stays
detached. Plan step 8 with step 5 says the opposite ("otherwise add a new device"), and the first version did
that. My m4 was about the same boot only. Staying detached after a reboot is defensible (an orchestrator or a
unit attaches), but it is a change of behaviour the report does not call out. Decide and write it in the plan.

Two smaller points in the same code:

- The record is removed, and in `bring_up` a DEAD device is deleted, **before** `LOCK` is taken and before the
  credential check (`daemon.rs:727-737`). In the first version the delete ran after `LocalVolume::open`. A second
  daemon instance on the same volume directory could delete a device the first is between `persist` and
  `START_DEV` on. Move the probe and delete behind the open.
- The locked-buffer bound is checked after `open_volume` (recovery has run, the floor is raised). Check it
  before, from the request and `volume.json`.

### R6 (`p1/m12a`, minor, needs a measurement). The allocator settings touch the data path

`crates/lunixd/src/trim.rs:21-38`.

- `M_MMAP_THRESHOLD = 128 KiB` makes every allocation of that size or more an `mmap` and every free a `munmap`.
  Under `mlockall` each such `mmap` is populated and locked at once, and each `munmap` sends TLB shootdowns to
  all threads, the queue threads included. Chunk buffers of volumes with chunks of 128 KiB and more, 1 MiB read
  buffers and destage buffers are in that range.
- `M_ARENA_MAX = 4` makes queue threads, destage workers and helpers share four arenas.
- `malloc_trim(0)` holds each arena lock while it walks it. It runs only when idle, so the exposure is a burst
  that arrives during the trim.

None of this is wrong, and it is what makes the memory gate reachable. It needs M16's numbers before and after
(64 KiB and 1 MiB chunks), and the root memory cell, which is still to run. If M16 shows a cost, pooled buffers
on the hot paths are the usual answer.

### Nits

- `daemon.rs:1183-1186`: with an orphan device the daemon exits with code 75 through the normal path. If that
  device became live late it has started requests; `abort()` is the sanctioned exit for that case.
- The audit line of an override is written when the call ends. A crash during the attach leaves only the stderr
  warning. Write an intent line before the open.
- `recover.rs`: a whole ring file with a bad header counts as a torn tail when it is the last of its lane. After
  a crash in `reinit_slot` such a file is zeroed below its header. A bad file with non-zero content is not
  residue; the scan has `nz_end` to tell them apart.

## Still to run (root, unchanged from the report)

The smoke at the final head with the memory gate after the workload, the two-volume quiesce cell on real
devices, and m3/m4 on real devices. Add for R1: a four-queue write load, `kill -9`, then attach with
`on_journal_damage = refuse`; I expect `journal_damage`.
