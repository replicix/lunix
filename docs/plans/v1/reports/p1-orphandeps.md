# Milestone orphandeps: copies relied on through a gone hint

Base: origin/main `fcacaf4`. Branch `p1/orphandeps`. Owner entry `[orphandeps]` in `tools/ownership.toml`.

## The hole (B1 of `01-p1-condemncut-review.md`, and its sibling on main)

`Liveness::deps` is recorded by `Mark::dep(from, on)` only when `from`, the segment an entry hints at, still exists. An entry whose hint points at a segment that is gone reads through the index to the copy's segment, and nothing recorded that dependency. A segment `S` could then be deleted while the only other copy of the content was in a segment `N` that only the tip FULL names (above `named_high`, or an unnamed compaction output). After `fsck --repair --truncate-tip` the mount sweep deletes `N` and the predecessor root cannot read the chunk.

## The sibling reproduces on main

Yes, through compaction `relocate`, with no proposal involved. The script (`crates/lunix-gc/src/tests/orphandeps.rs`, `script(true)`):

1. Segment `Y` holds chunks 0 and 1 and 2 (live) and five chunks that are overwritten and rolled off. Compaction moves the three live extents of `Y` into `S`.
2. The move of chunk 1 is dropped before the cut (the model of a restart losing pending moves); chunks 0 and 2 are re-hinted to `S`, and the cut names `S`. Chunk 1's entry keeps hinting `Y`; the index copy is in `S`.
3. `Y` is deleted once `S` is named by a FULL with a successor (`deps[Y] = {S}`, satisfied). Chunk 1's entry now hints a gone segment.
4. Chunks 0 and 2 are overwritten and rolled off; `S` has one live extent of three (via the index only) and is compacted into `N` (`relocate` moves the index copy; no entry hints `S`, so `deps[S]` stays empty). One cut names `N`: only the tip names it.
5. The delete passes run with no further cut: on main `S` is deleted (verdict `Delete`, `deps[S]` empty).
6. Simulated truncate-tip: `sweep_orphans` at the predecessor's high-water mark (removes `N` and the tip's packs), the index rebuilt from the segments left, every root of the predecessor read back.

On main (the `orphan_deps` check switched off, the recording left in) step 6 fails: `root 13 chunk 1: unreadable`. With the fix `S` is kept and the read-back passes. The control (`script(false)`: every re-hint applied, so the entry hints `S` and `deps[S]` already blocks) passes with and without the fix.

How real code gets there: the dropped move is the model of the review ("the pending moves are memory only"). The rest (a gone `Y` deleted legitimately by `deps`, then `relocate` moving the copy off `S`) is ordinary code on main; I did not trace a production sequence that loses exactly one move of a cut, so the partial loss is modeled, not derived. Code that would have prevented it and does not: `mark.rs` `dep()` ignores a gone `from`; `collector.rs` `segment_deletable()` reads only `deps[s]`.

## The fix

* `mark.rs`: `Liveness::orphan_deps: BTreeSet<SegId>`. `Mark::dep(from, on)` records `on` there when `from != on` and `from` does not exist (it is the one function both `stale()` and the `c.hint != hint` arm of `resolve` call). It is global, not per segment.
* `collector.rs`: `segment_deletable()` returns `NOT_DURABLE_YET` first when any member of `orphan_deps` is above `named_high` or in `outputs`. `Judge::verdict` turns that into `Defer(NOT_DURABLE_YET, ForFull)` and the existing `cut_wanted` path asks for the cut. Only a deferral is added: the check runs inside the `keep == None` arm, so it can never turn a Defer or an Uncondemn into a Delete, and it is not consulted for condemning (`dead`).
* Test-only: a per-thread counter (`ORPHAN_STATS`) of orphan deferrals and of segment delete verdicts, printed by the oracles.

## Tests

