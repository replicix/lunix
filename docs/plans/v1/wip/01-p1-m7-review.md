# Review of M7 — `lunix-store` (branch `p1/m7-store` at 9b9fbb7)

Independent review of the durable write order against the plan's M7 section
and gate, DESIGN §4, §4.4, §6, §6.1, §8.2, §11, on-disk-formats §7, §16, §18,
§22, D17, the `lunix_iface::store` traits and their contract suite. The gate
was re-run on the remote host in a fresh directory (deleted afterwards).
`rust-skills` rule ids are cited where they apply; plan and spec win.

## Verdict: **merge after fixes**

The durable order is right on every path I could construct, for a process
kill and for a power loss: data `fdatasync` before the rename, directory
`fsync` after every rename and unlink, create-only renames through
`RENAME_NOREPLACE` with a correct `link`+`unlink` fallback, a two-slot
checkpoint whose first write syncs the directory and whose `zero()` is one
atomic replace, and ids that start above every file on disk. No window leaves
a *named* file that is not durable, and no durable name is ever reissued. The
two majors are not crash windows: one is an in-process race that the trait's
"atomic replace" promise does not allow (finding 1), the other a CI-policy
choice the lead has to take (finding 2). The rest is small.

## Gate re-run (host, `9b9fbb7`, `tools/remote.sh -m m7 run bash tools/ci.sh`, 4 pinned CPUs)

| step | result |
|---|---|
| tool tests, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | `lunix-store`: **72 passed, 0 failed, 18.1 s**; doc-tests 0; every other crate unchanged |
| iface with crashpoints (clippy + 6 tests), feature-edge check `ok`, `lunixd --features crashpoints` builds | pass |
| ownership | `m7 ok (17 changed paths)` (report says 16: its own report commit is the 17th) |
| | `ci: all green`, exit 0 |

Counts match the report. The branch touches only `crates/lunix-store/**` and
the report.

## Power-loss walk (what the crash sweep cannot show)

- **Seal.** `.open` is created with `create_new` and appended to; the seal
  writes footer and trailer, `fdatasync` (covers the size change), renames
  with `NOREPLACE`, `fsync(segments/)`. A power cut before the directory sync
  leaves either `<id>.open`, `<id>.seg`, or (on the link fallback) both;
  the sweep removes `.open`, and `.seg` above the mark. Correct.
- **Lost `.open` entry.** The `.open` creation is never directory-synced, so
  after a power cut the entry can vanish and the id be reissued. This is
  harmless: nothing durable names it (index inserts are held until the
  commit, §8.2 step 2/7; the pack and commit that would name it come later
  in the same order), which is exactly why D17 takes the maximum of three
  inputs and not a persisted counter. Worth one sentence in `lib.rs` so
  M11 does not expect the disk scan alone to be sufficient (finding 9).
- **Swept id reissued after a crash.** Sweep deletes orphan `N`, crash before
  a new checkpoint: `N` can be reissued. Safe for the same reason; a stale
  index run that could point into `N` has an id above `N` and lives in
  `index/`, which the scan covers, so the allocator starts above it anyway.
- **Commit, evidence, state files.** `publish`: tmp, `fdatasync`, rename,
  `fsync(dir)`; the tmp is removed on error. Old or new, never torn.
- **Checkpoint.** First write: `set_len(8192)`, `pwrite` slot 0,
  `fdatasync`, `fsync(state/)`. Later: `pwrite` the slot not holding the
  newest valid copy, `fdatasync`. A torn 4 KiB slot fails its CRC and the
  other slot answers; a slot that the FS exposes with stale blocks fails
  magic/uuid/CRC. `zero()` replaces the whole file by rename, so no
  intermediate state has one valid slot above the truncated tip. Correct.
- **`set_aside`.** Rename `NOREPLACE` then directory sync; the link fallback
  can leave `<seq>` and `<seq>.damaged` as one inode, which keeps the old
  tip (the safe side) and a repeated repair finishes it.
- **Create.** Subdirectories, their `fsync`, `volume.json`, `fsync(dir)`,
  `fsync(parent)`, then `volume` create-only. A crash earlier is "no
  volume" and a second `create` finishes. Correct, apart from finding 7.

## Findings

Severity: **blocker** (none), **major**, **minor**.

