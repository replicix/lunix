# Report p1-jwin: ADR-33 T1 and T2 (the resident journal window and the read-around)

Branch `p1/jwin` from `origin/main` 49c6606. Spec: ADR-33 (accepted 2026-11-19, W2 and W6 as the maintainer chose) and the
note `docs/explanation/research/2026-11-19-write-throughput.md` (§4.1, §7). Builds and tests ran only on the remote test host
(kernel 7.0.0) through `tools/remote.sh`, CPUs 8-15, `-j 4`. No root run, no on-disk format change.

## What was built

| item | what | where | tests |
|---|---|---|---|
| T1 copy and index | a lane copies each batch's record bytes at the seal (`submit_open`), when the cap admits them (the bytes are reserved then), and indexes the copy per lane by `(ordinal, batch offset)` when the write completes, before the `Append` completions and while the file still counts a write in flight | `lunix-journal/src/{lane,window}.rs` | `what_was_written_is_served_from_memory_not_from_the_file` (the file is damaged on disk, the read still returns the written bytes) |
| T1 hits | `Journal::read_at` and `JournalLane::submit_read` look in the window after the file-in-use check (a reclaimed location is still `NotFound` / `ENOENT`); a `submit_read` hit completes at once and signals the lane's wake fd so the queue polls | `journal.rs`, `lane.rs` | same test, `a_lane_read_that_misses_still_reads_the_file` |
| T1 release | `reclaim` releases a file's entries once the file is `Recycling`, before the zero rewrite; `recover` and the first `prepare` clear the window | `journal.rs` | `reclaim_releases_exactly_the_entries_of_the_recycled_files` |
| T1 cap | `JournalConfig::resident_max` (`None`: `journal_max`, never above it; at least one maximal batch). A batch past the cap is not admitted, the oldest stay | `config.rs`, `window.rs` | `past_the_cap_a_batch_is_not_admitted_and_the_oldest_records_stay`, `a_cap_above_the_ring_is_the_ring_and_a_cap_below_a_batch_is_refused` |
| T1 counters | `FileJournal::window_stats()` -> `WindowStats` (resident bytes, cap, hits, misses, batches not admitted, window reads, completing reads, checksum failures) | `window.rs`, `journal.rs` | asserted in the tests |
| T2 read-around | a `read_at` miss reads the 1 MiB-aligned window around the location once (single flight per `(lane, ordinal, window)`; the other misses wait and use the leader's result), scans it with the recovery reader's rules (magic, file `gen`, the record's own offset, crc32c, 8-byte resync), makes the verified records resident within the cap and serves the target out of its verified record | `window.rs` | `concurrent_misses_in_one_window_read_it_once`, `the_first_cut_after_a_restart_reads_a_window_per_megabyte` |
| T2 CRC (W6) | only verified records are served or made resident; a record that fails its checksum, or has no valid header where the location says, fails the read with `Corrupt` and `buf` is not written. A sub-range read of a damaged record fails too | `window.rs` | `a_record_damaged_on_disk_after_the_write_fails_the_read_and_is_never_served`, `a_damaged_header_fails_the_read_too`, unit tests of the scanner |
| T2 cut records | a record cut by the window's end is completed with one more read (forward; done by the leader, so the walk through the file does not read the window again for it); a record that starts before the window and holds the target is found with one backward read of at most 1 MiB | `window.rs` | `a_record_cut_by_the_window_edge_is_completed_with_one_read` (both directions, exactly one window read and one completing read), `a_record_that_runs_a_few_bytes_past_the_edge_needs_the_completing_read` |
| T2 file ends, reclaim | the last window is clamped to the file length; a location reclaimed meanwhile gives `NotFound` or `Corrupt`, never other bytes, and leaves nothing resident | `window.rs` | `files_that_are_not_a_megabyte_multiple_end_in_a_short_window`, `a_location_reclaimed_while_it_is_read_never_returns_stale_bytes`, `readers_see_exact_bytes_during_appends_and_reclaim` |
| property | window and file return the same bytes (live, with and without a one-batch cap, and after a restart), for random records and sub-ranges, and they equal the written pattern | `tests/window.rs` | `the_window_and_the_file_return_the_same_bytes` (64 cases, one or two lanes) |
| config | `lunixd.toml` key `journal_resident_max` (at least 2097152, default the volume's `journal_max`), in the reference table of `lunixd/src/config.rs`, wired through `OpenParams::journal_resident_max` to `JournalConfig::resident_max` | `lunixd/src/{config,daemon}.rs`, `stack/mod.rs` | `journal_resident_max_is_validated`, the full-file parse test |
| docs | DESIGN §4.1 (resident window, read-around, reclaim) and §5 step 1; the lunix-journal crate docs (`lib.rs`, `window.rs`; the README is over the 40-line limit the ownership check allows to change) | | |

Not changed: the acknowledgement, the group flush, FUA, C6, recovery and every on-disk byte. `coord.rs` cut sizing is untouched.

## Deviations and choices

1. **Ownership.** `[jwin]` also lists `crates/lunixd/src/{config,daemon}.rs`, `crates/lunixd/src/stack/mod.rs` and
   `docs/explanation/DESIGN.md`: the `lunixd.toml` key lives in lunixd, the DESIGN notes in the spec. These paths overlap the entries of other
   milestones (`pace`, `gcrate`, `bstop`); the edits are small and local, a merge conflict is possible only in `stack/mod.rs` and `daemon.rs`.
2. **The leader completes the cut record eagerly.** The ADR says a record cut by the window edge is completed with one more read; I do it when the
   window is read, whether or not the triggering location is in that record. Without it, the walk through the file read the same window again
   when it reached the cut record (the first version of the first-cut test showed 3 window reads for 2 windows). A record that cannot be
   completed or verified is left for the read that needs it. The cost is one read of at most the rest of one record per window with a cut.
3. **Past the cap a window is shared only among the threads that miss in it at that moment** (the leader's parsed window is handed to the waiting
   followers). A later miss in the same window reads it again. With the default cap (`journal_max`) the cap does not bind, since the resident
   bytes are a subset of the ring's record bytes; a smaller `journal_resident_max` makes this the "used once and dropped" path of the note.
4. **`submit_read` misses stay a single asynchronous read of the piece** with no checksum, as before (the guest read path, not a chunk). Only
   `read_at` (destage, recovery replay) has the read-around, as W3 and W6 say. Its two early completions (a reclaimed location, an empty read)
   now signal the wake fd too, since a completion that is ready at once is otherwise polled only at the next unrelated wake.
5. **Destage failing the cut** is shown at the journal's interface: the damaged record is `Corrupt` (`EIO`), the buffer is untouched, and the
   destage assembly propagates a `read_at` error with `?` (`assemble.rs`). There is no new destage- or stack-level test that corrupts a ring
   block under a real cut; the existing stack and kill suites pass unchanged.
6. **A header cut by the window's end** (a record start within 56 bytes of the edge) is handled (the completing read fetches one more block) but
   cannot happen with this writer, which starts every record at a multiple of 64; that branch has no test.
7. **A forged record inside the payload of a record cut at the head of a window** could be taken as a record by the resync (the same acceptance as
   the recovery reader's: it needs the file's 64-bit `gen`, its own offset and a valid crc32c, which no guest write can see). The scan jumps over
   every verified record, so this concerns only the unsynchronised start of a window.
8. **`DECISIONS.md` is not edited** (not in this milestone's paths): ADR-33's "Not implemented yet" is stale for T1 and T2.
9. **Memory:** an entry per batch (written) or per record (read-around) is an `Arc<[u8]>`; a batch copy is `memcpy` of the packed bytes (no padding).
   Not locked.

## Review fixes (4b5301e: MERGE AFTER FIXES)

- **M1 (stale entry after the wipe).** Both fixes: a batch the cap did not admit evicts every entry it overlaps when its write completes
  (`Window::evict_range`), and `Window::clear` bumps a wipe epoch that a read-around captures before it reads and that `insert_record` checks
  under the lane lock (a follower's resolve uses the epoch of the window it resolves from). Tests: unit tests of both (`a_read_that_began_before_a_clear_...`,
  `a_batch_that_is_not_admitted_still_evicts_...`, `a_completed_batch_replaces_the_read_around_entries_...`) on a small cap, and
  `records_beyond_the_stop_point_read_before_the_wipe_never_come_back` (records beyond the stop point, a read before `open_lane`, new writes under a one-batch cap).
- **M2 (destage-level damaged record).** `lunixd/tests/stack_window.rs`: a plain volume, one-batch cap, 24 records of 64 KiB, a byte of a non-resident one
  flipped on disk, `commit_now` fails with `Corrupt`; the tip is unchanged, the engine is stopped and a second commit fails, no file outside the journal
  holds any of the written bytes (the segments of the failed cut are deleted), a resident record still reads. The rule that follows is the engine's for
  a Permanent failure (`coord.rs` `destage_failed`): no more commits in this process, writes fail, reads continue; the next start finds the damage with
  the recovery scan. Documented in the test's module comment.
- m1: a follower whose target is not found in the leader's window retries with a read of its own before reporting damage. m3: a `Corrupt` result for a
  location whose file is no longer the one that was read is `NotFound` and counts nothing; `Journal::read_at` documents `Corrupt` (the one doc line in
  `lunix-iface/src/journal.rs`, added to `[jwin]`). m4: a tail that is no record falls through to the backward read. m5: a lane dropped with
  writes that never completed returns their reservations. m6: evicted entries are dropped outside the lane lock. m7: one wake signal per poll for
  hits. m11: `resident_cap` is `#[must_use]`.
- m2, m8, m9: the crate docs, the config field docs, DESIGN §4.1 and the `window` module say that the scan does not apply the reader's `write_seq`
  order or its zero-block stop, that a small cap costs a 1 MiB read and check per window that was not made resident (a T6 cell should measure it), and that
  a guest read miss is unchecked, as ADR-33 allows (no read is added anywhere else; those bytes never reach a chunk).
- Tests: the data pattern is now a hash of (seq, byte index), so a shifted sub-range differs; the property test runs 64 cases over one or two lanes; the race
  tests have no sleeps (they count reads before and after the reclaim) and accept only `NotFound`, and check that nothing was counted as damage; new:
  a failed batch write gives its reservation back (`FileJournal::fail_next_batch_write`, a hidden test hook), a lane dropped with writes in flight, many
  threads past the cap, a header cut by the end of a buffer (scanner).
- Not done (optional): m4's `fill` still spends one completing read on a false tail; m6 only for the evictions in the window module; m10, m11's `as usize` casts.

## rust-skills conflicts

None.

## Second review round (41ac24e: MERGE AFTER FIXES)

- **Follower and the wipe.** A read that waits for another thread's window read and finds that read older than its own start (a wipe in between,
  `p.epoch != cx.epoch`) does its own read and resolve instead of serving from the old window.
- **Tests that fail without the wiring** (a reader held between its window read and its indexing, through `FileJournal::set_read_hook`, a hidden test hook;
  `plant_window_entry` is the second hidden hook). Each was run with its fix removed and failed, with it present they pass:
  `a_read_held_across_the_wipe_cannot_index_what_it_read` (the epoch captured after the read instead of before it: fails),
  `a_read_that_waits_for_a_window_read_before_the_wipe_does_not_use_it` (the follower's epoch check off: fails; `WindowStats::shared_reads` lets the
  test release the held read only once the follower waits, no sleeps), `a_batch_that_the_cap_did_not_admit_evicts_what_overlaps_it` (the `None` arm
  of `finish_write` removed: fails). Their rig writes F (255 blocks, admitted, leaving 8 128 bytes of room) and N (2 blocks, not admitted) where the
  damaged D and the stale valid S were; N's data is not `pattern(seq)` because S and N share a sequence number.
- **M2 control.** `stack_window.rs` now creates the volume with `CodecPolicy::None`; a control test commits the same writes undamaged and finds every
  record verbatim in the store; the failing run compares the file list (names and sizes) before and after the failed cut. `stack_window` is in the
  release stack list of `tools/ci.sh` (and `tools/ci.sh` in `[jwin]`).
- `fail_next_batch_write`: a relaxed load guards the swap. m7: `wake_raised` is set when the lane signals and cleared in `poll`; the error paths that push
  completions (a failed submit of a batch or of a read) signal too, so a hit never waits behind them. `resolve_before` keeps a clone of the record it offers
  to the window, so a refused insert frees it outside the lane lock.
- **m5 has no test.** The path (a failed `submit_and_wait` in the lane's drop) cannot be made to fail without dropping the ring while a write is in flight,
  that is, freeing a buffer the kernel may still write; the normal drop is tested (`a_lane_dropped_with_writes_in_flight_indexes_them_all`).

## Gate

GATE_PLACEHOLDER

## Commits

The branch holds the first-round commit (4b5301e), the review round (feb0d72, 145e44e, 9e3e7b6, fbb94a5), the merge of `origin/main` 62d4d59 (41ac24e) and
the commit of the second review round; the lead's message has the final hash.
