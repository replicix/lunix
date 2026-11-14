# lunix-fsck

Offline checker and repairer of a local Lunix volume (plan M14). A library
(`lunix_fsck::check`) and the `lunix-fsck` binary. It takes the volume `LOCK`,
so it refuses a volume a daemon holds, and it writes nothing unless a repair
flag says so.

```
lunix-fsck [--deep] [--json] [--key-file P] [--passphrase-env V] [--rebuild-index]
           [--repair [--delete-orphans] [--gc] [--truncate-tip] [--reset-floor]] <volume-dir>
```

Exit status: 0 clean (notes allowed), 1 warnings only, 2 errors, 64 usage.

## Decisions

1. **Own reader, store opened only for the lock and the repairs.** The checks
   read the files directly with the frozen format decoders and the codecs of
   `lunix-crypto`, so that every failure has its own code (the store maps most
   of them to `Corrupt`). `LocalVolume` supplies the lock, the commit store and
   the repairs (`set_aside`, `zero`, `remove_leftovers`, deletes).
2. **A check never writes to the volume.** The index is opened on a scratch
   copy of `index/` (opening an index removes `*.tmp` files and replaces a
   damaged manifest), and the delete-time index invariant (M17 rule 5) builds a
   second scratch index from the footers. The scratch directories live under
   the system temporary directory and are removed on exit.
3. **Hashing through `CommitCodec::object_hash`.** `seg_hash` and the volume
   hash are BLAKE3-256, which `object_hash` is; this avoids a `blake3`
   dependency, at the price of reading one container's extent region (a
   segment is 16 MiB) into memory at a time.
4. **The journal is read with `lunix_format::journal::scan_file`**, the
   normative reader, and merged by `write_seq` as recovery does; it never
   calls `Journal::recover` (which wipes tails on the first lane open).
5. **Orphans.** A sealed segment or pack is an orphan when no commit file on
   disk names it (below `chain_floor` included: such a commit is waiting for
   GC), no retained root references it and the condemned list does not name
   it. `orphan` when the volume was closed cleanly (no `*.open` or `*.tmp`
   file, no `state/device`), `crash_leftover` otherwise.
6. **Repairs run first, then the checks run again** and the report shows the
   state after. `--truncate-tip` lowers the floor first (a rerun after a crash
   is idempotent), then moves the tip aside, then zeroes the checkpoint.
   `--delete-orphans` is refused together with `--truncate-tip` (the zeroed
   checkpoint loses `id_high`) and records `id_high` in a fresh checkpoint slot
   before it deletes anything.
7. **Lowering the rollback floor is a stub.** `lunix-iface` has no way to
   lower the floor; see `FloorLowering` and the M14 report (proposal P1).

## Finding codes

The table is `lunix_fsck::code::ALL` (a test keeps it complete); the list
below is the same table.

