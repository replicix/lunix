# Milestone refs2: before the shadow soak of the C5 reference table

What the reviewer (`01-p1-refs-review.md`, round 2) required before the soak of `gc_condemn_on_cut = shadow`.

## What was built

| item | what |
|---|---|
| R2-1 | `Liveness::older_refs`: the mark collects the segments hinted at by the leaves it walks for every root after the tip (retained FULLs and pins). `MarkCondemned.named` is that set minus the tip's own segments. `judge` counts `shadow_unconfirmed_named` **first** (before confirmed, live, listed, kept). In `RefsStats`, in `lunix gc --status` (`refs shadow`, "named by a retained root (must be 0)") and in gc.md. It is 0 on a volume without pins; shadow mode ignores pins (gc.md), so a pin on an old commit can legitimately name an offered segment. The soak must have none. It is a lower bound (a root retired between the offer and the mark is not seen) |
| exact oracle | `EngineHooks::shadow_offered(tip, offered)` (default no-op) is called by `shadow_offer` after the table lock is dropped. In `tests/refs.rs` a wrapper collector (`Oracle`) runs in the collector call right after the offer (same coordinator mutex, same roots), walks every root of `mark::root_set` through the map and fails when an offered segment is hinted at. The proptest drivers assert `checked > 0` and `skipped == 0` (every offer was checked at its own tip), and the tick asserts the oracle and `shadow_unconfirmed_named == 0`. Negative control (done, then restored): with the lag mutated to 0 in `coord.rs` the oracle fails the three proptests ("offered at tip 2 but a retained root names it"), and, with its assertion placed second, the counter fails them too |
| recall / lead by bytes | `MarkCondemned.segs` is `(SegId, bytes)`. `shadow_confirmed_bytes`, `shadow_early_bytes`, `shadow_missed_bytes`, `shadow_lead_byte_ms` (bytes x lead ms, saturating); `lunix gc --status` has a new `refs bytes` line (human size and exact bytes, recall by bytes, lead byte-ms). gc.md updated |
| R2-2 | kill-loop `verify` takes `r0 = rebases` right after `commit_now` and asserts `rebases >= r0 + 2`, prints "refs after recovery: rebases R (cut at r0), drift_segments, report_mismatches, shadow_unconfirmed_named", and `assert_refs_clean` asserts `shadow_unconfirmed_named == 0`. The first full run failed at an early kill (`gc.mark.end:1`, 2 segments): `settle_gc` runs no further pass when nothing is left to do, so only one mark followed the cut. The verify now asks for requested passes (at most 4, each marks) until two marks rebased the table after the cut. The wording of p1-refs.md is fixed: S4 shows that a rebuilt table plus one real cut equals the next mark; deltas before or during the first mark after a recovery are covered by the engine proptests (`Op::Reopen`) only |
| R2-4 | docs of `shadow_unconfirmed_kept` and `_live` rewritten (a segment a retained root names is `named`, never `kept`) |
| R2-5 | `the_shadow_table_costs_a_cut_within_three_percent` is `the_shadow_table_cut_cost_is_reported`; module doc and printed line say the 3 % is a target, not asserted |
| B1 tests | `two_failures_around_one_mark_start_take_two_marks_to_repair`, `a_zero_recorded_while_drifted_is_gone_after_the_repair` (refs.rs unit tests); `an_offer_a_retained_root_still_names_is_counted_as_named`; gc: `a_mark_names_the_segments_an_older_retained_root_hints_at` |
| R2-3 | `table_costs` returns `(snapshot, ready, rebase)`; `the_table_snapshot_ready_and_rebase_cost_per_call` prints all three. Report only, no fix |

Files outside the refs list that the entry `[refs2]` in `tools/ownership.toml` owns: `engine/src/{coord,deps,lib}.rs`, `engine/tests/common/mod.rs`, and `p1-refs.md` (wording).

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-15, `-j 16`), on the final tree | green: fmt, clippy `-D warnings`, 2407 tests passed, 0 failed, crashpoint legs, lunixd stack tests in release, ownership |
| kill loops, release, `LUNIX_KILL_HITS=all`, `gc_condemn_on_cut = shadow` and `gc_fast_recheck` on, four run concurrently on CPUs 0-15 | plain 16K pass (69 recoveries, 4803 s), integrity 16K pass (69, 5113 s), plain 64K pass (69, 4852 s), recheck plain 16K pass (15, 1384 s). In every recovery: `rebases >= r0 + 2` (smallest `rebases - r0` seen: 2 in the plain and integrity loops, 3 in the recheck loop), `drift_segments == 0`, `report_mismatches == 0`, `shadow_unconfirmed_named == 0`, no "reference table" warning in a child log |
| engine proptests (48 cases each, dedup- and compaction-heavy) | pass with the oracle: `checked > 0`, `skipped == 0` |

The first kill-loop run (before the requested-pass fallback) failed `rebases >= r0 + 2` at one early kill of the integrity loop and the plain 64K loop; the recheck loop passed. All four were rerun from scratch with the final test code.

## Cost of `rebase` under the mutexes

Single call on a table that is right (the steady state: the comparison and the repair pass over every segment), mark count list built outside the timing, 20 repetitions, release build, quiet host (nothing else on CPUs 0-15 once the loops were done); snapshot clone next to it:

| segments | waiting zeros | mark-start snapshot | `ready()` | `rebase` |
|---|---|---|---|---|
| 16 000 | 0 | 0.02 ms | 0.000 ms | **1.46 ms** |
| 128 000 | 0 | 0.21 ms | 0.000 ms | **13.7 ms** |
| 1 000 000 | 0 | 8.80 ms | 0.000 ms | **306 ms** |
| 1 000 000 | 500 000 | 8.84 ms | 7.09 ms | 150 ms |

`rebase` is about 35 times the snapshot clone at 1M segments and runs under the coordinator mutex and the table mutex (every map-transaction commit takes the latter): about 0.3 s of coordinator stall per mark at 1M segments, 14 ms at 128k, 1.5 ms at 16k. Under pressure (recheck marks every 1 to 2 s) 1M segments would be 15 to 30 % of coordinator time. Fine for the soak volumes; before the mode is on for volumes above roughly 100k segments `rebase` needs the merge of the sorted mark list against the table, outside the coordinator mutex, as the review proposed. (Figures with 0 waiting are the ones to quote; with half the segments waiting there are half as many counts.)

## rust-skills

No conflicts.
