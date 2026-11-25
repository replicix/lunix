# Milestone orphanhold: the orphan durability rule made precise

Base: origin/main `d552bbe0`. Branch `p1/orphanhold`. Owner entry `[orphanhold]` in `tools/ownership.toml`.
Builds and tests ran only on the remote test host (kernel 7.0.0) through `tools/remote.sh`, CPUs 0-15, `-j 16`.
No on-disk format change. Items are those of "Round 2" of `01-p1-condemncut-review.md`.

## Part 1: S8 confirmed, and the precise hold

**S8 reproduced on the stack** with the global hold, and does not with the precise one.

Test: `stack_gc_ladder::a_pending_orphan_copy_does_not_keep_the_urgent_pass_from_freeing_space_on_a_full_host`
(real stack, manual engine, integrity, 64 KiB chunks, retain 2, host file system that refuses new segments).
1. Chunks 0..64 (`Y1`), chunks 8..64 again (`Y2`); a snapshot pin is sealed on the tip: it reaches 8 extents of `Y1`.
2. Chunks 64..128 (`W`), chunks 88..128 again (`V`): `W` keeps 24 tip-only extents.
3. A pass compacts `Y1` and `W` into `S`; two cuts name it; `Y1` and `W` are deleted. The pin's entries for chunks 0..8
   now hint the gone `Y1` (the index copy is in `S`).
4. Chunks 64..128 are written again and the window rolls: `V` is dead and unrelated; `S` keeps only the 8 pinned
   extents.
5. An urgent pass condemns `V` and compacts `S` into the unnamed `N`: the pin's copies are now a pending orphan copy.
6. After the floor grace the host bound is set to what is on disk. A cut fails for space, the urgent pass runs.

* **Old rule (every segment delete held while any orphan copy is pending):** the urgent pass defers `V`, the cut
  that would name `N` fails for space, nothing frees space: the test ended with `the volume stays full (240 failed
  cuts)` after 120 s. Done by temporarily restoring the global check in `segment_deletable` (reverted).
* **Precise rule:** the urgent pass deletes `V` (freed 1.6 MB), the next cut is made without raising the bound
  (1 failed cut, 0.6 s), `S` is still there and a later pass without a cut defers it with the orphan reason and asks for
  a cut (`orphan_deferrals` 1, `cuts_requested` 1); after three more cuts `S` is freed. The test asserts all of it,
  then fsck and a reopen read equal.

So the full-host case is real: the cut cannot be made and nothing else frees the space. The first case of the review
(a pinned root keeping the volume in this state for most passes) is the same state; I did not build a long-running
workload for it. The per-event price is the same two cuts, and the precise rule makes it cost only the segments that
hold the ids.

**The fix (approved by the lead, as specified):**
* `Mark::new(cfg, named_high, outputs)`: the mark knows which copies are pending when it starts. `orphan_dep(on, id)`
  records the chunk id of a pending copy. After the live pass the mark has a new stage `Hold`: for each dead listed
  segment it reads the footer (the bounded footer cache, 32 footers per unit) and, when the footer holds a pending id,
  records `Liveness::orphan_holds[segment] = {copy segments}`. Only when some copy is pending; a footer that cannot be
  read holds the segment for every pending copy (the mark does not fail for a dead segment).
* `Judge::segment_deletable` reads `deps[s]` and `orphan_holds[s]`, filters both by the verdict-time durability
  (`named_high`, `outputs`) and defers only those. No other segment is held. `Liveness::orphan_deps` stays (the set of
  all orphan copies, used by the tests).
