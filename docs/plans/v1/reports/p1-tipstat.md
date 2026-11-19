# p1-tipstat: the status path takes no coordinator mutex (ADR-33 T5)

Branch `p1/tipstat`, cut from `origin/main` 57deabf.

## What was built

- `lunix-engine`: the coordinator publishes a `TipView` (`commit_seq`, `cut_seq`, `created_at`, one
  `Copy` value behind a `std::sync::RwLock`) right after `co.tip = full` (`Inner::publish_tip`, called
  from the one commit that installs a tip) and at open (from the recovered tip). `Engine::tip`,
  `tip_created_at` and `window_turn_left` read it and never lock the coordinator. The write lock is held
  for one 24-byte store, never across I/O. A seqlock or atomics per field were rejected (a reader could
  see the commit_seq of one commit with the created_at of another); no `arc-swap` is in the workspace,
  so no dependency change is needed or asked.
- `window_turn_left` computes the debt from the published `commit_seq` (`turn_owed` now takes a
  `CommitSeq`, not the `Coord`); the coordinator's own callers pass `co.tip.common.commit_seq`.
- Test hook `Engine::hold_coordinator` (`#[doc(hidden)]`, `CoordHold`) and
  `Daemon::engine_of` (`#[doc(hidden)]`), used only by the tests.

## Audit of the `status` / `gc --status` paths

Removed: the three coordinator-mutex reads (`tip`, `tip_created_at`, `window_turn_left`).

Checked and left:

- `gc --status`: `GcStatusHandle::get` locks `gc_status`, which is held only to assign or clone a
  snapshot (`gc.rs`, after the collector call returns); no I/O under it. No slot mutex.
- `status` takes the per-volume slot mutex (it finds the stack through it). Handlers holding it for long:
  `attach`, `detach`, `resize` (a grow commits), `space` (a `volume.json` write). All operator-driven,
  none held by the write load. Left: removing it means a lock-free slot registry, a larger daemon change.
- `engine.stats()` is atomics plus `map.dirty_leaf_count()`, which locks the map's `inner` mutex. By
  reading `lunix-map` that mutex is taken in short bursts (the seal copies out under it and does its I/O
  outside); the map crate is not mine. Left, unmeasured under load.
- `space.snapshot()` / `is_read_only()` lock the gate's state mutex (in-memory, short); `lunix-space` is
  not mine. `journal.fill()` locks each ring's `inner` for a counting loop; the journal crate is not mine.
- `store.rollback_floor()` reads `state/pins` from disk without `pins_lock`: a small file read under the
  slot mutex. Left.

## Tests

- `lunix-engine/tests/tip_view.rs`: the getters return while `hold_coordinator` holds the mutex (10 s
  `recv_timeout`, so a regression fails instead of hanging); the published tip and `created_at` equal the
  store's committed tip after open, six `commit_now`, a forced FULL and a `tick` commit; the same across a
  crash after C6 (hook `engine.c7.checkpoint`, store and journal crashed) and reopen, and a commit after it.
- `lunixd/tests/status_lockfree.rs`: an in-process daemon (null device) attaches a volume; with the
  coordinator held, `VolumeStatus` and `GcStatus` answer (51 us measured) with the right
  `last_commit_seq`. The test takes about 10 s, all of it the attach (an existing cost, not this change).
- Existing `full.rs` window-turn tests pass unchanged on the lock-free `window_turn_left`.

## Gate

Full `tools/ci.sh` on the remote host (`-j4`, CPUs 8-15), including ownership (`tipstat ok`, 8 changed
paths): all green, 2022 tests passed, 0 failed, 28 ignored (sum of every `test result` line).

## Deviations, conflicts

- None from the plan. rust-skills: no conflicts. No on-disk format change, no dependency change.
- Remote disk: target and source removed at the end.
