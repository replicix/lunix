# Review of M17.A (`lunix-gc` against the testkit fakes)

- Reviewed: branch `p1/m17a` at `30638a9` (base `59251e6`), `crates/lunix-gc/**`, `docs/reference/gc.md`,
  `docs/plans/v1/reports/p1-m17a.md`, against the M17 section of the phase 1 plan, D1-D20, the `lunix-iface`
  rustdoc, the engine's cut and recovery code (`lunix-engine/src/{coord,recover}.rs`), `lunix-index`,
  `lunix-map`, `lunix-destage` and the testkit fakes.
- Reviewer: independent, 2026-11-14. Nothing in the worktree was changed.

## Verdict

**Merge after fixes.** One blocker (a delete path that lets dedup target a deleted segment after a single
transient index error, reproduced), five majors (M1 and M2 need a lead decision, M3 and M4 can land before
the M17.B gate if the lead prefers, M5 is three ready-made tests). The core design is sound: I could not make the collector delete
an object that a retained or pinned root, a durable commit, the rollback-floor window, a pending re-hint or
an unnamed compaction output still needs through any crash or interleaving of cuts; the hole is in an
**error path without a crash**, which neither the oracle nor the crash sweep exercises.

## What I checked and found sound

- **Condemn → grace → recheck → delete.** Candidates come only from the listing taken when the mark started;
  the index flag is set (durably) before a grace starts; the recheck is a new mark whose start is after
  `grace start + grace` (`collector.rs:836-838`); every delete is re-judged against the view of its own step
  (`collector.rs:877-890`). GC runs on the coordinator between cuts and destage is synchronous inside a cut,
  so no dedup lookup is in flight across a step. Rule 12 (i)-(iv) hold as argued.
- **No new reference to a condemned pack.** Packs have no flag, so I checked the map: `seal_leaf` reuses a
  location only when the node at the same position of the sealed tree has the same hash
  (`lunix-map/src/map.rs:529`); there is no cross-root reuse by hash, so a new root references only packs its
  parent references or new ones. The full `walk_nodes` per root (M5 note) is implemented (`mark.rs:281`).
- **Crash and restart of the delete-time state.** `named_high`, `outputs` and the outbox are memory only.
  After a restart `named_high` starts at the tip's high-water mark and `outputs` is empty; that is sound
  because after recovery every index entry comes from a persisted run or from `catch_up` over chain-named
  segments, and an unnamed output is either swept (above the mark) or has no index entry. Losing the list is
  safe (flags are adopted with a fresh grace; a listed segment without a flag is re-flagged with a fresh
  grace before any recheck: `reconcile` always runs before a mark).
- **Commit objects.** Kept at or above the durable tip's `chain_floor`, when pinned, and at or above the
  rollback floor's `max_commit_seq`; an unreadable floor keeps every commit (`collector.rs:180-192`), read
  again for every delete; `CommitStore::delete` gets the durable tip's `chain_floor`. Deletion is ascending,
  so a crash leaves a contiguous suffix.
- **N11.** The attach walk (`recover.rs:112-147`) is `verify_chain(tip, floor)` plus the hash of the floor
  commit: links only, no map root, no named object. Keeping the commit objects without their data is enough
  for it. fsck (M14) must not check the objects named by commits below the tip's `chain_floor`; say so in
  the M14 section (see recommendation under N11).
- **Compaction.** Re-seal through `ExtentCodec::seal` with the id from the authenticated footer; `relocate`
  only after `writer.seal` returned; a move is emitted only when `relocate` reports the old copy was current;
  moves go to the outbox one by one and survive a failed step; the output is in `outputs` from the seal on.
- **Lean volumes.** Hint-only liveness, compaction refused, no `deps`.

## Blocker

### B1. A transient `drop_segment` error after the unlink lets dedup target the deleted segment

`collector.rs:896-903` (`forget`), `collector.rs:909-914` (`delete`), `collector.rs:517` (`reconcile`).

`delete` unlinks the segment, then calls `index.drop_segment`, then clears the flag. If `drop_segment` fails
(the real index reads runs and swaps its manifest there, `lunix-index/src/lib.rs:519-549`: an I/O error or
`NoSpace`, which is likely exactly when GC is deleting), the step fails and the pass is abandoned with the
file gone, the index entries still present and the segment still listed and flagged. The next pass finds the
listed segment missing from its listing (`Verdict::Gone`) and `forget` calls **only** `unmark_condemned`.
From then on `get_for_dedup` returns the stale entries again. Entries inserted in this process are
`Confirm::Verified`, which destage trusts without reading (`lunix-destage/src/worker.rs:272-279`), so the
next write of that content is mapped onto a segment that no longer exists and is acknowledged: silent loss
of an acknowledged write, no crash involved.

