# Review: M14 (`lunix-fsck`) and M13a (`lunix-cli`)

- Reviewed: `p1/m14` at `eb076a0` and `p1/m13a` at `f57e093`, both on base `59251e6`, 2026-11-14.
- Method: full read of `crates/lunix-fsck/src` and the M13a sources, against the M14 and M13a plan sections, rule 13
  (R1-R4), ADR-31, DESIGN §4.1 and §4.4 (main's current journal rule), M17 rules 1-7 and 12, and the iface rustdoc.
  Three findings were confirmed with tests on the remote test host (section "What I ran").
- Nothing was committed or edited in either worktree.

## Verdicts

| milestone | verdict |
|---|---|
| **M14** | **Block until B1 is fixed**, then merge after the majors (M1-M8). The check side is thorough and I found no tampering case of R1-R4 that it misses. The problems are in the repairs. |
| **M13a** | **Merge after fixes**: one major (a wrong, destructive hint), the rest minor. Secret handling, modes and the exit-code table are right. |
| **P1** (`reset_rollback_floor`) | **Accept**, with four amendments (section "Proposal P1"). |

---

## M14: blocker

### B1. `--delete-orphans` deletes live segments, and `--rebuild-index` leaves them out, when the index cannot resolve stale hints

**Where:** `src/repair.rs:416-426` (the guard), `:432-441` (the delete set), `:105-143` (delete runs before the index
rebuild); `src/indexcheck.rs:243-256` (`rebuild` takes only "in use" segments); `src/resolve.rs:79-131`
(`referenced` is filled only by entries that resolved).

**Cause.** A segment is "in use" when a commit file on disk names it, or a map entry *resolved* into it, or the
condemned list names it (always empty in M14). For an old segment (its naming commit is below `chain_floor` and GC
deleted the commit file) that the retained roots reach only through stale hints, the second condition depends on the
on-disk index. When the index is missing, damaged, or the scratch copy could not be made (`open_view` returns
`None`), those entries are `map_entry_unresolved`, the segment is in no set, and:

- `--repair --delete-orphans` deletes it. The guard checks only `tip`, `chain_ok` and `tip_complete`. It does not
  check unresolved entries, a failed walk of a retained root other than the tip, or a missing index view.
- `--rebuild-index` (and plain `--repair`, which rebuilds when it sees `index_missing`) builds the new index without
  that segment, so the entries stay unresolved although the data is on disk, and the segment is then reported as an
  orphan for the next GC or `--delete-orphans` to remove.

The helper for the invariant check (`indexcheck::rebuilt`, `:146-160`) already uses every good segment; the real
rebuild does not.

**Confirmed.** On a volume whose two retained roots have stale hints only and whose older commit files are gone
(the plan's own stale-hint model, `tests/stale.rs`, plus `rm` of the commits below `chain_floor`), with `index/`
emptied:

- `--repair --delete-orphans` printed `repaired delete_orphans 3 segments and 3 packs deleted` and removed **every
  segment of the volume**; the same volume with its index is `warnings` only and resolves every entry.
- `--rebuild-index` printed `rebuilt from the footers: 0 entries` and left 488 `map_entry_unresolved` errors.

The state is reachable in the design: M17 rule 3 makes liveness "the copy the index names", so GC deletes a segment
that hints still point into and the surviving copy is found only through the index (rule 5, first alternative: "the
index durably names the current copy"). `index/` being lost is the case `--rebuild-index` exists for.

**Fix.**
1. `rebuild` takes every good sealed segment whose id is at or below the high-water mark (the highest id a retained
   commit names), plus the named ones, as DESIGN §4.4 step 4 and M17 rule 7 draw the line. Segments above the mark
   are the open-time sweep's.
2. Orphans are computed only from a complete resolution: refuse `--delete-orphans` when any retained root's walk
   failed, when any entry is unresolved, when the index view was not available, or when `roots()` failed or is not
   empty (M2). The simplest safe rule: refuse when the check pass has any error-severity finding.
3. Run the index rebuild first, then the checks again, then delete orphans from the second pass's facts.
4. A segment above the high-water mark that no commit names may be deleted without the resolution (the engine does
   the same at open).
5. Add the two tests above to `tests/repair.rs` or `tests/stale.rs`.

---

## M14: majors

### M1. `--repair --truncate-tip --reset-floor` leaves the floor above the new tip

**Where:** `src/repair.rs:80-94`, `:296-336`. After a successful truncate the function goes on with the facts of the
check that ran **before** it. `reset_floor` reads `facts.tip`, which is the commit just set aside.

**Confirmed.** Floor one commit behind the tip (a crash between the commit and the floor raise), tip authentic
(deviation 9 allows it): the run printed `truncate_tip: commit 3 moved aside ... commit 2 is the tip` and then
`reset_floor: floor was commit 2, now commit 3`. The next check, and so the next attach, is `rollback`. The volume
is unmountable until a second reset, which needs the lowering path.

The index rebuild in the same run also uses the old tip's facts.

**Fix.** After a successful truncate, run the checks again and feed the new facts to every later repair, or refuse
`--reset-floor` and `--rebuild-index` in a run that truncates. Add the test.

### M2. Pins (`snapshots/`, `readers/`, `holds/`) are not roots for the orphan decision

**Where:** `src/state.rs:73-79` only checks that `roots()` parses; `src/repair.rs:416` and `src/resolve.rs:207` never
look at it. The iface says of `PinSource`: "GC reads them so no version can delete a future pin's base" and "GC must
not run past a pin it cannot read". fsck deletes past both: with a `pin_object` error, or with a valid pin on a
commit below `chain_floor`, `--delete-orphans` removes segments only that pinned root references.

Phase 1 has no pins, so nothing breaks today. **Fix now, one condition:** refuse `--delete-orphans` when `roots()`
fails or returns anything, until fsck walks pinned roots.

### M3. The map walk keeps one open descriptor per pack, without a bound

**Where:** `src/mapwalk.rs:55`, `:222-232` (`files: HashMap<ObjId, File>`, never evicted).

Map packs are deleted whole only and are not compacted in phase 1 (M17 rule 9), so a volume that has lived for a
while has one live pack per commit that left a node behind: thousands. At the usual soft limit of 1024 descriptors
the walk gets `EMFILE`, reports `io` errors, and exits 2 on a healthy volume. `lunix-store` bounds its own cache at
256 for the same reason. Not run; read from the code.

**Fix.** An LRU of a few hundred descriptors, or open, `pread` and close per node (the walk visits a pack's nodes
close together), plus a test with a low `RLIMIT_NOFILE`.

### M4. The binary takes the passphrase from the environment

**Where:** `src/bin/lunix-fsck.rs:14`, `:49-57`; `src/lib.rs:83-91`.

The plan and rule 4 say a passphrase comes from a terminal prompt, `--passphrase-fd` or a key file, "never from argv
or the environment"; M13a tests exactly that. `--passphrase-env V` puts the secret in `/proc/<pid>/environ` and in
every child. It is also held as a plain `Vec<u8>` in a `Clone` struct (rule 3: keys live in `Zeroizing` memory), and
`to_string_lossy` silently changes a passphrase that is not UTF-8, so such a volume cannot be unlocked.

**Fix.** `--passphrase-fd N` (the M13a reader can move to a shared place, or the binary reads the descriptor
itself), `Zeroizing<Vec<u8>>` in `Credentials`, no `Clone`.

### M5. The scratch copy of the index goes to a predictable, shared, world-readable place

**Where:** `src/indexcheck.rs:31-37` (`temp_dir()/lunix-fsck-<pid>-<n>-<tag>`, `create_dir_all`), `:75-106`.

- `create_dir_all` succeeds on a directory that already exists, so another local user can create the name first and
  own the directory fsck then copies the index into and reads it back from. What comes back decides which entries
  resolve, and so (B1) which segments are "in use".
- The directory gets the default mode, so on a multi-user host the chunk ids and locations of a volume whose own
  directory is private are readable in `/tmp` for the length of the run.
- `/tmp` is often tmpfs: the copy is RAM, on top of M7.

**Fix.** Create the directory with mode 0700 and fail if it exists (retry with another name), preferably next to the
volume directory on the same file system (a sibling, not inside it, so "a check never writes to the volume" still
holds) and with a reflink or hard link where possible. Better: ask the lead for a read-only open of `LsmIndex`.

### M6. A relative volume path of one component looks for the keyring in `/families`

**Where:** `src/session.rs:117-118`. `Path::new("vol").parent()` and `Path::new(".").parent()` are `Some("")`, not
`None`, so the `unwrap_or_else(".")` never runs and `local_keyring_path("", ..)` is `/families/<id>/keyring`.
`cd <dir> && lunix-fsck <uuid>` on a keyed volume exits 2 with `keyring_parse`. **Fix:** `std::path::absolute(dir)`
first; a test with a relative path.

### M7. Memory grows with mapped and distinct chunks; the doc and the report understate it

**Where:** `src/lib.rs:47-52` ("the walk is streaming"), report deviation 10 ("about 100 bytes each");
`src/mapwalk.rs:27-33`, `src/resolve.rs:156`, `src/indexcheck.rs:169-176`.

The gate is met as written (1 M mapped chunks, 84 MiB). What the code holds for a real volume, from the types:

| structure | per item | lifetime |
|---|---|---|
| `tip_entries: Vec<EntryRec>` | 64 bytes per **mapped** chunk (dedup does not shrink it) | whole run |
| `Container::exts` | about 48 bytes per extent in every footer | whole run |
| `unique_ids: HashSet<ChunkId>` | about 40-70 bytes per distinct chunk | resolution |
| `footer_ids` and `seen` in `compare` | twice about 40-70 bytes per distinct chunk | index compare |

That is roughly 250 bytes per distinct mapped chunk at the peak: about 4 GiB for a full 1 TiB volume at 64 KiB
chunks and about 16 GiB at 16 KiB, before the index copy in the temporary directory (M5). These are estimates from
the code, not measurements. An fsck that is OOM-killed on the volumes that most need it is a release problem, not an
M14 gate failure.

**Fix for this milestone:** correct the doc and the report, and add a second memory test with many distinct extents
(for example 2 M) that records the real figure. **Follow-up (lead's call):** resolve in streaming order (sort entries
per segment, drop them after resolution), replace the three id sets with sorted vectors or per-extent bits in
`Container`, and print an estimate before starting.

### M8. `--truncate-tip` does not check that the previous commit is still a root

**Where:** `src/repair.rs:182-226`. The checks are: the previous file exists, authenticates, is a FULL, and its
chain verifies. Nothing checks that its map still resolves. With `retain_last = 1` (or after it was lowered), the
previous commit is below the tip's `chain_floor`, GC may already have deleted what only it references, and the
commit file is still there because the rollback floor keeps it (M17 rule 6). Truncating then gives a tip whose map
points into deleted segments, with the journal for that range reclaimed.

**Fix.** When the tip authenticates, refuse if `prev.seq < tip.chain_floor`. In every case walk the previous root
and resolve its entries before changing anything, and refuse (or require a second explicit flag) when it does not
resolve. When the tip does not authenticate, warn if `retain_last < 2`.

---

## M14: minors

1. `src/journal.rs:131-143`, `:230-234`: a ring file whose **header** is damaged, at the end of its lane, is the note
   `journal_tail` and exit 0, although every record in it is gone. A torn tail is a damaged record, not a damaged
   header of a file that was written long before. Make a bad header of a non-zero file a warning at least.
2. `src/repair.rs:256-264`: the truncate message says records `cut+1..=q-1` are lost. With that gap recovery ignores
   **every** record above the new cut (`ignored_after_gap`, shown only by the second pass). Say so in the action.
3. `src/repair.rs:416-489`: after `--delete-orphans` the on-disk index still names the deleted segments. Mount's
   `catch_up` drops them, so it is not a loss, but the run ends with `index_stale` or `index_extra` warnings that it
   caused. Call `drop_segment` or rebuild after the delete.
4. `src/repair.rs:403`: `rewrite_checkpoint` zeroes both slots and then stores; a crash between loses `id_high`.
   Store first, then store again to replace the damaged slot.
5. `src/containers.rs:64-73`: `read_dir` errors are swallowed, so an unreadable `segments/` looks empty. Report `io`.
   Files in `segments/` and `map/` that are not container names, and `*.damaged` commits, are not reported at all.
6. `src/containers.rs:172-183`: the footer buffer is allocated from the unauthenticated trailer before the footer
   authenticates. It is bounded (the container limit is 4 GiB), but a sparse file with a made-up trailer costs more
   than 1 GiB per file. Cap `footer_len` by the real file length and by a smaller constant.
7. `src/chain.rs:173-186`: a `chain_floor` **below** the expected value cannot come from a raised `retain_last` and
   should be an error; only "above" needs to be a warning (deviation 4).
8. `src/chain.rs:240-275`: `state/pins` absent on a volume whose tip is above genesis is only the note `no_floor`
   (ADR-31: the floor is written at creation). A warning would be honest. `pins.max_epoch` is not compared with the
   chain's epoch; it is 0 in phase 1, add it with phase 2.
9. `src/mapwalk.rs:197`: an empty root is accepted when its hash is the empty hash of **any** level; only the root's
   own level is right.
10. `src/repair.rs:127`, `src/indexcheck.rs:91-97`: `index_file` triggers a rebuild on every `--repair` but the
    stray file stays, so the finding never clears.
11. `src/repair.rs:150-163`: `--repair --gc` always records a `gc` action, which forces a second full check pass for
    no change.
12. `src/lib.rs:224-228`: a repair that changed something and was then refused (floor lowered, set-aside failed)
    records no action, so the report shows the state before the change.
13. A keyed volume with no credential gets no check at all, not even the unkeyed ones (trailers, journal CRCs).
    Worth one line in "what is not covered".
14. Taking the lock rewrites `LOCK` with the pid, so "a check never writes" has one exception and a read-only
    medium cannot be checked. This is the store's behaviour; say it in the README.
15. The report names paths under the remote home (`lunix-p1/m14/{src,target}`, line 7). The rules ask for none.

### What I looked for and did not find

- **A check that writes.** Apart from the `LOCK` pid, no write on the check path: the index is opened on the copy,
  the journal is read with `scan_file`, the store calls used are readers. `--delete-orphans`, `--truncate-tip`,
  `--reset-floor` and `--gc` without `--repair` are refused before anything changes.
- **R1-R4 gaps.** R2 (codec 0, header id, recomputed hash, level and location against the authenticated footer) and
  R3 (footer through `MetaCodec::open` under the container's name, `seg_hash` over the whole region, failed
  container treated as absent) are complete. R4: `rollback`, `fork` at the floor height, the walk below
  `chain_floor` with every link authenticated, and `fork_unverifiable` for a missing or damaged link match ADR-31.
  A rejected `state/pins` is an error and is never replaced. Orphan deletion and truncation never happen without
  their flags.

### Deviations 1-13 of the M14 report

| # | subject | view |
|---|---|---|
| 1 | floor lowering stub | fine; replaced by P1 |
| 2 | no `blake3`, region read whole | accept |
| 3 | own LZ4 encoder in tests | accept |
| 4 | `chain_floor` mismatch is a warning | accept for "above"; "below" should be an error (minor 7) |
| 5 | commits below `chain_floor` keep what they name out of the orphan list | accept: it errs on the side of keeping, and GC collects it. It does not cover the B1 case (the commit file is gone) |
| 6 | condemned list not parsed until M17.B | accept |
| 7 | damage with no later record in its lane is `journal_tail`, also above the cut | accept for a damaged record: with several lanes a torn record with later records in other lanes is a legal crash state, and it still gives `ignored_after_gap`. Not for a damaged file header (minor 1) |
| 8 | `--delete-orphans` refused with `--truncate-tip` | accept; M1 asks for the same care with `--reset-floor` |
| 9 | `--truncate-tip` allowed on a tip that authenticates | accept only with M8 |
| 10 | memory | understated (M7) |
| 11 | exit 64 for usage | accept; M13b maps it to 2 |
| 12 | scratch copy in the temporary directory | needs M5 |
| 13 | `gc.rs` contract | accept |

---

## Proposal P1: `VolumeDir::reset_rollback_floor`, gated by `StoreOptions::allow_floor_reset`

**Recommendation: accept.** ADR-31 names exactly this path, the gate at open time keeps a running daemon from
lowering its own floor, and the order in `--truncate-tip` (floor, set-aside, checkpoint) is idempotent after a
crash. Four amendments:

1. **The store checks what it can.** It holds the commit files and the codec: refuse unless the commit at
   `floor.max_commit_seq` exists and its stored bytes hash to `floor.tip_hash`. For `TruncateTip` also require the
   new height to be below the recorded one. A caller with stale facts (M1) then cannot record a floor that makes the
   volume unmountable.
2. **A durable audit record.** ADR-31 says "audit-logged". A warning in `take_warnings` and a line on stdout are gone
   when the terminal closes. Append a line (time, reason, old and new floor) to a small local file in `state/`, or
   send it to syslog from the binary; the lead picks which.
3. **A rejected `state/pins` has no repair path.** The proposal returns `Corrupt` and never replaces the file, which
   is right for the default. It leaves a volume with a damaged pins file unattachable for good, with no tool but
   `rm`, which also drops `max_keyring_seq`, `max_epoch` and the pin without a trace. Either add an explicit third
   reason that replaces a rejected file and is audited, or document the manual step and what it gives up.
4. **Keep the misuse surface small.** `StoreOptions` is public, so nothing stops `lunixd` from setting the flag. Add
   a contract test for "refused without the option" (the proposal has it) and a CI grep that only `lunix-fsck` sets
   `allow_floor_reset`. An offline-only trait that `dyn VolumeDir` does not carry would be stronger, but the flag is
   enough.

---

## M13a: major

### A1. The hint for `rollback` and `fork` recommends `--truncate-tip`

**Where:** `crates/lunix-cli/src/error.rs:102-105`.

> lower the floor offline with `lunix fsck NAME --repair --truncate-tip` (loses the commits above it)

For `rollback` the tip is already **below** the floor. `--truncate-tip` sets that tip aside as well and makes its
predecessor the tip: the operator loses one more commit and no "commits above" exist. For `fork` it also drops a
commit. The repair that accepts the present copy without losing anything is `fsck --repair --reset-floor`, and the
first advice should stay "restore the newer state".

**Fix.** Change the text to name `--reset-floor` (after the operator has decided this copy is the right one) and keep
`--truncate-tip` for a tip that does not authenticate. `FsckArgs` (`src/fsck.rs`) has no `--reset-floor` and no
`--delete-orphans`; M13b must add both or the hint names a flag `lunix fsck` cannot pass.

## M13a: minors

1. `src/secret_input.rs:111-121`: `--passphrase-fd` pointing at a terminal (for example `--passphrase-fd 0` typed
   interactively) reads to end of input with echo on. Refuse a terminal there (`IsTerminal`) and point at
   `--passphrase`.
2. `src/secret_input.rs:93-98`: a trailing `\r` is removed even when no `\n` came before it, so a passphrase from a
   file that ends in `\r` is changed. Strip `\r` only as part of `\r\n`. The decision text says "minus one trailing
   newline".
3. `src/secret_input.rs:127-155`: `stty` is found through `PATH`, and SIGINT at the prompt leaves echo off (the
   report says so). Use `/bin/stty` or an absolute lookup; acceptable until a termios binding is allowed.
4. `src/client.rs:44-50`: no read timeout, so a daemon that accepts and never answers hangs the CLI. Attach can be
   slow, so a long default with an option is enough.
5. `src/fsck.rs:37-40`: the help of `--truncate-tip` says "drop the commits above the last good one"; it drops the
   tip only.
6. `src/secret_input.rs:54-56`: passphrases that differ are exit 1; exit 2 fits the table better.
7. The report names the host alias and its CPU count (lines 7, 57, 63). The rules ask for none.
8. The report's CI section says the full `tools/ci.sh` did not complete (workspace tests of other crates). The lead's
   full run has to stand in for it before the merge.

### Decisions 1-5 and the focus points

- **Secrets:** never in argv or the environment (canary test against the live child's `/proc` files), read one byte
  at a time into one preallocated buffer, carried as `Secret` (zeroized, redacted `Debug`), and the wire payload is
  `Zeroizing`. Daemon messages are stripped of control characters. No logging exists that could leak. Good.
- **Decision 2 (explicit `--credential-mode`):** right. The CLI never copies the volume's mode from `status`; absent
  with key material means `encrypted`, as the iface says, and the hint explains the integrity case.
- **Exit codes:** taken from `ErrorCode::exit_code`, so the table cannot drift: 3 busy, 4 not found, 5 no space,
  6 downgrade, wrong key, rollback, fork and fork_unverifiable, 7 journal damage, 2 usage and `invalid`, 1 the rest
  and an unreachable daemon. The help prints the table.
- **Default integrity and `--plain`:** no flag sends `integrity` and no key file; the three flags are one clap group
  (exit 2 in pairs); `--plain` conflicts with key options and prints the snapshot-tested warning on stderr before
  any prompt.
- **Decisions 1, 3, 4, 5:** accept. Decision 4 (`stty`) is the weakest; see minor 3.
- **Gap that stays open:** the daemon-backed checks the report lists (key file 0600 written by the daemon, attach
  without a credential) wait for M12a.

---

## What I ran

All on the remote test host, without root, on 4 CPUs while the lead's CI ran on the others, one cargo job at a
time, from scratch copies of the two worktrees (deleted afterwards, with the host directory).

| run | result |
|---|---|
| `cargo test -p lunix-fsck` (branch as is, `RUST_TEST_THREADS=1`) | 56 passed, 0 failed (unit 2, clean 3, flips 1, journal 7, matrix 16, memory 2, repair 12, stale 4, tamper 9): the report's counts |
| `cargo test -p lunix-cli` (branch as is, `RUST_TEST_THREADS=1`) | 53 passed, 0 failed (unit 13, canary 3, help 3, size_prop 4, socket 27, warn 3): the report's counts |
| four review tests added to a scratch copy of `lunix-fsck/tests` (below) | 1 passed, 3 failed as predicted: B1 twice, M1 once |

Not run: `tools/ci.sh`, clippy, anything as root, and no test for M3 (descriptor limit) or M7 (memory); those two
are read from the code.

### The review tests

They reuse `restamp`, `stale`, `spec` and `clear_index` of `tests/stale.rs` and `DirectPins` of `tests/common`.

```rust
/// Restamps again on top of the previous restamp.
fn restamp_next(b: &mut Built) {
    restamp(b, stale);
    let seq = b.known.tip + 1;
    let bytes = b.read(&b.commit_rel(seq));
    b.known.tip = seq;
    b.known.tip_hash = b.crypto.meta().object_hash(&bytes);
}

/// Both retained roots have stale hints only, and the commits that named the
/// segments are gone (GC deleted them below chain_floor).
fn aged(mode: lunix_format::CryptoMode) -> Built {
    let mut b = build(&spec(mode));
    let old_tip = b.known.tip;
    restamp_next(&mut b);
    restamp_next(&mut b);
    for s in 0..=old_tip {
        std::fs::remove_file(b.path(&b.commit_rel(s))).expect("remove old commit");
    }
    b
}

#[test] // passes: with its index the volume resolves every entry (warnings only)
fn review_aged_volume_is_clean_with_its_index() {
    let b = aged(lunix_format::CryptoMode::Integrity);
    let r = b.fsck(&b.opts());
    assert!(r.exit_code() <= 1, "{}", r.render_text());
    assert!(!r.has(code::MAP_ENTRY_UNRESOLVED));
}

#[test] // FAILS (B1): "3 segments and 3 packs deleted", segments/ is empty
fn review_delete_orphans_without_an_index_keeps_live_segments() {
    let b = aged(lunix_format::CryptoMode::Integrity);
    let before = list(&b.dir, "segments");
    clear_index(&b);
    let r = b.fsck(&Options { repair: true, delete_orphans: true, ..b.opts() });
    assert_eq!(before, list(&b.dir, "segments"), "{}", r.render_text());
}

#[test] // FAILS (B1): "rebuilt from the footers: 0 entries", 488 unresolved entries
fn review_rebuild_index_without_an_index_restores_resolution() {
    let b = aged(lunix_format::CryptoMode::Integrity);
    clear_index(&b);
    let r = b.fsck(&Options { rebuild_index: true, ..b.opts() });
    assert!(!r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
}

#[test] // FAILS (M1): "floor was commit 2, now commit 3", then `rollback`
fn review_truncate_tip_with_reset_floor_leaves_a_mountable_floor() {
    let b = build(&spec(lunix_format::CryptoMode::Integrity));
    // The floor lags one commit behind the tip.
    let prev = b.known.tip - 1;
    let prev_hash = b.crypto.meta().object_hash(&b.read(&b.commit_rel(prev)));
    {
        let store = b.open_store();
        DirectPins
            .set_floor(
                &store,
                &RollbackFloor { max_commit_seq: CommitSeq(prev), tip_hash: prev_hash },
                "test",
            )
            .expect("set floor");
    }
    b.fsck(&Options {
        repair: true,
        truncate_tip: true,
        reset_floor: true,
        floor_lowering: Some(Arc::new(DirectPins)),
        ..b.opts()
    });
    let again = b.fsck(&b.opts());
    assert!(!again.has(code::ROLLBACK), "{}", again.render_text());
}
```
