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
- Tests: `tests/wiring.rs`, `tests/r1_crypto.rs`, `tests/destage_adapter.rs`,
  `tests/control.rs` (no root); `tests/root/m12a-smoke.sh` (root, through `tools/remote.sh root`).
  `tests/stack_*.rs` and the body of `src/testsupport.rs` are M12b's.