Reproduced in a scratch copy with one injected `Fault::Io` at `index.drop.before` (test
`rev_drop_segment_error_then_dedup_targets_a_deleted_segment`): after the failed step and later passes the
list is empty, `get_for_dedup` returns `DedupHit { seg 1, confirm: Verified }` for the dead id, the write
lands in segment 1, which does not exist, and `check_roots` reports `root 11 chunk 5: unreadable`.

Fix (verified: with the first line the reproduction passes):

1. `forget` for a segment: `drop_segment(s)` before `unmark_condemned(s)` (idempotent in both indexes); the
   same in `reconcile` for a flagged segment that does not exist.
2. Better still, keep the flag until the drop succeeded as an invariant: never call `unmark_condemned` for a
   segment that is not in the store unless `drop_segment` returned `Ok` in the same call.
3. Tests: the directed case above, and **non-crash fault injection in the oracle** (one-shot `Io`/`NoSpace`
   at every `MemIndex`/`MemStore` point during passes, then the usual read-back). Today a failed call that is
   not a crash is a test panic (`crash.rs:73`), so this whole class is untested.

## Majors

### M1. Commit deletion breaks `fsck --repair --truncate-tip` (lead decision)

`collector.rs:180-192`. With `retain_last = 2` and tip `T`, GC deletes every commit object below
`T.chain_floor = T-1` once the floor is at `T`. After `--truncate-tip` the tip is `T-1`, whose own
`chain_floor` is `T-2`; mount runs `verify_chain(T-1, T-2)` and then opens every commit from `T-2`
(`recover.rs:190-225`), so it fails with a missing link. Reproduced
(`rev_truncate_tip_needs_the_predecessors_chain_floor_commit`): at quiescence the commits present are
`[19, 20]`, the predecessor's `chain_floor` is 18, `verify_chain(19, 18)` is `NotFound`. Plan rule 1 gives
"so an offline `fsck --repair --truncate-tip` can drop a damaged tip" as the reason for `last:2`, and M14's
gate mounts the truncated volume, so the two milestones contradict each other once GC has run.

Options: (a) GC keeps commit **objects** at or above the lowest `chain_floor` recorded by any retained FULL
(one more object with the defaults; a two-line change in `commit_kept`, plus the oracle's
`expected_commits`); the truncated volume then mounts, but its older retained root (`T-2`) has lost data, so
the engine must not treat it as a GC root or fsck error until the next commit rolls it off (today the mark
would fail closed on the missing pack until then); or (b) GC roots are the FULLs from that lower floor up
(one more root, costs one commit interval of garbage, and `--truncate-tip` gives a fully consistent
volume). I recommend (a) now and a plan note for M14/M12b; (b) if the maintainer wants a clean fsck after a
truncation. Record it as a spec gap (N12).

### M2. "Named once by a FULL" is not "durably indexed" (latent until pins exist; engine side)

`collector.rs:285-292`, decision 3. The delete-time invariant treats a copy as durable when its segment is
at or below `named_high` and not an unnamed output. After the naming FULL has left the retained window,
`catch_up` no longer covers that segment (`recover.rs:265-267` passes only chain-named segments), so the
relocation must be in a persisted run. The engine does not guarantee that: a failed `persist` is a warning
(`coord.rs:557-563`), and entries that `catch_up` inserted wait for a cut that wrote segments. Sequence:
compaction `S → X`; the naming cut crashes between C6 and C8; restart (`catch_up` restores X in memory);
two commits without new segments (discards or dedup hits only), so no `persist` and the naming FULL rolls
off; GC deletes `S` (`X <= named_high`); crash. X's entries are gone from the index, and every root that
still has stale hints into `S` cannot be read. Only a **pinned** root can hold such hints after the naming
FULL rolled off (retention is a suffix), so phase 1 cannot reach it, but the oracle already drives pins and
snapshots will. The same family: when `apply_moved` fails, the engine continues the cut without naming the
output (`coord.rs:358-360`), then `persist(upto)` can write relocations of an unnamed segment (breaking the
`persist` invariant of the iface) and destage can dedup onto the unnamed output; it is safe today only
because the map pack sealed afterwards has a higher id than the output.

Recommendation (no change needed in this branch, record as spec gap N13 and M12 input): the engine names
compaction outputs in the next FULL whether or not the re-hints applied (naming an existing sealed segment
is always safe), persists index entries whenever the memtable holds entries of named segments, and treats a
failed `persist` as "retry before the naming FULL rolls off"; then decision 3's rule is exact. Until then,
state in `gc.md` that the invariant relies on it.