1. **major — replace-style writes race inside the process and can leave a
   truncated file.** `fsops.rs` `publish` uses the fixed temp name
   `<name>.tmp` and `create_tmp` truncates it. `LocalVolume` is `Clone +
   Send + Sync`, and nothing serialises two callers of `store_volume_json`,
   `store_device_record`, `store_local_pins`, `store_condemned` or
   `CheckpointStore::zero` on the same file: A writes and syncs, B truncates
   the same tmp, A renames B's empty file into place, B's rename fails
   `NotFound`. The destination is torn, which the trait forbids ("replaces
   atomically"). `CheckpointStore::store` has the same shape (two callers
   compute the same slot and interleave `pwrite`s); the CRC turns that into
   a lost hint, so it is only unpleasant there. Fix: one `Mutex<()>` in
   `Inner` taken by `publish` with `Publish::Replace` and by `store`, or a
   unique temp name (`<name>.<pid>.<n>.tmp`; keep the `.tmp` suffix so the
   sweep still finds it). Add a two-thread test. (`own-mutex-interior`)
2. **major (lead decision) — the dev-dependency on `lunix-iface/crashpoints`
   switches the registry on for every crate in `cargo test --workspace` and
   `cargo clippy --workspace --all-targets`** (report decision 12). With
   resolver 2 the dev-edge is unified for the whole invocation, so M3, M5,
   M6, M11 and `lunixd` are tested with every `crashpoint!` compiled to a
   real call and `inventory` linked, while `cargo test -p <crate>` tests
   them with the macro compiled out: two different programs, and the one
   CI runs is not the one that ships. The feature-edge check passes only
   because it ignores dev edges by design. The precedent (M1.B1 review 15)
   is a dedicated `ci.sh` line. Recommend: give `lunix-store` its own
   feature `crashpoints = ["lunix-iface/crashpoints"]` like `lunixd`, gate
   `tests/crash.rs` on `#[cfg(feature = "crashpoints")]`, drop the dev-dep,
   and have the lead add `cargo clippy/test -p lunix-store --features
   crashpoints` to `ci.sh`. If the lead prefers the dev-dep for wave 1a,
   write the consequence into the plan's rule 6. (`proj-feature-additive`)
3. **minor — unbounded reads of state files.** `volume.rs`
   `read_state_file`, `read_volume_file`, `checkpoint.rs`
   `read_checkpoint_file` and `gcstate.rs` `load_condemned` call `fs::read`
   with no size check; the length is checked only after the allocation
   (`VolumeJson` 1 MiB, `DeviceRecord` 96 B, checkpoint 8 KiB, condemned
   256 MiB). Commits and pins already `metadata()` first; do the same here
   (cross-cutting rule 2, `mem-with-capacity`).
4. **minor — `IdCounter::observe` adopts any 16-hex stem.**
   `highest_id_on_disk` takes the first 16 hex characters of *every* name
   in `segments/`, `map/` and `index/`. A stray `<hex>…` name with a zero
   counter or `MAX_COUNTER` makes every later `allocate` fail with "object
   id counter exhausted", which misnames the cause. Observe only the shapes
   the layout defines (`<id>.seg|.open|.tmp`, `<id>[.open|.tmp]`,
   `<id>[.tmp]`, `manifest`), `warn` on anything else, and let `allocate`
   report a bad observed value separately from exhaustion. (`err-custom-type`)
5. **minor — `sweep_orphans` and `remove_leftovers` unlink a live writer's
   `.open` file** (`tests/misc.rs` `leftovers_are_listed_and_removed`
   demonstrates it). The failure is loud (the seal's rename gives
   `NotFound`) but the trait only says "runs before `catch_up`". Keep the
   ids of open writers in `Inner` and skip them, or refuse the sweep while
   a writer is open.
6. **minor — the sweep trusts an inconsistent request.** A `named_segments`
   id above `high_water_mark` is kept (the safe side) but silently; refuse
   with `Invalid`, since it means the engine computed the mark wrong.
   Also rename `kept_unreferenced` in the iface docs: with `named_*` =
   `new_segments[]` of retained commits almost every old segment lands
   there, so fsck must not read it as "orphan".
7. **minor — `create` swallows the parent-directory `fsync` error**
   (`fsync_dir_of`, `let _ =`). It is the one sync that makes the volume
   directory's own entry durable; propagate it. Also `create_dir_all(dir)`
   runs before the tmpfs refusal, so a refused create leaves an empty
   directory. (`anti-empty-catch`)
8. **minor — the container cache is keyed by id alone.** `open_container`
   checks `trailer.container_kind` only on a miss; a hit returns a
   container of the other kind for a wrong `read_node`/`read_extent`. Key
   by `(kind, id)` or check `c.kind` on the hit. Also `MAX_FOOTER_LEN` (128
   MiB) could be `footer_body_cap(footer_off)` plus the envelope overhead.
