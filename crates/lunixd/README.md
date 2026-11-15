# lunixd

The Lunix daemon (plan M12a): configuration (`/etc/lunix/lunixd.toml`, a flat `key = value`
file, see `src/config.rs`), the host registry `<state_dir>/volumes.json`, the control socket
(length-prefixed JSON frames of `lunix-iface::ctl`, audit log in `<state_dir>/audit.log`), the
wiring of the real crates into one volume (`src/stack`), the ublk device in front of an engine,
signals and startup recovery.

```
lunixd [--config PATH]          serve
lunixd --list-crashpoints       crash points of a `--features crashpoints` build
lunixd ctl --socket PATH JSON   one raw control request (until `lunix`, M13a, is merged)
lunixd check --dir VOLDIR       offline store self-check of a detached volume (until M14)
```

- `stack::create_volume`: descriptor (MAC'd), keyring and key slots (`integrity.key` beside a
  default integrity volume), `volume.json`, the genesis FULL commit, `state/pins` with the pin.
- `stack::open_volume`: `check_attach`, the store, the ring-size check, journal (lanes = queues),
  overlay, index, space gate, map opener (`lunix-map`), the M9 destage adapter, `Engine::open`,
  then the pins. The engine's rollback floor goes through `VolumeDir::rollback_floor`.
- `device = "none"` runs the engine without a block device (control tests without root).
- Notes for operators: `lunixd ctl` takes a request carrying a passphrase only on stdin (`-`).
  Journal lanes never shrink in phase 1 (an attach needs at least as many queues as the
  journal has lanes) and `journal_max` must be at least queues x 128 MiB. `dir` and `key_file`
  of a request are paths the root daemon reads or creates: give `socket_group` only to
  trusted users.
- `on_journal_damage = refuse` refuses only loss at or below the journal's witnessed flushed
  watermark (ADR-4): crash residue (a torn tail, a gap across lanes, two batches in flight) is
  wiped as under `wipe`. The journal witnesses every flush with a later record or a mark of its
  own after `mark_idle` (50 ms); `VolumeStatus` reports `journal_marks_written` and
  `journal_marks_skipped`. A clean detach commits a cut, which covers the last flush; after
  `QuiesceAll` the next daemon's first lane open writes the mark for the recovered records.
- Memory: once every volume has been idle for `idle_trim_ms`, the daemon empties each volume's
  decoded-chunk read cache (`read_cache_max`, 128 MiB by default, the bulk of what a workload
  leaves resident) and then trims the heap.
- After a reboot, a volume that was attached (its `state/device` is from another boot) is
  attached again at start with a new device.
- Tests: `tests/wiring.rs`, `tests/r1_crypto.rs`, `tests/destage_adapter.rs`,
  `tests/control.rs`, `tests/startup.rs`, `tests/shutdown.rs`, `tests/memory_diag.rs` (no root;
  `LUNIXD_MEMDIAG=1` prints the heap after a 1 GiB workload); root, through `tools/remote.sh
  root`: `tests/root/m12a-smoke.sh`, `m12a-lifecycle.sh`, `m12a-refuse-crash.sh`, and
  `m12a-faults.sh` (needs a `crashpoints` build: the lunix-ublk fault hooks).
  `tests/stack_*.rs` and the body of `src/testsupport.rs` are M12b's.
