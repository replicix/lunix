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
5. Orphan: above the high-water mark (below it, unreferenced is GC's garbage), no
   commit file names it, no root of GC's retained set references it, not listed.
   `orphan` after a clean close, else `crash_leftover`.
6. Repairs run first, then the checks run again. `--truncate-tip` lowers the
   floor, sets the tip aside, then zeroes the checkpoint; it loses the journal
   records C9 already reclaimed, and says which. `--delete-orphans` is refused
   with `--truncate-tip` (the zeroed checkpoint loses `id_high`).
7. The floor is lowered only through `VolumeDir::reset_rollback_floor`
   (store opened with `allow_floor_reset` only for `--truncate-tip`,
   `--reset-floor` and `--repair-pins`; audited in `state/floor-resets.log`).
8. Memory: about 270 bytes per distinct chunk, plus about 30 per mapped one (crate docs).
9. Taking the lock rewrites `LOCK` with the pid: that is the one write a check
   makes, and a read-only medium cannot be checked.
10. A keyed volume without a credential gets no check at all.
11. `index_lost` after a kill is expected until N13's engine side lands; fix it with
    `--rebuild-index` (it holds every entry in memory, over 100 B per chunk).