| test | what |
|---|---|
| `orphandeps::compaction_relocate_with_a_gone_hint_keeps_the_old_copy_until_the_new_one_is_durable` | the sibling above; fails without the fix |
| `orphandeps::control_every_rehint_applied_reads_back_after_truncate_tip` | control, the conditions are absent |
| `orphandeps::orphan_deps_is_recorded_only_for_a_gone_hinted_segment` | every mark taken while `Y` exists has an empty `orphan_deps`, in both scripts; a mark after `Y` is gone has a member |
| `orphandeps::a_crash_with_the_only_index_copy_in_an_unnamed_output_keeps_every_root_readable` | the crash shape (no proposal, no fsck): same script, but no cut names `N`; urgent passes run; the process dies and reopens (the sweep removes the unnamed `N`); every root is read. Fails on main (`root 11 chunk 1: unreadable`), passes with the fix |
| `orphandeps::control_a_crash_with_every_rehint_applied_keeps_every_root_readable` | its control |
| `collector::orphan_judge::*` (4) | the judge: member above `named_high` defers; member that is an unnamed output defers; durable members, an empty set, and a non-member output leave `Delete`; a live segment stays `Uncondemn` and a gone one `Gone` whatever the set holds |

Negative control (done, then restored): with `orphan_pending()` forced false the truncate-tip sibling, the crash test and the two deferral judge tests fail, the others pass.

The crash shape matches the one the C5 agent found in `oracle_unfiltered_proposals_16k_id16` (a pin or retained root hints a gone segment, a pass compacts the segment holding the index copy into unnamed outputs above `named_high`, the source is deleted, a crash drops the outputs). That case needs the condemncut branch; this branch reproduces the same hole on main with a lost re-hint instead of a proposal. A fresh mark after the compaction sees the copy in the output, which `orphan_deps` holds and `orphan_pending` defers. One observation: without the fix the ordinary `pass()` is not repeated on the idle volume after a compaction, so the crash test drives `urgent` passes.

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-7, `-j 8`) on the final tree | green: fmt, clippy `-D warnings`, 2416 tests passed, 0 failed, 30 ignored, ownership `orphandeps ok (7 changed paths)` |
| gc oracles at `LUNIX_GC_CASES=400` | pass: `oracle_16k_id16`, `oracle_16k_id32`, `oracle_64k_id16`, `oracle_64k_id32`, `oracle_lean_id0`. Fail, unrelated (ledger pack bytes: "ledger [..] != files [..]", the bug another agent is fixing; reported, not chased): `oracle_fast_recheck_16k_id16`, `oracle_fast_recheck_64k_id32` |
| stall check (the oracles print `orphan_deps deferred N segment delete verdicts, M allowed`, 400 cases each) | 16k id16: 142 / 9488; 16k id32: 61 / 9258; 64k id16: 31 / 9130; 64k id32: 20 / 9334; lean: 0 / 6358. About 0.2 to 1.5 percent of delete verdicts are deferred and every oracle still reaches the quiescent state (`check_quiescent` passes), so a deferral lasts one cut and does not stall |
| kill loops, release, `LUNIX_KILL_HITS=all`, four run concurrently on CPUs 0-7 (`--test-threads=4`, built on the tree before the crash test was added; the collector code is the same) | all pass: plain 16K (68 kills, 4959 s), integrity 16K (68, 4953 s), plain 64K (68, 4879 s), recheck plain 16K (15, 1984 s). Every recovery printed `drift_segments 0, report_mismatches 0, shadow_unconfirmed_named 0`. A cargo test of this branch ran on the host for a few minutes during the loops (CPU contention only) |

## Deviations and open choices

* The partial loss of re-hints is modeled by dropping one move from `Sim::moved`; I did not derive a production sequence that loses exactly one move of a cut.
* The deferral holds every segment delete (not only `S`) while any orphan copy is not durable, as the review specified; the oracles show it costs one cut.
* rust-skills: no conflicts. `Mark::dep` early-returns instead of nesting.
* Remote disk: removed at the end.

After the rebase onto origin/main `5f02e505` (it brought `p1/recheckoracle`, which fixes the ledger bug): all 120 `lunix-gc` lib tests pass at `LUNIX_GC_CASES=400`, including both `oracle_fast_recheck_*` (the two failures listed above are gone). Deferrals there: 39 and 29 of 5893 and 6099 delete verdicts in the two recheck oracles. `tools/ci.sh` and the kill loops ran on the pre-rebase tree (the rebase touches only `lunix-gc` tests and the ledger fix); they were not repeated.