### M3. Step budgets do not hold beyond the gate's size

Plan rule 2 ("never stalls a cut for more than its budget"). `mark.rs:291-296`: a root is walked in at most
64 leaf batches, so a unit is 1/64 of the new leaves of a root and grows with the volume. With the scale
test's own cost model the 1 M-chunk case already reaches 57 ms; a 64 M-chunk volume (1 TiB at 16 KiB) gives
units of seconds on the coordinator thread. Also unbudgeted single units: the full `walk_nodes` of a root
(`mark.rs:281`), `finish` (the sort of the whole live vector, `mark.rs:442`), `recheck`
(`collector.rs:827-847`, with `object_bytes` a linear `find` per listed object: quadratic), `Judge::dead`,
`reconcile`, `observe_named`. `urgent_inner` clones `todo` once per element (`collector.rs:1185-1188`,
quadratic) on the path that runs when the disk is full. `Judge::new` runs for every deleted object
(`collector.rs:877`) and re-reads `state/pins`, the manifest and the named set each time.

Fix: bound a leaf batch by a leaf count, not by a fraction; make the node walk and the recheck resumable;
binary search or maps for `object_bytes`; build the `Judge` once per step. Acceptable as a follow-up before
the M17.B gate, which should add a case at 64 M chunks. Note also the live set itself: 24 bytes per live
extent is about 1.5 GiB per TiB at 16 KiB chunks (the plan allows it, M17.B should report it).

### M4. An error (not a crash) drops the evidence of deletes already made, and the urgent pass reports failure after freeing

`collector.rs:588-595` (`abort_pass` drops `ctx`), `collector.rs:1215-1218`, `collector.rs:1268-1275`.
Rule 11 allows losing a pass's evidence to a crash. Here any failed step or urgent pass discards the
evidence lines of objects it already deleted, and `gc/` is the only record of what the one deleting
component did. Two paths make it routine rather than rare: (1) the urgent pass compacts when still short,
compaction needs headroom, and `NoSpace` there returns `Err` from `urgent`: the ladder is told the pass
failed, although it freed bytes, and no evidence object is written; (2) one extent of a low-live segment
that fails `ExtentCodec::open` fails the compaction of every later pass (the segment stays the best
candidate), so every pass from then on ends in an error and writes no evidence.

Fix: write the evidence object best effort in `abort_pass`; in `urgent`, treat a compaction error as the end
of compaction (finish the pass, return the bytes freed); skip a source whose extent does not open (log, an
evidence line, keep the segment out of the candidates for the process lifetime).

### M5. Three safety guards are correct but no test fails without them

Mutation runs (below) show that the shipped suite, oracle included, stays green when each of these is
removed, and a directed test (appendix) then shows real data loss, so each guard is load-bearing:

- **Re-flagging a listed segment that has no index flag** (decision 6, `collector.rs:489-492`). Without it,
  after a crash at `gc.condemn.after_list_write` a dedup hit taken after the recheck mark lands on a segment
  that is then deleted (`rev_listed_but_unflagged_after_a_crash_is_reflagged`: `root 6 chunk 5: unreadable`
  with the mutation, green without).
- **The `outputs` half of the delete-time invariant** (`collector.rs:287`). Without it, a source is deleted
  while its output is unnamed and below the mark (a cut without new segments lifted the mark while the moves
  were pending); a crash then leaves the tip pointing into the deleted source
  (`rev_source_outlives_an_unnamed_output_below_the_mark_across_a_crash`: `root 4 chunk 0: unreadable` with
  the mutation). The shipped lagging-wiring scenario checks only that the output survives.
- **Confirming the index's copy against its footer** (`mark.rs:400`). Trusting an unconfirmed entry keeps
  the suite green because the fake index never holds a wrong or dangling entry. A real index can (a crash
  between the unlink and `drop_segment`, a run that is only checksummed). Needs a directed test with an index
  entry that points at a missing segment, and one that points at another chunk, while a root hints an
  older copy.

Also untested, lower risk: the grace itself on the normal path (removing both the `Grace` wait and the
`elapsed` check fails only two unit tests on incidental state assertions; add "nothing is deleted before
`gc_grace` has run since the flag"), and the re-judge at delete time (redundant by rule 12 for segments and
packs; for commits add a floor source that starts failing after the recheck).

Fix: add the two tests of the appendix, the confirm test, and the non-crash fault sweep of B1.

## Minors

1. **`deps` keeps only the highest dependency** (`mark.rs:362-367`, `collector.rs:287`). The `outputs` check
   looks at that one segment; a lower dependency that is still an unnamed output is not seen (possible when
   re-hints stay pending while a destage segment with a higher id is named). Keep a small set per segment,
   or a flag "some dependency is in `outputs`" recomputed at delete time.
