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

## Gate

`tools/ci.sh` on the host (CPUs 8-15, `-j 4`, `CARGO_PROFILE_{DEV,TEST,RELEASE}_DEBUG=0`) on `6c404b8` (the code commit; the commit after it changes only
this report): green, ownership included (`jcut ok (27 changed paths)`). 2 087 tests passed, 0 failed, 28 ignored (the existing by-hand cells), the sum of
every `test result` line. origin/main did not move (8731931), so no rebase.

By hand, in the kill loops that stay out of CI (plain, integrity and encrypted volumes, 16 KiB chunks, first hit of every point,
`LUNIX_KILL_CUT_MS=1`, the child's commits are `commit_bounded`, `cargo test -p lunixd --features testsupport,crashpoints --test stack_kill`):

| loop | points | reached | kills | result |
|---|---|---|---|---|
| `kill_loop_plain_16k` | 55 | 52 | 52 | ok (1 727 s) |
| `kill_loop_integrity_16k` | 55 | 52 | 52 | ok (1 771 s) |
| `kill_loop_encrypted_16k` | 55 | 52 | 52 | ok (1 749 s) |

Not run: the 64 KiB loops and `LUNIX_KILL_HITS=all`; I did not measure how many of the commits in the child left requests uncut (the engine model counts it:
every case has a partial cut). The M16 write cells and `lunix status` under load are T6 (measurement), not this milestone.

## Deviations and choices

1. **`lunix-iface`, beyond the one query.** `Journal::cut_freeing_file` (documented in rustdoc, implemented by `FileJournal`, `MemJournal`, the engine's test
   wrapper and the iface trivial fixture) is the pre-approved change. The counters also needed the control protocol: `VolumeStatusBody` gains one field,
   `write_path: WritePathStatus` (seven `u64`; ADR-33's "Interface changes" lists "counters"). `EngineStats` could not carry them: it is persisted in the
   checkpoint slot. The two `VolumeStatusBody` literals of `lunix-cli` and the ctl property test were updated; the CLI's text output does not show the new
   fields (they are in `--json`), to keep its goldens untouched. Per the no-compat rule the field has no serde default.
2. **Explicit commits stay unbounded.** `commit_now`, `grow` and the final commit of a detach cut everything written so far; the bound is the
   coordinator's (`tick`, hence the background thread). `Engine::commit_bounded` is the bounded counterpart, used by the tests and the kill loops. Nothing
   in production calls it.
3. **No history, no rate bound.** The first cut after an attach (and any cut while the history took no measurable time) is bounded only by the half ring and
   the floor. The history is the last four committed cuts of this process; it is not persisted.
4. **The floor wins over the ceiling.** If one ring file of the fullest lane is more than half the ring, the cut frees the file.
5. **`coordinator_wait`, `reclaim_hook` and the `advance` hook of the test bench** are test-only conveniences: `Engine::coordinator_wait` and
   `FileJournal::set_reclaim_hook` are `#[doc(hidden)]` public (the journal's other hooks are the same). A tick that failed waits its period again, so a
   commit that keeps failing with the ring above the trigger does not spin.
6. **Ring waiters and the longest wait** count a request from the first time it parks for the ring to its admission, failure or its queue's drop. A wait
   still going is counted as of the last time it was judged (each wake and each period of the coordinator thread), not continuously.
7. **Ownership.** `[jcut]` lists exactly the 27 changed paths, including `crates/lunix-engine/src/**` and `tests/**` (new files `cut.rs`, `tests/jcut.rs`),
   `lunix-iface`'s `journal.rs`, `ctl/msg.rs`, two test files, the testkit's `journal.rs`, two `lunix-cli` files, three `lunixd` sources and two of its tests. Several overlap
   other milestones' entries (`lunixd/src/stack/mod.rs`, `daemon.rs`, `config.rs` with `jwin`); the edits are small and local.
8. **`DECISIONS.md` is not edited** (not in this milestone's paths): ADR-33's "Not implemented yet" is stale for T1-T5 now, and `cut_target_ms` is a
   `lunixd.toml` key documented in `lunixd/src/config.rs`.
9. **A wrong first gate run.** My failing-without-the-rule harness restored each mutated file with an older modification time than the remote build, so the
   next cargo run reused the mutated build; the first `ci.sh` run failed in `lunix-journal` for that reason (it ran the `.max()` mutation). The table above comes
   from runs in which the mutated file was the newest one; after the harness was fixed (it touches the file) every file was touched and `ci.sh` run again:
   that is the gate above.
10. **Not tested:** the per-file wake at the engine level (see above); `cut_freeing_file` against the real journal with several lanes beyond the two-lane
    test; the daemon's `status` mapping beyond `status_lockfree`'s field checks.

## rust-skills conflicts

None.

## Remote disk

Largest sample 9.0 GiB (the full gate leaves 6.6 GiB of incremental cache; budget 10 GiB), then 2.6 GiB during the kill loops after the incremental
cache was removed. The remote directory (sources, target, scratch, logs) was deleted afterwards.

## Commits

`6c404b8` (the code, tests and DESIGN) and the commit of this report, both signed, on `p1/jcut` (pushed; not merged).
