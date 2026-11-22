# p1-gcbatch: G1, one index update per batch for condemn and delete, no run scan, a footer cache in bytes

Branch `p1/gcbatch` from `main` `1738f2e`. Source: `gc-candidates.md` section G1 (the lead's study). A cost
reduction only: the collector condemns and deletes the same objects at the same points. **No on-disk format
change** (index manifest, runs, condemned list, segments are byte-identical in what they hold and how they
are written; the manifest simply gets fewer swaps). `docs/explanation/DECISIONS.md` is not edited.

## What changed

| | before | after |
|---|---|---|
| condemn | one list write, then one index manifest swap (3 syncs) per segment | the same list write, then **one** `mark_condemned_all` swap for all the segments of the pass (`reconcile`'s re-flagging and the urgent pass's condemn too) |
| delete | per segment: `drop_segment` (a scan of every run covering it, then a swap) and `unmark_condemned` (a swap), after the unlink | the unlinks, as before; the index follow-up is queued and made by **one** `retire_segments` swap (entries dropped and flag cleared by the same manifest) at the end of the step, of the delete phase, or when 256 are queued |
| `drop_segment` | counted the entries it dropped (a block-by-block CRC scan of the covering runs) and discarded the count | no scan. The count is read only by `catch_up`'s report (once at open, asserted by the contract tests); it stays there as `count_entries` |
| mark's footer cache | 32 entries, FIFO | `GcConfig::footer_cache_bytes`, default 32 MiB, FIFO bounded by the bytes the footers hold (40 bytes per extent) |

Code:

* `lunix-iface` `ChunkIndex`: two methods with default bodies that call the single-segment ones,
  `mark_condemned_all(&[SegId])` and `retire_segments(&[SegId])`. The single calls stay (fsck, tests).
  An interface addition, not a format change.
* `lunix-index` `LsmIndex`: both overridden, one `swap_manifest` each (none when nothing would change);
  `drop_locked` is split into `count_entries` (catch_up only) and `retire_locked`; `mark_condemned` calls
  `mark_condemned_all`, so the crash point `index.after_condemn_flag` has one site.
* `lunix-gc` collector: the `Condemning` phase flags the whole pass in one call; deleted segments go to
  `Gc::retired` and are flushed by `flush_retired` (end of the delete phase, end of every step in
  `run_units`, 256 queued, the urgent pass). A stop at a crash point flushes nothing (a killed process does no
  more). `abort_pass` first tries to flush the queue (review S1; a failed
  delete step no longer leaves the segments it had unlinked flagged until the next pass), then drops it:
  `reconcile` of the next pass finds what is left (flagged, no file) and retires it in one call as well.
  Un-condemns (review N2) are queued too: one `unmark_condemned_all` swap and one list write per step, flag
  first, list second (a crash between leaves a live segment flagged and listed; the next recheck un-condemns it
  again).
* `footers.rs`: `FooterCache::new(cap_bytes)`; `GcConfig::footer_cache` (count) became `footer_cache_bytes`.
* `lunix-testkit` `MemIndex`: atomic overrides of the two calls plus a call counter for the tests.
* `docs/reference/gc.md`: the delete and condemn text, the crash table, the footer cache line.
* `tools/ci.sh`: the new index kill test runs with the crashpoints feature.

## Crash consistency

The state of a volume at any instant is, per segment, a tuple (file exists, flag, entries dropped, listed).
Recovery is a function of that tuple, segment by segment: `reconcile` (flagged, not listed, no file: retire;
listed, not flagged, file exists: flag again), the verdict `Gone` (listed, no file: retire), `catch_up` at open
(entries of a missing segment: drop) and fsck (`index_extra` is a warning, orphan class). Batching changes
which tuples can exist **at the same time**, not which tuples exist:

* **Condemn.** Order unchanged: the list is written first and is authoritative, then the flags, then the
  graces start. The flags of a pass are one atomic manifest swap, so a crash leaves none or all of the pass's
  flags; the old path could leave a prefix. Both are covered by "a listed segment without a flag is flagged
  again with a fresh grace" (`reconcile`). The graces start only after the swap returned, as before.
* **Delete.** Order unchanged per segment: unlink, directory sync (inside the store call), ledger release,
  *then* the index follow-up, and the list is stored after it at the end of the phase. What changes is that
  the follow-up of up to one step's segments (at most 256) is one swap. A crash after `k` unlinks and before
  that swap leaves `k` segments in the tuple (no file, flagged, entries present, listed): the state the old
  path had for the one object in flight, now for several. Dedup never reaches them (the flag stays until the
  entries are dropped: the rule B1), `catch_up` drops the entries at open, `reconcile`/`Gone` retires them.
  The swap writes `dropped` and clears the flags in **one** manifest, so the retire itself never
  leaves "dropped but still flagged" (that state exists only after `catch_up` dropped a crashed batch and before
  the first pass's `reconcile`, and is harmless), and never "flag cleared with entries reachable" (the invariant "a segment's flag is never
  cleared while entries may still point into it").
* **Flush at step end.** The queue is flushed at the end of every `step`, so nothing is pending while the
  collector is not running. This matters for a clean close: a first version flushed only at the end of the
  delete phase, and one of four RAM cells detached with `index_extra` in `lunix fsck --deep` (a step ended in
  the middle of the phase, the daemon closed). With the flush at step end a clean close leaves none
  (fsck clean in all measurement volumes).
* **Abandoned pass, failed update.** An error in the retire swap leaves the segments flagged and missing; the
  collector forgets the queue and the next pass retires them from the flags (`a_failed_retire_update_...`).
* **Delete-time index invariant** (`segment_deletable`) reads marks computed before the delete phase, not the
  index, so segments whose follow-up is queued do not change another segment's verdict. The randomized
  equivalence test below includes heavily deduplicated volumes.
* **fsck.** `index_extra` after a kill inside a delete phase was already legal after the one object in flight;
  now it can name several segments. `stack_gc_kill` no longer allows it by the name of the kill point: after
  every kill it opens a copy of the killed index and requires every entry into a segment with no file to be
  flagged condemned (and at most 256 such segments); only an image in that state may show `index_extra` or
  `index_stale` (another copy of the id exists). At rest it still allows nothing.

## Rule to test

| rule | test | fails when |
|---|---|---|
| a pass flags its segments in one index update, deletes them in one per step | `lunix-gc` `batch::a_pass_condemns_and_retires_its_segments_in_one_index_update_each` (whole and pipelined passes, call counts on `MemIndex`) | condemn per segment (mutation A); delete per object (B) |
| the delete batch is bounded (256) | `batch::deletes_are_batched_up_to_a_bound` (600 deletes: 1 condemn update, 3 retire updates) | batch of 1 (B) |
| nothing stays queued between steps | `batch::a_step_never_leaves_a_deleted_segment_unrecorded` (unit steps, no flagged segment without a file after any call) | no flush at step end (M) |
| a crash after the k-th unlink of a batch is completed by the next passes | `batch::a_crash_inside_a_delete_batch_is_completed_by_the_next_passes` (k = 1..6, whole and pipelined; asserts `k` flagged-missing at the crash, then roots read, ledger equals the files, convergence, no flag or entry for a missing segment) | batch of 1 (B) |
| a failed retire update loses nothing | `batch::a_failed_retire_update_is_redone_by_the_next_pass` | batch of 1 (B), the queue never flushed (N), no retire in reconcile (I), condemn only the first (G) |
| same objects condemned and deleted at the same points | `equivalence::{whole,pipelined}_passes_16k`, `pipelined_passes_64k` (12 seeded random volumes each: writes, discards, cuts, steps, urgent passes, retention changes, clock jumps; a digest of the objects that exist, the index flags, the segments the index has entries into that have no file, the condemned list and each call's progress after every call; every seed must delete segments; the index entries themselves (dedup, `get`) are pinned by the `lunix-index` proptest below, not by the digest; **the expected digests come from the per-object collector at `1738f2e`**) | the queue never flushed (N), condemn only the first segment (G) |
| the batch is the single-segment calls (`LsmIndex`) | `lunix-index` `batch::batch_updates_leave_the_state_the_single_calls_leave` (proptest, 48 cases, random inserts, persists, batches, reopens; `get`, `get_for_dedup`, flags, `iter_entries` equal) and the contract case `batch_updates_equal_the_single_calls` (every `ChunkIndex` implementation) | a retire that keeps the flags (D) |
| entries and flag go together | `batch::retired_segments_lose_entries_and_flags_together` | D |
| a batch is one manifest swap | `batch_crash::a_batch_is_one_manifest_swap_however_many_segments` (child process, 12 segments, hit counts of `index.before/after_manifest_swap`) | per-segment swaps (J), two swaps (E) |
| a kill at the swap leaves all or none | `batch_crash::a_kill_around_the_condemn_swap_flags_all_or_none`, `..._retire_swap_never_clears_a_flag_over_live_entries` (SIGKILL at `index.before/after_manifest_swap`, `index.after_condemn_flag`; reopen) | J, E |
| no run scan in `drop_segment`/`retire_segments` | `batch::retiring_a_segment_does_not_read_the_runs` (the run file is overwritten in place, any block read fails its CRC) | a scan back in (K) |
| crash point names are unique | `batch_crash::the_index_crash_points_have_unique_names` | two sites with one name (L; the kill loop counted wrongly until this was fixed) |
| footer cache holds bytes, evicts oldest first, serves an oversize footer | `footers::tests::the_cache_is_bounded_by_bytes_and_evicts_the_oldest`; `scale::a_pass_over_four_million_chunks_...` (peak heap) | never evicting (H) |
| an error in a delete step records what the step unlinked | `batch::an_error_in_a_delete_step_still_records_the_segments_it_unlinked` (EIO on the fourth unlink: three segments unlinked, no flagged-missing segment after the failed step) | no flush in `abort_pass` (P) |
| un-condemns are one update | `batch::rechecks_uncondemn_their_segments_in_one_update` (five listed segments get a dedup hit between mark and flags; one `unmark_condemned_all`, no single calls, flags cleared, files kept) | one swap per segment (Q) |
| the killed image's window is safe | `stack_gc_kill::entries_into_missing_segments`: on a copy of every killed image, every index entry into a segment with no file has its flag set and at most 256 such segments exist; only then are `index_extra`/`index_stale` allowed | a flag cleared over live entries |
| kills inside the retire swap | crash points `index.retire.before_swap`/`after_swap` (one site each; reached by `stack_gc_kill`, killed in `batch_crash` and the GC loop) | |
| reconcile retires a gone flagged segment | existing `directed::reconcile_drops_the_entries_of_a_gone_flagged_segment_before_clearing_its_flag`, `review::a_drop_segment_error_after_the_unlink_...` | no retire in reconcile (I) |

Mutations (each applied alone to the final code, the named tests run on the remote host; letters as in the
table): A condemn per segment: 2 failures; B retire batch of 1: 4; N `flush_retired` a no-op: 9 (the three
equivalence tests among them); G condemn only the first segment: 17 (equivalence, oracle, crash sweeps);
I no retire in reconcile: 3; M no flush at step end: 1. D (`retire_locked` keeps the flag): 2; E (retire as two
swaps): 2; H (cache never evicts): 2; J (condemn as per-segment swaps): 2; K (scan back): 1; L (duplicate crash
point name): 1 were run on the first commit; the code they touch did not change afterwards. Removing only the
flush at the end of the delete phase fails nothing now: the flush at the end of the step covers it (in the
tests a step runs to the end of the phase), so it is kept as the one that does not depend on the step budget.

## Gates

Remote host, CPUs 8-15, `-j 8`, on the final code (after the merge of `origin/main` @ `32f686f`, review
follow-up included):

* `tools/ci.sh` (fmt, clippy `-D warnings` on all targets and with `crashpoints`, `cargo test --workspace`,
  the crashpoints suites including `lunix-index --test rebuild_crash --test batch_crash`, the stack tests with
  `lunix-fsck`, ownership `gcbatch ok`): **all green, 2 314 tests passed, 0 failed, 30 ignored** (sum of the
  `test result` lines).
* Kill loops, one at a time, `LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`
  (`cargo test --release -p lunixd --features testsupport,crashpoints`):

| loop | result |
|---|---|
| `stack_gc_kill gc_kill_loop_plain_16k` (the GC-specific kill loop: every `gc.*`, `index.retire.*`, `store.delete.*`, `store.segment_delete.*`, `store.evidence.*` and `index.after_condemn_flag` point) | ok: 23 of 23 points reached, 23 kills, 1 040 s |
| `stack_gc_kill gc_kill_loop_integrity_16k` | ok, 1 040 s |
| `stack_kill kill_loop_plain_16k` (`LUNIX_KILL_CUT_MS=1`) | ok, 1 815 s |
| `stack_kill kill_loop_integrity_16k` (`LUNIX_KILL_CUT_MS=1`) | ok, 2 018 s |
| `stack_gc_kill gc_kill_loop_plain_64k` | ok, 1 035 s |

  Each kill is followed by: fsck (no error, no `index_lost`; `index_extra`/`index_stale` only on an image whose
  entries into missing segments are all flagged and at most 256 segments, checked on a copy of the killed
  index), recovery under `refuse`, the oracle, GC ticked to an empty condemned list, a clean close, and fsck at
  rest (nothing allowed). The first version of the GC loop failed its last assertion ("registered crash points
  the workload no longer reaches: `index.after_condemn_flag` twice"): the batch call had a second site with the
  name of the single call, and the loop counts hits by name. Fixed (one site), covered by
  `the_index_crash_points_have_unique_names`.
* `lunix fsck --deep` after every measurement volume: clean (the measurements of the first review round, run
  on the code before the follow-up; the follow-up changed the abort and un-condemn paths, not the delete
  path measured). The first round of (b) produced one `index_extra` after a clean detach, the finding that led
  to the step-end flush; none after it.
* The 64 KiB `stack_kill` and the encrypted loops were not run.

## Measurement

Same host, same binaries for every cell (release, `1738f2e` "base" against this branch "after"), CPUs 8-15,
alternating base and after, 3 repetitions. No wall-clock assertion anywhere in the tests.

### (a) A backlog of 3 000 dead segments, collector off while it is built

`crates/lunixd/tests/stack_gc_cost.rs` (ignored; by hand, committed so the numbers can be repeated): the stack
(not ublk: no root), 64 KiB chunks, integrity, 3 000 forced cuts of one chunk each (one segment per cut; the
chunk is overwritten eight cuts later), built with `gc_budget = None`; then the volume is opened with the
daemon's collector (`GcConfig::daemon()`, grace 10 s) and the `gc pass done` lines are read. The volume is on
the host's ext4 root file system. 2 992 segments are condemned and deleted, with 2 999 packs, 3 366 index runs
and 3 002 commit objects (12 359 objects, one `delete` unit each). `lunix-fsck` clean after every run.

| tree | rep | condemn ms | delete ms | segs condemned | objects deleted (seg+pack+run+commit) | wall of the passes s | drain s |
|---|---|---|---|---|---|---|---|
| base | 1 | 26797 | 90656 | 2992 | 12359 | 138 | 148.2s |
| base | 2 | 24516 | 86122 | 2992 | 12359 | 130 | 140.8s |
| base | 3 | 24572 | 87178 | 2992 | 12359 | 132 | 141.8s |
| after | 1 | 46 | 40955 | 2992 | 12359 | 53 | 63.6s |
| after | 2 | 27 | 52329 | 2992 | 12359 | 66 | 76.2s |
| after | 3 | 33 | 36060 | 2992 | 12359 | 45 | 55.6s |

Per-object costs (medians; these are the cost constants for the simulator):

| constant | before | after |
|---|---|---|
| `c_condemn` per segment | 8.2 ms (24.6 s over 2 992) | 0.011 ms (33 ms over 2 992: the list write, one swap, the units) |
| delete, every kind of object (unlink and directory sync, one unit) | 7.05 ms mean over the 12 359 objects (87.2 s) | 3.31 ms mean (41.0 s) |
| of which unlink + directory sync of one object | about 2.8 ms (derived: the 9 367 packs, runs and commits, with the first version's 34.2 s for all 12 359) | 2.8 ms (unchanged: one syscall pair per object) |
| `c_seg_index` per deleted segment (drop + unflag: two swaps and the run scan) | about 17.7 ms (derived) | about 0.6 to 2.3 ms amortised (one swap per step, about 4.3 segments and 17 objects per step) |
| `c_swap` | the 8.2 ms per condemned segment above (three syncs) | the same swap, once per pass for the flags and once per step for the delete follow-up |

Reading: the per-segment index work is gone as a cost class (condemn 24.6 s to 33 ms; the delete phase 87 s to
41 s, drain of the whole backlog 141 s to 56 to 76 s). **What is left is the unlink and directory sync of every
object, about 2.8 ms, 12 359 of them: 34 s of the 41 s.** The lead's expectation of "well under a second" for
3 000 objects holds for the condemn side only; the delete side is bound by the directory syncs of the packs,
runs and commits, which a manifest change cannot touch. A batch unlink with one directory sync needs a store
call that takes a list (`SegmentStore::delete`, `PackStore::delete_pack`, `delete_run`, `CommitStore::delete`
each sync the directory themselves): a separate candidate, not done here.

An earlier version of the branch kept the queue until the end of the delete phase, not the step. Its delete
phase took 34.2 s, 34.2 s and 53.5 s (three reps, the third on a busier host), against 41.0 s, 52.3 s and 36.1 s
for the final code: within the noise of the host, and why the step-end flush is kept.

### (b) One RAM-backed 64 KiB w4k overwrite cell

`tests/perf` of `p1/m16` (T6 setup, not part of this branch: a trimmed copy of `partA.sh`, in a scratch
tree), through `tools/remote.sh root`: a dedicated tmpfs (`LUNIX_ALLOW_TMPFS_FOR_TESTS=1`), a 16 GiB integrity
volume with 64 KiB chunks, four queues, a 2 GiB working set written once, then three `w4k` cells in a row on
the same volume (4 KiB random writes, QD 128, 8 jobs, 60 s plus 5 s ramp); daemon on CPUs 8-12 (queues 8-11),
fio on 13-15; two rounds per tree (base, after, base, after), so six cells each. `lunix fsck --deep` clean on
all four volumes. The host was not quiet (the harness's check warned: other agents were running on CPUs 0-7),
so IOPS and stop counts are not comparable with the T6 report; the base and after cells are interleaved.
The columns are summed over the `gc pass done` lines of the cell's window.

| tree | round.rep | passes | mark ms/pass | condemn ms (segs) | delete ms (segs deleted, units) | GC held ms (% of window) | gate stops | throttled s | IOPS |
|---|---|---|---|---|---|---|---|---|---|
| base | 1.1 | 9 | 70 | 8 (848) | 1220 (425, 3316) | 2857 (4.3 %) | 1 | 31 | 12806 |
| base | 1.2 | 11 | 86 | 32 (1143) | 2911 (1045, 4911) | 5739 (8.9 %) | 2 | 48 | 12490 |
| base | 1.3 | 10 | 104 | 50 (1111) | 2975 (1087, 5986) | 6318 (9.6 %) | 1 | 59 | 12696 |
| base | 2.1 | 9 | 47 | 8 (843) | 0 (31, 1760) | 1102 (1.7 %) | 3 | 35 | 11519 |
| base | 2.2 | 11 | 81 | 21 (906) | 2628 (930, 4461) | 5294 (8.1 %) | 4 | 48 | 11607 |
| base | 2.3 | 12 | 62 | 33 (1025) | 2856 (1109, 3769) | 5199 (7.7 %) | 3 | 45 | 12704 |
| after | 1.1 | 9 | 56 | 0 (723) | 334 (420, 2365) | 2072 (3.2 %) | 1 | 30 | 11286 |
| after | 1.2 | 12 | 64 | 0 (1117) | 662 (802, 3881) | 4142 (6.3 %) | 4 | 42 | 11732 |
| after | 1.3 | 10 | 73 | 0 (975) | 591 (807, 4535) | 4282 (6.5 %) | 4 | 46 | 10891 |
| after | 2.1 | 9 | 50 | 0 (729) | 132 (198, 2364) | 1739 (2.6 %) | 1 | 36 | 11996 |
| after | 2.2 | 12 | 62 | 0 (1059) | 786 (1011, 4070) | 4026 (6.1 %) | 3 | 39 | 12385 |
| after | 2.3 | 11 | 76 | 0 (1117) | 713 (935, 5062) | 4575 (7.0 %) | 2 | 50 | 10625 |

Medians over the six cells:

| | base | after |
|---|---|---|
| mark time per pass | 75 ms | 63 ms |
| delete phase per segment deleted | 2.79 ms | 0.77 ms |
| condemn phase per segment condemned | 0.026 ms | below 1 ms per pass (rounds to 0) |
| coordinator time held by GC (sum of the phase times of the passes, share of the window) | 7.9 % | 6.2 % |
| gate stops per cell | 2.5 | 2.5 |
| write IOPS (inconclusive, see below) | 12 593 | 11 509 |

On tmpfs a manifest swap costs no syncs, so the saving is the CPU of the swaps and the run scan (the scan
grows with the index): 2.0 ms per deleted segment, and the held time falls by about a fifth. The
stop counts differ from cell to cell with the compaction share and the amplification (3 to 12). **The IOPS
comparison is inconclusive**: the after cell was faster in only 6 of 36 base/after pairs (median 9 % lower, not
significant, not nothing) on a loaded host, and there is no mechanism on tmpfs (the GC held time fell). It must
not be used as a baseline; the quiet-host A/B will re-measure.

**Footer cache.** The mark time per pass is 15 % lower in the median (base 47 to 104 ms, after 50 to 76 ms
over six cells each), which is the direction the note predicted (the leaf walk resolves entries scattered over
more segments than 32), but the spread of the base cells is as large as the difference and nothing counted the
footer loads, so this is **not** a measured effect of the cache alone. The default is 32 MiB, not the note's
64 MiB: `scale::a_pass_over_four_million_chunks_...` bounds the peak GC heap beyond the live set at 64 MiB
and a 64 MiB cache made it 66 MiB. 32 MiB holds the footers of about 3 000 segments of 64 KiB chunks or 200 of
4 KiB chunks.

## Deviations and open points

* **The delete batch is one step, not one pass.** See the clean-close finding above. The condemn side is one
  swap per pass. The batch is bounded by 256 segments as well.
* **Unlink and directory sync per object remain** (about 2.8 ms each): the largest cost left in the delete
  phase (see (a)). Not an index matter.
* **Measurement (a) is a stack test on the host's ext4, not a ublk root cell**: the collector, the store and
  the file system calls are the same; no device, no fio. The object mix is 3 000 one-chunk cuts, not the
  gcrate cell's.
* **Measurement (b)** is the T6 cell with the daemon, queues and fio on 8 of the 16 CPUs and on a host with
  other load; absolute rates differ from T6's.
* **`ChunkIndex` gained two methods** with default bodies (a default that loops the single calls is correct
  for any implementation; `LsmIndex` and `MemIndex` override). Wrappers in `lunix-engine` and `lunix-gc` tests
  forward them.
* **`stack_gc_kill` allows `index_extra` after more kill points** (above). Nothing is allowed at rest.
* **Un-condemn is batched** (review N2) with the flags cleared before the list is stored, both at the end of the
  step; `unmark_condemned_all` is a third `ChunkIndex` method with a default body. A failed update leaves the
  segment flagged and unlisted, which `reconcile` adopts with a fresh grace and the next recheck un-condemns
  again.
* **`catch_up` still scans the runs** to fill `CatchUpReport::dropped` (once per open; the report and its
  contract test use it).
* **Footer cache default 32 MiB** instead of 64 MiB (above); `GcConfig::footer_cache` is renamed
  `footer_cache_bytes` (no compatibility alias, the project is WIP).
* The measurement scripts of (b) are not in the repository (they come from `p1/m16`); (a) is
  `stack_gc_cost.rs`.
* Remote: the target directories, the binaries and the tmpfs were removed at the end.