2. **An output with no successful relocation stays in `outputs` for the process lifetime** when `relocate`
   or the ledger `transfer` fails before the first move (`compact.rs:275-290`): never condemned, and every
   source that depends on it is deferred forever. On a `transfer` error the reservation taken with
   `mem::take` is also never released. Remove the output from `outputs` when the seal step fails with no
   move emitted, and release the reservation.
3. **Live extents without an index entry are copied every pass** (`compact.rs:207-254`): `relocate` returns
   `false`, no move, the source stays low-live and is picked again (up to 1 GiB rewritten per pass, the
   outputs become orphans). Check `index.get(id)` equals `old` before copying, and do not count such a
   segment as a candidate. The same loop exists for a segment kept alive by an unresolvable entry
   (`mark.rs:432-435`).
4. **GC advances only at C10** (`coord.rs:587`), so on an idle volume a pass waiting in `Grace` never
   finishes and condemned objects stay. The plan's M12 inputs already ask for a step from the idle tick;
   this collector depends on it.
5. **`gc/` grows without bound**: about one object per pass that did anything, that is one every 90 s on a
   volume that commits; `evidence_passes()` lists the directory at the end of every pass. Approve request 3
   and schedule the pruning before the phase 1 exit; consider one object per N passes.
6. **`Verdict` by string** (`collector.rs:273`): `Some("rollback floor unreadable")` is matched as a string
   literal; a reworded reason silently turns "defer" into "un-condemn". Use an enum for the keep reason.
7. **The collector must live as long as the engine**: `outputs` and the outbox are memory only, so a wiring
   that rebuilds `Gc` while moves are pending reopens N10. Say so in `lib.rs` and `gc.md`.
8. **Uncondemn is not durable until the end of the delete phase** (`collector.rs:860-867`, list stored at
   `729`): a crash in between re-flags a live segment at the next load (safe, dedup is lost for one grace).
   Storing the list after an un-condemn closes it; at least add it to the crash table in `gc.md`.
9. **`CondemnedList` beyond `MAX_ENTRIES`** makes every `store_list` fail and with it every pass; cap the
   condemn batch.

## Nits

- The report names host details the rules exclude (paths under the remote home, the CPU count and pinning).
- `Action::Delete` is documented as "deleted (or found already gone)", but the `Gone` path writes no line.
- `after_mark` takes an unused `view` (`let _ = view`); `GcState` has no finishing state (mapped to
  `Compacting`).
- `FloorSource`'s blanket implementation does not let `Arc<dyn VolumeDir>` become `Arc<dyn FloorSource>`;
  the wiring will write a wrapper, so "there is no no-floor implementation" is a convention, not a
  guarantee. Fine for now; `GcView::rollback_floor` (request 3) is the clean form.
- rust-skills: narrowing and float `as` casts in `compact.rs` and `collector.rs:1049` (`num-cast-try-from`);
  the immediately invoked closure in `step` reads better as a method.

## Decisions to confirm

| # | decision | recommendation |
|---|---|---|
| 1 | floor through the constructor | Confirm. Add `GcView::rollback_floor` when the iface next opens. |
| 2 | unnamed outputs tracked by the collector | Confirm; it is required (N10). Pair it with the engine change in M2 (name outputs unconditionally). |
| 3 | delete-time invariant enforced with `named_high` | Confirm for phase 1, with M2 recorded: the rule is exact only if the engine persists relocations before the naming FULL rolls off. Fix minor 1. |
| 4 | moves never lost | Confirm. |
| 5 | grace on the monotonic clock, restarted at load | Confirm (a restart can only lengthen a grace). |
| 6 | flags and list reconciled at pass start and load | Confirm, with the B1 fix (never clear a flag of a missing segment without a successful drop). |
| 7 | no evidence for an empty pass, 48 MiB cap | Confirm; add M4 (evidence on abort). |
| 8 | when a pass is due | Confirm; M12 must also step on idle (minor 4). |
| 9 | reader pin deadlines ignored | Confirm for phase 1; must be revisited with the first reader pin. |
| 10 | `clen` sums, `u = live / size`, age by id | Confirm. |
| 11 | `bytes_moved` definition | Confirm. |
| 12 | crash sweep in-process | Confirm for M17.A; the kill loop is M17.B's. Add the non-crash fault sweep (B1). |
| 13 | salt test with a test codec | Confirm; the real codec run is M17.B's. |

## Requests to the lead