9. **minor — document the three allocator inputs in `lib.rs`** and have
   `open` observe the checkpoint's `id_high` itself (it has `load()`); the
   caller still feeds the chain's highest id. Cheap defence in depth for
   the lost-`.open` case above.
10. **minor — `Inner.warnings` grows without bound** when the daemon does
    not drain `take_warnings()`; cap it (keep the last 64, count the rest).
11. **minor — the lock.** `flock` is enough for the common `kill -9`
    (the kernel releases it at exit), but not for ADR-29's actual case: a
    killed holder whose last thread is stuck in the kernel keeps its file
    table, so the lock stays held and every open gets `Busy` with no pid.
    That is the safe direction (ADR-29 rejects takeover on a timeout), so
    accept it for M7 and: (a) make the `Busy` error carry the holder's pid
    (read the `LOCK` text or `FlockLock::holders`; `Error::Busy { what:
    &'static str }` cannot, so this is an iface nit for the lead), (b)
    state the limitation in `lib.rs` and the plan, (c) leave `/proc`
    classification to M12, where a control socket exists to ping.
12. **minor — the fork race in the tests.** `tests/mod.rs` `open` retries
    `Busy` for 10 s because a child between `fork` and `exec` holds a copy
    of every flock'd descriptor of other tests. Real in tests only (Rust
    opens with `O_CLOEXEC`, so an exec'd child never keeps a lock), but a
    retry hides a genuine `Busy` for 10 s. A test-global `RwLock` (write
    around `spawn_child`, read around `take_lock`) is deterministic.
13. **minor — leftover scan scope** (report decision 10): the sweep deletes
    `*.tmp` under `journal/` and `index/` too. Record in the plan that M3
    and M6 temp names must end in `.tmp` and that the engine runs the
    sweep before either crate starts writing.

Decoders: `parse_extent`, `read_range` (the stored-`clen` cross-check reads
bytes 48..52, inside `OVH` = 68/108), `open_container`, `container_footer`,
`parse_hex16`, the pin and commit size caps and the format-crate decoders
they lean on are bounded, checked and panic-free on the inputs I tried
(truncation and bit flips are covered by the damage sweep). `unsafe`: none.
`err-no-unwrap-prod`: library code has no `unwrap`/`expect` on disk data.

## Decision answers (report numbering)

1. **Confirm.** Construction stays inherent; M11/M12 call
   `read_volume_file`, derive keys, then `LocalVolume::open`.
2. **Accept with the limitation written down** (finding 11); the plan's
   "stale-lock takeover (ADR-29)" wording should be softened to "flock,
   released by the kernel; a holder stuck in the kernel keeps it".
3. **Confirm.** `load` as written plus `load_checkpoint_hint`/`tails_usable`
   is the right split; `zero()` as one rename is better than two slot writes.
4. **Confirm**, with the cap of finding 10. (`obs-library-facade` would
   prefer the `tracing` facade; a dependency change is the lead's call.)
5. **Confirm.** The footer is authenticated and tiling-checked; extents are
   verified by their tag on read; `verify_segment` serves fsck.
6. **Confirm.** The stored-`clen` check is cheap and catches a misaligned hint.
7. **Confirm.** Note `is_empty()` exists for callers that must not seal empty.
8. **Confirm**; `max_open_files` is a cache bound, not a descriptor bound
   (`ExtentLocation`s keep more alive): say so in the doc comment.
9. **Confirm.**
10. **Confirm** with finding 13.
11. **Confirm.**
12. **Prefer the own-feature + `ci.sh` line** (finding 2).

Spec gaps: both agreed. (a) `Hint.clen` is the payload length,
`NodeLoc.len` the whole extent: one sentence in on-disk-formats §7/§8 and
the iface docs. (b) `delete_run`: strike it from M7 step 7; M6 owns `index/`
and deletes its runs with the same `unlink` + directory `fsync`. If the lead
wants one place for every durable delete, a `pub fn delete_run(&self, RunId)`
on `LocalVolume` around `unlink_durable` is a ten-line addition.

## rust-skills notes

`mem-drop-order` is used deliberately (`_lock` last in `Inner`);
`conc-atomic-ordering`: `IdCounter` could use `AcqRel`/`Relaxed`, harmless;
`api-non-exhaustive` on `StoreOptions` is a conscious in-workspace choice;
`perf-io-buffering`: the 1 MiB append buffer is right; `test-descriptive-names`
and the instrumented `FsOps` seam (`test-mock-traits`) are good.
