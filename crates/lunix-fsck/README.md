# lunix-fsck

Offline checker and repairer of a local Lunix volume (plan M14): the library
`lunix_fsck::check` and the `lunix-fsck` binary. It takes the volume `LOCK`
and writes nothing unless a repair flag says so.

```
lunix-fsck [--deep] [--json] [--key-file P] [--passphrase-env V] [--rebuild-index]
           [--repair [--delete-orphans] [--gc] [--truncate-tip] [--reset-floor]] <volume-dir>
```

Exit status: 0 clean (notes allowed), 1 warnings only, 2 errors, 64 usage.
The finding codes, with severity and meaning, are `lunix_fsck::code::ALL`
(a test keeps the table complete); `docs` of the crate root list the checks.

## Decisions

1. Own reader: files are read with the frozen decoders and the `lunix-crypto`
   codecs, so every failure has its own code; `LocalVolume` supplies the lock
   and the repairs.
2. A check never writes: the index is opened on a scratch copy of `index/`.
3. BLAKE3 goes through `CommitCodec::object_hash` (no `blake3` dependency).
4. The journal is read with `lunix_format::journal::scan_file`, never through
   `Journal::recover` (its first lane open wipes tails).
5. Orphan: no commit file on disk names it, no retained root references it and
   the condemned list does not. `orphan` after a clean close, else `crash_leftover`.
6. Repairs run first, then the checks run again. `--truncate-tip` lowers the
   floor, sets the tip aside, then zeroes the checkpoint; it loses the journal
   records C9 already reclaimed, and says which. `--delete-orphans` is refused
   with `--truncate-tip` (the zeroed checkpoint loses `id_high`).
7. Lowering the rollback floor is a stub (`FloorLowering`) until the lead
   approves `VolumeDir::reset_rollback_floor` (M14 report, proposal P1).
