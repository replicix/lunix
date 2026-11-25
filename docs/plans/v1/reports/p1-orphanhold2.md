# Milestone orphanhold2: follow-ups of the orphanhold review (S10-S13, N10-N18)

Base: origin/main `3d09bbe4` (orphanhold merged). Branch `p1/orphanhold2`, ownership entry `[orphanhold]`.
Builds and tests ran only on the remote test host (kernel 7.0.0), CPUs 0-15, `-j 16`. No on-disk format change.
Review: `01-p1-orphanhold-review.md` (verdict merge, no blocker).

## S10: hold-off negative controls

* **B1 proposal test:** `propose::control_with_the_hold_off_the_b1_proposal_script_fails`: `b1_script(true, false)`
  with the hold off loses `S` and the predecessor does not read back (`s_alive == false`, `read.is_err()`), on the
  current code. The sibling and crash-shape controls stay.
* **Oracles:** `LUNIX_GC_HOLD_OFF=1` (test-only env of `oracle_cfg`) with `LUNIX_GC_CASES=400`:
  **two of the ten oracles fail**, `oracle_unfiltered_proposals_16k_id16` and
  `oracle_unfiltered_proposals_with_recheck_passes_16k_id16`, both with `after a crash: root N chunk M: unreadable`
  (the crash shape reached through random histories with pins and proposals). The other eight pass with the hold
  off, so they do not witness the rule; no truncate-tip step was added to the oracle because the two that fail
  already do, and the truncate-tip shape is covered by the three scripts (sibling, B1, unrelated-segment) with
  read-back after `truncate_tip_and_check`. If a T6 pin cell needs the truncate-tip shape from random histories, that
  is the place to add it.
* The switches are drop guards now (`HoldOff`, `DropPinRoots`): restored on panic (N12).

## S11: the `Hold` branches

Mark-level tests in `mark::tests` (a mark placed at the start of `Hold` over a real sim volume):
* `an_unreadable_footer_holds_the_segment_for_every_pending_copy` (a store whose `footer` fails for one dead segment:
  held for both pending copies, readable holders only for their own, an unrelated readable segment not at all;
  `hold_footers` 6, `hold_failed` 1).
* `two_dead_holders_of_one_pending_id_are_both_held` (an older duplicate sealed by hand).
* `the_hold_stage_spans_units` (101 dead segments: 1 Finish unit plus 4 Hold units, a holder in the last unit found).
* `past_the_bound_the_copies_hold_every_dead_segment` (N11, below).
* Sim tests that read `orphan_holds` from a real mark with the collector's facts (`mark_with_facts`, `liveness_with`):
  `a_real_mark_holds_the_segment_that_holds_the_id_and_no_other` (both endings: the keys are exactly `{S}` and the
  value `{N}`; the unrelated dead segment is not a key). The three controls where nothing is pending assert the
  `Hold` stage did not run.

## S12: instruments

`GcReclaimStats`: `hold_marks`, `hold_footers`, `hold_footer_errors`; the `Err(_)` branch now logs a warning naming
the segment and the error and counts. `lunix gc --status` line `durability holds`; `gc.md`. Asserted in the stack
test (marks and footers non-zero, errors 0).

## S13: denominator and wording

`segment_delete_verdicts` (segment verdicts that ended in a delete or in a deferral for a copy that is not durable;
counted in `apply_verdict`, so dry runs do not move it). `gc.md` no longer says "above 1 % is a finding": the
ratio grows with the pass rate while a copy is pending and has no threshold on a volume with a pin, a restart or an
old retained root; there a non-zero rate is information. In a cell without them a non-zero count is a finding, and
`hold_footer_errors` must be 0 everywhere.

**Measured, oracles at 400 cases** (orphan-rule deferrals as a share of deferrals plus allowed deletes, per oracle,
thread-local counters): 16k_id32 0.9 %, 16k_id16 1.2 %, fast_recheck_16k 3.0 %, unfiltered_16k 4.1 %,
unfiltered_with_recheck_16k 8.8 %, lean 0 % (no index, no hold), 64k_id16 1.6 %, unfiltered_64k 3.6 %, 64k_id32 1.2 %,
fast_recheck_64k 3.4 %. So 0 to 8.8 %, and 0.9 to 8.8 % where a hold can exist (the orphanhold report said 1 to
6 %; the 8.8 % is the unfiltered oracle with recheck passes). These are histories with pins, lost moves and crashes,
not a T6 cell.

## Nits

* N10 done: `Hold` lists segments with `live_bytes == 0` (the judge's test). N14 done: outputs are not listed.
* N11 done: `orphan_ids`, the overflow set and the Hold list are in `heap_bytes`; `orphan_ids` is bounded at 65 536
  ids (about 100 bytes each), past that the copies of the dropped ids go to `orphan_overflow` and hold every dead
  segment (conservative, the old global behaviour for those copies); tested by lowering the bound.
* N12 done (drop guards). N13 done (`expected = "walk of root"`). N15 done: `dry_run` calls `observe_named`.
* N16 done: the stack test's doc says the red result was measured by a temporary revert. N17 done: the
  `on != seg` filter is gone. N18: `the_hold_works_with_encrypted_extents` (sim encrypted codec, truncate-tip
  read-back, unrelated segment freed, holder kept).

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-15, `-j 16`) | green on the third run: fmt, clippy `-D warnings`, workspace tests, crashpoint legs, lunixd stack tests, ownership. **Runs 1 and 2 failed `stack_throttle::with_the_band_off_the_volume_is_never_paced`** (state `Stop` with throttling off, 44 MiB of the 64 MiB cap; the known load flake, nothing else of mine was running). Rerun alone: 13 of 13 pass (65 s). The whole stack-test sequence of `ci.sh` run by itself: all pass. The third `ci.sh` run: green. Nothing in this change is on that test's path (the `Hold` stage runs only while an orphan copy is pending) |
| gc oracles, `LUNIX_GC_CASES=400` | 11 of 11 pass (the ten oracles and the control); hold-off run (S10): 2 fail as above |
| kill loops, release, `LUNIX_KILL_HITS=all`, three at once (`--test-threads=3`) | all pass in 5261 s: plain 16K (74 kills, 5202 s), integrity 16K (74, 5261 s), condemn arm with `on` (30 kills, 10 points, 3157 s); no "reference table" line |


## Deviations

* rust-skills: no conflicts. Remote disk: removed at the end.