* The public `lunix_gc::liveness` (fsck's comparison) passes `named_high = u64::MAX`: no hold work.
* Negative controls, in the suite: `control_with_the_hold_off_the_sibling_and_the_crash_shape_fail` (a test-only switch
  turns the hold off: the truncate-tip sibling and the crash shape fail with `unreadable`, as before the fix);
  the existing orphandeps and the B1 proposal tests pass with it on.
* Test that an unrelated segment is no longer held: `orphandeps::an_unrelated_dead_segment_is_deleted_while_the_orphan_copy_is_pending`
  (both endings, S still held, the unrelated segment gone), `orphan_judge::an_unrelated_segment_is_not_held_by_a_pending_orphan_copy`,
  and the stack test above.

## Part 2: S9

The orphan deferral now carries the `helps` test (blocking copy in an unnamed output or named by the tip). When no cut
helps the reason is `... a cut does not help` and the engine is not asked (`asks_for_cut`). Test
`orphan_judge::the_orphan_rule_asks_for_a_cut_only_when_it_helps` (judge level: the empty-FULL loop needs a restart to
reach the state, so it is shown on the verdicts: no-help gives the no-cut reason, `asks_for_cut` is false; tip-named
and output give the cut-asking reason).

## Part 3: N9

`stale()` records a copy in `orphan_deps` directly when the hinted segment does not hold the id (gone or existing),
unless the copy is in the hinted segment itself. Test `mark::tests::a_hinted_segment_that_exists_but_does_not_hold_the_id_records_an_orphan_dep`
(`deps` stays empty, `orphan_deps` and the pending id are set).

## Part 4: before-T6 items

* **Product counters** (not `cfg(test)`): `GcReclaimStats::orphan_deferrals` (delete verdicts the orphan rule deferred,
  counted per verdict) and `cuts_requested` (cuts the collector asked for). Distinct evidence reasons:
  `an orphan copy of an id it holds is not durable yet (the hinted segment is gone)` and its `; a cut does not help`
  form. `lunix gc --status` prints `durability holds`; `gc.md` documents them (section "Deleting", paragraph
  "Orphan copies", and the reason list). The ctl round-trip test carries the fields.
* **Negative control on the unfiltered oracle:** `oracle::control_unfiltered_oracle_fails_without_the_pin_roots_in_the_recheck`
  (`should_panic`): a test-only switch leaves the pin roots out of the mark's root set (the commits stay); the unfiltered
  oracle fails ("walk of root N: not found: pack"). The switch is off in every other test.
* **B1 proposal test asserts its mechanism:** in the proposal arm the index copy of chunk 1's id has left `S` and only
  the tip FULL names the new segment; in the control arm it is still in `S`; a third run adds three cuts and passes and
  shows `S` deleted once the deferral ends (`b1_script` now returns a struct).
* **`GcConfig::propose_unfiltered` is `#[cfg(test)]`** (the collector reads it only under `cfg(test)`).
* **gc.md:** `shadow_unconfirmed_named` is 0 for a pin more than a second old; in `shadow` mode an offer in the
  second after a pin appeared can raise it (and the iface doc comment says the same).
* `fsck --repair --gc` cannot cut: one sentence in `gc.md`.

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-15, `-j 16`) on the final tree | green: fmt, clippy `-D warnings`, workspace tests, the crashpoint legs, the lunixd stack tests in release (`stack_throttle` included, run with the host otherwise quiet: passed), ownership `orphanhold ok (14 changed paths)` |
| gc oracles, `LUNIX_GC_CASES=400` | 11 of 11 pass (`cargo test --release -p lunix-gc --lib oracle`: the ten oracles and the new control, which must panic and does): `oracle_16k_id16`, `_16k_id32`, `_64k_id16`, `_64k_id32`, `_lean_id0`, `_fast_recheck_16k_id16`, `_fast_recheck_64k_id32`, `oracle_unfiltered_proposals_16k_id16`, `_64k_id32`, `_with_recheck_passes_16k_id16`. The printed `orphan_deps deferred N / M allowed` (thread-local, shared by the tests on a thread): 217/6898, 580/13530, 916/9529, 0/6778 (lean), 174/10456, 506/13442, 121/10243, 254/7168: 1 to 6 percent of the delete verdicts wait for a pending copy, every oracle reaches the quiescent state |
| kill loops, release, `LUNIX_KILL_HITS=all`, four at once (`--test-threads=4`, CPUs 0-15) | all pass in 5449 s: plain 16K (74 kills, 5214 s), integrity 16K (74, 5350 s), plain 64K (74, 5449 s), condemn arm with `gc_condemn_on_cut = on` (30 kills, 10 points, 3160 s); no "reference table" line in the log |
| `stack_throttle` | passed in `tools/ci.sh`; it did not fail while the loops ran (not run then), so there is no second result |

The new tests and where they ran: the stack test in 53 s alone (precise rule) and 153 s (old rule, red); `lunix-gc`
lib tests 152 pass at the default cases.


## Deviations and open choices

* The stack test shows the full-host stall by a temporary restoration of the old global check, not by a permanent
  switch (the hold-off switch is a `lunix-gc` `cfg(test)` thread-local and not reachable from `lunixd`).
* The orphan counters count verdicts, so a held segment counts once per pass it waits.
* The oracles' `orphan_deps deferred N` line is the `cfg(test)` thread-local, summed over the tests that share a
  thread; it now counts only the segments the precise rule held.
* rust-skills: no conflicts.
* Remote disk: removed at the end.