1. **`true_up_index` double release**: confirmed by reading `coord.rs:611-624`: the engine releases
   `index_charged - total` after GC already released the run's bytes. Fix in the engine: true up against the
   ledger's own `Index` value instead of a private counter (the ledger is the single source). Do not make GC
   skip the release: the walk-through says GC releases what it deletes.
2. **Wiring**: approve all three. Handing the urgent pass's moves to the next cut is required, not optional:
   without it the outputs are never named and their sources are never deleted.
3. **Iface additions**: approve both; `delete_evidence` should not wait long (minor 5).
4. **`ci.sh` crashpoints line for `lunix-gc`**: approve.

## Spec gaps

- **N10**: accept the proposed text for rule 7. Add the engine half: "a compaction output is named by the
  next FULL after its seal, whether or not its re-hints were applied".
- **N11**: accept. Add to M14: fsck authenticates and links commits below the tip's `chain_floor` and does
  not check the objects they name; it reports the window as information.
- **N12 (new, M1)**: `--truncate-tip` needs the predecessor's `chain_floor` commit (and, for a clean fsck,
  its data).
- **N13 (new, M2)**: the delete-time index invariant needs "index entries of a named segment are persisted
  before the naming FULL leaves the retained window".

## Test quality

- The primary safety check is independent of the collector: every block of every root is read through the
  hint or the index fallback and compared with the model content. Good. The "reachable set" at quiescence
  and the liveness recount re-implement the collector's own liveness rule (index's current copy, else the
  hint), so they prove the code matches the rule, not that the rule is right; the read-back covers that.
- Gaps: no non-crash failures (B1); the sim always persists the index at every commit and always applies the
  moves at the next commit, both more favourable than the engine (`coord.rs:557`, `358`); no truncation
  case (M1); the crash sweep reopens with `catch_up` only over the retained chain, but never with a failed
  `persist` (M2).
- Crash points: the nine of rule 10 are there and each is hit; the windows between them that matter
  (unlink done, index not dropped; list written, flags missing; relocate done, moves not applied) are
  covered. Not covered by a point: flag cleared, list not yet stored (minor 8).
- Mutations I ran are listed below; M5 lists the three that matter.

## What I ran

All on the shared build host through `tools/remote.sh` from a scratch copy (deleted afterwards), CPUs 12-15,
`RUST_TEST_THREADS=1`, no root.

Baseline: `cargo test -p lunix-gc --lib`, 41 passed (235 s), as reported.

Reproductions (tests added only in the scratch copy; code in the appendix):

| test | result on `30638a9` |
|---|---|
| `rev_drop_segment_error_then_dedup_targets_a_deleted_segment` (B1) | fails: write mapped onto deleted segment 1, `root 11 chunk 5: unreadable`; passes with `drop_segment` added to `forget` |
| `rev_truncate_tip_needs_the_predecessors_chain_floor_commit` (M1) | fails: commits `[19, 20]`, `verify_chain(19, 18)` is `NotFound` |
| `rev_listed_but_unflagged_after_a_crash_is_reflagged` (M5) | passes; fails with the re-flag removed |
| `rev_source_outlives_an_unnamed_output_below_the_mark_across_a_crash` (M5) | passes; fails with the `outputs` check removed |

Mutations (one at a time, full `--lib` run unless noted):

| mutation | failing tests |
|---|---|
| recheck ignores `elapsed` only (`collector.rs:839`) | 0 (redundant with the `Grace` phase) |
| no grace at all (also the `Grace` wait) | 2 (`units::a_lost_condemned_list_*`, `units::status_and_stats_*`), no oracle |
| "named by a retained commit" ignored (rule 12 ii) | 5 (four oracles, `rule12_ii_*`) |
| pinned commit objects not kept | 8 (five oracles, three directed) |
| index copy not confirmed against the footer | **0** |
| listed segment without a flag not re-flagged | **0** |
| urgent pass with zero grace | 1 (`units::the_urgent_pass_deletes_only_after_the_floor_grace_*`; units and scenarios only, the full run lost its connection after the oracles had passed) |
| no re-judge at delete time | 0 (redundant by design) |
| `outputs` check of the delete-time invariant removed | **0** |
| stale-hint candidates not marked live | 5 (four oracles, the liveness recount) |

Not re-run: the implementer's rollback-floor and high-water-mark mutations (reported as 6 and 1 failures).
One run was lost to a dropped connection and repeated. The scratch copies and the review directory on the
host are deleted.

## Appendix: the directed tests

They use the crate's own `tests::sim` and go in `crates/lunix-gc/src/tests/`.