| code | severity | meaning |
|---|---|---|
| `volume_parse` | error | the volume descriptor does not parse |
| `volume_mac` | error | the descriptor MAC or checksum does not verify |
| `keyring_parse` | error | the keyring is missing or does not parse |
| `credential` | error | no credential was given, or it unwraps no slot (a damaged keyring looks the same) |
| `attach` | error | the attach check refused the volume (downgrade, mode mismatch, pinned fingerprint) |
| `keyring_rollback` | error | the keyring is older than the highest keyring_seq this host has seen |
| `busy` | error | the volume is locked by another process |
| `io` | error | an i/o error stopped a check |
| `volume_json` | error | volume.json is missing or invalid |
| `state_pins` | error | state/pins is rejected (a rejected file is never read as 'no floor') |
| `state_device` | warning | state/device does not parse or names another volume |
| `checkpoint` | warning | a checkpoint slot is damaged or describes a commit above the tip (a hint only) |
| `pin_object` | error | a snapshots/, readers/ or holds/ object does not authenticate |
| `condemned_invalid` | warning | state/gc/condemned does not decode (ignored; GC re-marks) |
| `no_floor` | note | no rollback floor is recorded: rollback of earlier history cannot be detected on this host |
| `no_commit` | error | state/commits holds no commit |
| `tip_auth` | error | the tip does not authenticate (repair: --repair --truncate-tip) |
| `tip_not_full` | error | the tip is not a FULL commit |
| `commit_missing` | error | a commit at or above chain_floor is missing |
| `commit_auth` | error | a retained commit does not authenticate or decode |
| `chain_broken` | error | commit_seq, parent_hash or epoch of a link is wrong, or genesis does not name the volume |
| `commit_below_floor` | warning | a commit file below chain_floor does not authenticate (legal only while condemned) |
| `chain_floor` | warning | chain_floor is not the older of the retain_last retained commits |
| `chain_floor_above_tip` | error | chain_floor is above the tip |
| `created_at` | warning | created_at decreases along the chain (a clock step; never a mount error, D17) |
| `size` | error | the tip's size is not a multiple of the chunk size |
| `rollback` | error | the tip is below the rollback floor in state/pins |
| `fork` | error | the chain has another commit than the one the rollback floor recorded |
| `fork_unverifiable` | error | a link between the tip and the rollback floor is missing or does not authenticate |
| `map_node_missing` | error | a map node's pack is missing or failed its own checks |
| `map_node_loc` | error | a map child's location is not an extent of its pack |
| `map_node_auth` | error | a map node extent does not authenticate |
| `map_node_corrupt` | error | a map node extent is structurally invalid |
| `map_node_codec` | error | a map node is not codec 0 (R2) |
| `map_node_id` | error | a map node extent carries another id than the hash its parent names (R2) |
| `map_node_hash` | error | a map node's recomputed hash differs from child_hash (R2) |
| `map_node_shape` | error | a map node's level, fanout or children do not fit its position |
| `map_root` | error | the map root does not fit the commit's size or its empty-root hash |
| `map_entry_range` | error | a map entry lies past the end of the volume |
| `map_entry_unresolved` | error | a map entry resolves to no extent, by hint or through the index |
| `index_invariant` | error | a map entry with a stale hint does not resolve once the index is rebuilt from the footers |
| `container_trailer` | error | a segment or pack trailer is damaged or names another object |
| `footer_auth` | error | a footer does not authenticate (R3); the container counts as absent |
| `footer_corrupt` | error | a footer does not decode or contradicts its trailer |
| `seg_hash` | error | the extent region does not match the footer's seg_hash (R3) |
| `footer_entries` | error | the extents found in the region differ from the footer entries (offset, length, codec or id) |
| `extent_auth` | error | an extent's tag or AEAD does not verify, or its id is not the recomputed id (--deep) |
| `extent_id` | error | an extent's header id differs from its footer entry (--deep) |
| `orphan` | warning | a sealed segment or pack that no retained root references and nothing condemns (volume closed cleanly) |
| `crash_leftover` | warning | a sealed segment or pack that no retained root references (the volume was not closed cleanly) |
| `leftover` | warning | a temporary or unsealed file (repair: --repair) |
| `index_run` | error | an index run or the manifest is damaged (repair: --rebuild-index) |
| `index_missing` | warning | chunks in sealed segments have no index entry (mount catches up) |
| `index_stale` | warning | index entries name an extent that is not at that location |
| `index_extra` | warning | index entries for chunks no segment holds |
| `index_file` | warning | an unnamed or unreadable file in index/ |
| `journal_uuid` | error | a journal file belongs to another volume |
| `journal_damage` | error | journal damage above the floor with valid records after it: recovery would stop there |
| `journal_damage_below_floor` | warning | journal damage at or below the floor (covered by the commit) |
| `journal_tail` | note | a torn last record of a lane: crash residue, legal |
| `ignored_after_gap` | warning | valid records beyond the first gap of the merged write_seq order are ignored by recovery |
| `journal_file` | warning | a file in journal/ has an unusable name or header |
| `plain_notice` | note | a plain volume detects corruption only: no check can find a writer who recomputed the checksums |
| `repair_refused` | error | a requested repair was refused; nothing was changed by it |
| `gc_pending` | note | GC work is pending in state/gc |
