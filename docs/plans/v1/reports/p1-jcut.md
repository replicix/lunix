# Report p1-jcut: ADR-33 T3 and T4 (bounded cuts, wake per freed ring file, write-path counters)

Branch `p1/jcut` from `origin/main` 8731931. Spec: ADR-33 (accepted 2026-11-19) and the note
`docs/explanation/research/2026-11-19-write-throughput.md` (§2.2, §4.2, §5-§7). Builds and tests ran only on the remote test host
(kernel 7.0.0) through `tools/remote.sh`, CPUs 8-15, `-j 4`. No root run, no on-disk format change, no dependency change.

## What was built

| item | what | where | tests |
|---|---|---|---|
| T3 bound | a cut taken by the coordinator (`tick`, and the new `Engine::commit_bounded`) is lowered to at most `cut_target` times the drain rate of the last four committed cuts, in journal bytes. The bytes come from the per-lane charge lists (`JournalCharges::cut_for`: a merge over the lanes in `write_seq` order, the first record at which the sum reaches the budget); the rate is journal bytes over the time from C1 to C9 of each cut on the engine's clock (`DrainHistory`). No history: the rate bounds nothing | `lunix-engine/src/{coord,cut,full}.rs` | `the_cut_follows_the_drain_rate_of_the_last_cuts`, `the_coordinators_tick_cuts_the_bound_and_a_light_load_is_cut_whole`, `a_cut_is_at_least_one_record` |
| T3 floor | never less than what frees one ring file of the fullest lane: new read-only query `Journal::cut_freeing_file` (fullest lane = fewest free files among lanes with a sealed file, its oldest sealed file's highest `write_seq`; ties take the lower answer). The floor wins over the half ring | `lunix-iface/src/journal.rs`, `lunix-journal/src/journal.rs`, `lunix-testkit/src/journal.rs` | `a_cut_is_never_less_than_what_frees_a_ring_file`, `the_cut_that_frees_a_file_is_the_last_record_of_the_oldest_sealed_file` (journal) |
| T3 ceiling | never more than half the ring (`fill().capacity_bytes / 2`) | `coord.rs` | `a_cut_is_never_more_than_half_the_ring` |
| T3 back to back | the background thread does not wait its period after a tick that committed while the ring is above `journal_fill_trigger` with writes not yet cut (`Inner::coordinator_wait`; an error or an idle tick waits as before) | `engine.rs`, `coord.rs` | `commits_follow_back_to_back_while_the_ring_is_above_the_fill_trigger`, `the_wait_is_a_period_when_the_last_tick_did_not_commit` |
| T3 config | `EngineConfig::cut_target` (1 s); `lunixd.toml` key `cut_target_ms` (default 1000, 10 ..= 60000) through `OpenParams::cut_target`. `journal_resident_max` already existed from T1. `volume.json` untouched | `lunix-engine/src/config.rs`, `lunixd/src/{config,daemon}.rs`, `lunixd/src/stack/mod.rs` | `cut_target_ms_is_validated`, the full-file parse test |
| T4 wake | `reclaim` signals a lane's wake fd as soon as one of its files is free (after the zero rewrite, rename and directory sync), not after every lane's files; `open_lane` keeps the wake fd in `Shared::wakes` | `lunix-journal/src/{journal,shared}.rs` | `a_lane_is_woken_as_soon_as_one_of_its_files_is_free` |
| T4 counters | ring waiters (gauge), longest ring wait (a wait still going is counted as of its last judgement), last cut bytes and duration: atomics in the engine (`Engine::cut_stats`, `CutStats`); the window's resident bytes, hits and misses from `FileJournal::window_stats`. All in `VolumeStatusBody::write_path` (`WritePathStatus`), read without the coordinator mutex | `lunix-engine/src/{cut,queue,full}.rs`, `lunix-iface/src/ctl/msg.rs`, `lunixd/src/daemon.rs` | `ring_waiters_and_the_longest_wait_are_counted`, `a_failed_ring_wait_leaves_the_waiter_count`, `status_lockfree` (the fields come from the same atomics), the ctl round-trip property test |
| DESIGN | §8.1 (bounded cut), §8.2 step 1, §4.1 "Reclaim" (per-file wake) | `docs/explanation/DESIGN.md` | |

Every cut stays a prefix cut: the bound only picks a smaller `c'` in `(last cut, c]` after C1's wait, before the map's leaf cap (`bound_cut`).
C1-C9, their crash points, the reclaim rule (a record leaves the ring only below a durable FULL) and every on-disk byte are unchanged; the charge
release at C9 is per cut as before.

Explicit commits (`commit_now`, a grow, the final commit of a detach) stay unbounded: they ask for everything written so far. `commit_bounded` is the
bounded counterpart for tests.

## Tests: crash points and recovery at a small bound

- `lunix-engine/tests/model.rs`: `model_16k_one_record_cuts` and `model_64k_one_record_cuts` run the M11 model (random writes, FLUSH/FUA, discards,
  grows, ticks, crashes at every engine crash point and inside recovery, plain and with marks) with `cut_target` 1 ms and each cut taking 1 s of the
  manual clock: every cut after the first is about one record. The durable-prefix claim of the model follows `cut_seq` (a `write_seq` to log-length map;
  after any reopen the recovered tip's `cut_seq` is a lower bound of the recovered prefix). The suite asserts that the bound is in play
  (at least one partial cut per case) and the usual crash-point coverage (5 workloads per point). The default 100 cases x 2000 ops.
- `lunixd/tests/stack_kill.rs`: `LUNIX_KILL_CUT_MS=<ms>` runs the real-stack kill loops with bounded cuts (the child's commits are `commit_bounded`).
  Run by hand, see "Gate".

## Failing without the rule

Each test below was run with its rule removed in the source (the mutation is a one-line edit on the remote copy) and failed; with the rule it passes.

| rule removed | tests that failed |
|---|---|
| `reclaim` does not signal the lane (`wake_lane`) | `a_lane_is_woken_as_soon_as_one_of_its_files_is_free` (first assertion: the freed file's lane is not signalled while the reclaim goes on) |
| `cut_freeing_file` takes the emptiest lane instead of the fullest (`.min()` -> `.max()`) | `the_cut_that_frees_a_file_is_the_last_record_of_the_oldest_sealed_file` (60 instead of 59) |
| the drain rate does not bound the cut (the budget is the half ring only) | `the_cut_follows_the_drain_rate_of_the_last_cuts` (140 instead of 60), `the_coordinators_tick_cuts_the_bound_and_a_light_load_is_cut_whole`, `a_cut_is_at_least_one_record`, `a_cut_is_never_less_than_what_frees_a_ring_file`, `commits_follow_back_to_back_...` |
| no file-boundary floor | `a_cut_is_never_less_than_what_frees_a_ring_file` (11 instead of 252), `commits_follow_back_to_back_...` |
| no half-ring ceiling | `a_cut_is_never_more_than_half_the_ring` (2097 instead of 2017) |
| `tick` cuts everything (the bound is only for `commit_bounded`) | `the_coordinators_tick_cuts_the_bound_and_a_light_load_is_cut_whole` (55 instead of 6), `commits_follow_back_to_back_...` |
| no back-to-back wait (`coordinator_wait` always the period) | `commits_follow_back_to_back_while_the_ring_is_above_the_fill_trigger` |
| ring waiters not counted | `ring_waiters_and_the_longest_wait_are_counted`, `a_failed_ring_wait_leaves_the_waiter_count` |
| the bound removed from the commit path (every cut everything) | `model_16k_one_record_cuts`, `model_64k_one_record_cuts` (no partial cut: the model proves nothing without the bound) |

`a_cut_is_at_least_one_record` and the light-load half of the tick test describe behaviour the structure already gives (a budget of 0 takes the first
record; a load below the bound is cut whole); they fail when the drain rate is removed, not on a rule of their own.
No test of the per-file wake at the engine level: the engine's `MemJournal` fake has no per-file reclaim, and the real stack's test driver polls its
queues continuously, so it cannot tell a signalled lane from an unsignalled one; the journal-level test observes the signal itself.