```rust
//! Reviewer proofs of concept (scratch copy only).
use std::collections::BTreeSet;
use std::time::Duration;

use lunix_format::{ChunkId, CommitSeq, ExtentKind, IdLen, SegId};
use lunix_iface::crypto::ExtentCodec;
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{CommitStore, SegmentStore};

use super::sim::{data_of, Sim, SimCfg, GRACE};
use crate::condemned::ObjectRef;

const CS: u32 = 16 << 10;

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store).unwrap().iter().map(|x| x.seg).collect()
}
fn id_of(seed: u64) -> ChunkId {
    lunix_testkit::PlainCodec::new().chunk_id(ExtentKind::Data, &data_of(seed, CS))
}
fn hint_seg(s: &Sim, ci: u64) -> SegId {
    let e = s.map.get(ChunkNo(ci)).unwrap().expect("populated");
    lunix_iface::map::entry_hint(&e).seg
}

/// A transient error of `drop_segment` after the unlink, then later passes:
/// the list entry is "gone", `forget` clears the flag without dropping the
/// entries, and dedup returns a verified hit into the deleted segment.
#[test]
fn rev_drop_segment_error_then_dedup_targets_a_deleted_segment() {
    let mut s = Sim::new(SimCfg::new(CS, IdLen::Sixteen));
    s.write(0, 77);
    s.commit().unwrap();
    let x_seg = hint_seg(&s, 0);
    s.write(0, 78);
    s.commit().unwrap();
    s.commit().unwrap();
    s.commit().unwrap();
    s.clock.advance(Duration::from_secs(5));
    s.index
        .faults()
        .arm("index.drop.before", 1, lunix_testkit::Fault::Io);
    let mut failed = false;
    for _ in 0..200 {
        match s.step() {
            Ok(p) => {
                if p.done {
                    break;
                }
            }
            Err(e) => {
                eprintln!("rev: step failed as injected: {e}");
                failed = true;
                break;
            }
        }
        s.clock.advance(Duration::from_secs(10));
    }
    assert!(failed, "the injected drop_segment error fired");
    assert!(!segs(&s).contains(&x_seg), "the segment file is gone");
    eprintln!("rev: condemned after failure: {:?}", s.gc.condemned());
    // Later passes (no crash, same process).
    for _ in 0..6 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
    }
    eprintln!("rev: condemned later: {:?}", s.gc.condemned());
    let hit = s.index.get_for_dedup(&id_of(77)).unwrap();
    eprintln!("rev: dedup hit for the dead id: {hit:?}");
    // A write of the same content now.
    s.write(5, 77);
    s.commit().unwrap();
    let target = hint_seg(&s, 5);
    eprintln!("rev: write landed in {target:?}, exists {}", segs(&s).contains(&target));
    let r = s.check_roots();
    eprintln!("rev: check_roots: {r:?}");
    assert!(segs(&s).contains(&target), "dedup targeted a deleted segment");
    r.unwrap();
    let _ = ObjectRef::Segment(x_seg);
}

/// `fsck --repair --truncate-tip`: the predecessor becomes the tip and mount
/// verifies the chain down to ITS chain_floor. Is that commit still there?
#[test]
fn rev_truncate_tip_needs_the_predecessors_chain_floor_commit() {
    let mut s = Sim::new(SimCfg::new(CS, IdLen::Sixteen));
    for i in 0..12u64 {
        s.write(i % 8, 100 + i);
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    s.quiesce().unwrap();
    let tip = s.tip.common.commit_seq.0;
    let pred = match &s.retained[s.retained.len() - 2] {
        lunix_format::commit::Commit::Full(f) => f.clone(),
        _ => unreachable!(),
    };
    assert_eq!(pred.common.commit_seq.0, tip - 1);
    let have: Vec<u64> = CommitStore::list(&s.store).unwrap().iter().map(|c| c.seq.0).collect();
    eprintln!(
        "rev: tip {tip}, tip.chain_floor {}, predecessor.chain_floor {}, commits present {have:?}",
        s.tip.chain_floor.0, pred.chain_floor.0
    );
    let r = lunix_iface::crypto::CommitCodec::verify_chain(
        &*s.commit_codec,
        &s.store,
        CommitSeq(tip - 1),
        pred.chain_floor,
        &[0x11; 32],
    );
    eprintln!("rev: verify_chain(predecessor, its chain_floor): {:?}", r.as_ref().map(|_| ()));
    assert!(r.is_ok(), "after truncate-tip the mount's chain walk fails");
}

/// Decision 6: a crash between the list write and the index flags. After the
/// reopen the listed segment must be flagged again before its recheck, or a
/// dedup hit after the recheck mark lands on a segment that is then deleted.
#[test]
fn rev_listed_but_unflagged_after_a_crash_is_reflagged() {
    use lunix_iface::gc::GcState;
    let mut s = Sim::new(SimCfg::new(CS, IdLen::Sixteen));
    s.write(0, 77);
    s.commit().unwrap();
    let x_seg = hint_seg(&s, 0);
    s.write(0, 78);
    s.commit().unwrap();
    s.commit().unwrap();
    s.commit().unwrap();
    s.clock.advance(Duration::from_secs(5));
    s.hooks.arm("gc.condemn.after_list_write", 1);
    let mut crashed = false;
    for _ in 0..200 {
        if s.step().is_err() {
            crashed = true;
            break;
        }
        s.clock.advance(Duration::from_secs(1));
    }
    assert!(crashed && s.hooks.crashed().is_some());
    s.crash_reopen(1).unwrap();
    assert!(segs(&s).contains(&x_seg));
    s.commit().unwrap();
    let mut reached = false;
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.state == GcState::Deleting {
            reached = true;
            break;
        }
        if p.done {
            break;
        }
        if p.state == GcState::Grace {
            s.clock.advance(GRACE * 2);
        } else {
            s.clock.advance(Duration::from_millis(100));
        }
    }
    eprintln!("rev: reached Deleting: {reached}; condemned {:?}", s.gc.condemned());
    eprintln!("rev: dedup hit: {:?}", s.index.get_for_dedup(&id_of(77)).unwrap());
    s.write(5, 77);
    s.commit().unwrap();
    eprintln!("rev: write landed in {:?}", hint_seg(&s, 5));
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    s.clock.advance(GRACE * 2);
    s.commit().unwrap();
    s.pass().unwrap();
    let r = s.check_roots();
    eprintln!("rev: check_roots {r:?}");
    r.unwrap();
}

/// The `outputs` half of the delete-time invariant: an unnamed output that a
/// cut lifted below the high-water mark (moves still pending), then a crash.
#[test]
fn rev_source_outlives_an_unnamed_output_below_the_mark_across_a_crash() {
    let mut s = Sim::new(SimCfg::new(CS, IdLen::Sixteen));
    for i in 0..4 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    let src = hint_seg(&s, 0);
    for i in 1..4 {
        s.write(i, 300 + i);
    }
    s.commit().unwrap();
    s.commit().unwrap();
    s.commit().unwrap();
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert!(!s.moved.is_empty(), "compacted");
    let out = s.moved[0].new.seg;
    // The wiring lags: a FULL without the moves lifts the mark above the output.
    let moved = std::mem::take(&mut s.moved);
    s.discard(3);
    s.commit().unwrap();
    assert!(out.raw() < super::sim::high_water(&s.retained));
    s.moved = moved;
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.urgent(u64::MAX).unwrap();
    }
    eprintln!(
        "rev: source exists {}, output exists {}, unnamed {:?}",
        segs(&s).contains(&src),
        segs(&s).contains(&out),
        s.gc.unnamed_outputs()
    );
    s.crash_reopen(3).unwrap();
    let r = s.check_roots();
    eprintln!("rev: after the crash: {r:?}");
    r.unwrap();
}
```

---

# Re-review (2026-11-14, `p1/m17a` at `8671843`, fixes in `2ce3aa4`)

## Verdict

**Merge.** The blocker is fixed and every major is addressed or correctly handed to the engine. I found no new
safety finding. Three small things remain (below); none blocks the merge.

## Per finding

- **B1: fixed.** The flag is cleared only after `drop_segment` returned `Ok` in all three places: `forget`
  (`collector.rs:1008-1011`), `reconcile` for a flagged segment that is gone (`613-614`) and `delete`
  (`1087-1091`). A failed drop leaves the segment listed and flagged, and the next pass retries through
  `Gone → forget`. The released-bytes rule is sound: `delete` records the listed size in `uncertain` before the
  store call, removes it when the call returns (`Ok` or `Refused`), and only a call that failed keeps it; the
  bytes are released when a later recheck no longer lists the object (`forget`, `1014-1017`). I looked for a
  double release (the call failed without deleting, then the object is un-condemned or deleted normally
  later) and found none: the normal path overwrites and removes the entry, and `forget` runs only for an
  object that is absent from the store's listing. A stale `uncertain` entry for an object that still exists
  is harmless. The oracle now injects non-crash `Io`/`NoSpace` at 18 store and index points.
- **M1: fixed as option (a).** `Facts::keep_floor` is the lowest `chain_floor` of any retained FULL
  (`collector.rs:206-215`); `commit_kept` uses it. The test sets the tip aside and verifies the predecessor's
  chain. See the N12 recommendation below.
- **M2: handed to the engine (request 5, N13), which is right.** The GC-side "conservative" rule (a FULL's
  names count once a later FULL exists) changes nothing in practice: `Facts::read` takes
  `named_high.max(tip.high_water_mark)` (`collector.rs:225`), and the mark already includes the tip's own
  names, so every decision is the same as before. It is harmless, but the report and `gc.md` should not
  present it as closing part of (a); the hole stays open until the engine persists reliably. Still
  unreachable without pins.
- **M3: improved, acceptable for phase 1.** Leaf units are a fixed 32 leaves, the finish is per segment and
  bounded, recheck and delete are one object per unit with binary searches, the facts are read once per
  step, the urgent pass no longer clones. The two unsplit units are acceptable: one `walk_nodes` per root is
  one callback per stored node without reading leaves, and the candidate scan is one linear pass over sorted
  listings per pass. One residual: each leaf unit still traverses the whole root with the other leaves in the
  skip set, so a unit costs one hash lookup per leaf of the root and a mark costs `leaves² / 32` lookups
  (about 125 000 leaves per TiB at 16 KiB: fine at 1 TiB, over budget per unit near 8 TiB, and tens of
  seconds of CPU per mark well before that). Removing it needs a range walk in the map interface; record it
  for M17.B's measurements.
- **M4: fixed.** `abort_pass` writes the evidence best effort; a compaction error ends compaction and the
  pass finishes; an extent that does not open (`Auth`, `Corrupt`, `Unsupported`) marks its source bad for the
  process and is reported; `urgent` returns `Ok` with the bytes freed when it freed any. `abandon` sets
  `next_source` to the end, so the urgent loop terminates after an error.
- **M5: fixed.** All five tests exist and each mutation now fails at least one test.
- **Minors 1-3, 6-9 and the nits: done as reported.** Minors 4 and 5 are requests to the lead, as intended.

## Remaining (not blocking)

1. **Two fixes have no failing test.** Removing `drop_segment` from `reconcile` (the third B1 site) and
   reducing `deps` to its highest element (minor 1) both leave all 51 tests green. The `reconcile` case needs
   a lost list plus a missing flagged segment while the process still holds verified entries; a directed
   test is short. Add both with M17.B.
2. **The M2 wording** (above): say that the successor rule is a no-op today.
3. **The quadratic leaf walk** (above): a note in `gc.md` "Steps and their bounds" and a 64 M-chunk case in
   M17.B.

## N12: option (a) or (b)

**Recommend (b), stated as one rule: GC's retained set is every FULL from the lowest `chain_floor` recorded by
a retained FULL up to the tip, for roots and for "named by a retained commit" alike.**

Reasons:

- Under (a) the truncated volume's older retained root has lost data, and nothing on disk says so. The new
  tip legitimately records that commit as its `chain_floor`, so "this retained root is not a root" cannot be
  derived from the chain; it needs extra persistent state written by fsck (or inferred from the `.damaged`
  object), which the engine, GC and fsck would all have to honour until the next commit. Today the mark
  would fail closed on the missing pack and GC would not run until that commit; fsck would report errors on
  a volume it just repaired.
- Under (b) the truncated volume is an ordinary consistent volume: both retained roots are complete, the
  objects they name exist (so the sweep and `catch_up` at open see nothing missing), fsck is clean, GC runs
  at once, and no component needs a special case. That is what `last:2` was meant to buy.
- The cost of (b) is one more root per mark, which shares almost all subtrees with its successor, and
  garbage kept for one more commit interval. GC has to read that commit itself, as it already does for
  pins, because the view carries only commits from the tip's `chain_floor` up.
- (a) as implemented is a strict subset of (b), so merging now and extending the rule afterwards loses
  nothing. Do it before M14's truncate gate and M12b's mount test.

## What I ran

Same host and rules, sequential, one job; scratch copy and host directory deleted afterwards.

- Baseline `cargo test -p lunix-gc --lib`: 51 passed (202 s).
- Mutations, with `LUNIX_GC_CASES=16`, 6 crash workloads and 200 liveness volumes to keep each run short (so
  oracle counts are lower bounds):

| mutation | failing tests |
|---|---|
| `forget` without `drop_segment` | 3 (one oracle, the B1 test, the fault sweep) |
| `delete` clears the flag before the drop | 1 (the B1 test) |
| `reconcile` clears the flag without the drop | **0** |
| `keep_floor` = the tip's `chain_floor` | 31 (the truncate test and every test that checks the commit set) |
| listed segment without a flag not re-flagged | 1 |
| `outputs` half of the delete-time invariant removed | 1 |
| index copy not confirmed against the footer | 1 |
| no grace at all | 4 |
| `deps` reduced to the highest dependency | **0** |
| unreadable floor deletes commits | 2 |
