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

## Review round (MERGE AFTER FIXES: the Fable review of 6c404b8/7dcf169)

Fix commit `2541d6a`.

| finding | what changed | test (fails with the fix removed) |
|---|---|---|
| F1 | `reclaim` resets a freed file's counters **before** the slot goes on the free list, under the ring lock (it was after, outside it: a lane woken by T4 could fill the file before the reset and seal it with `max_seq` 0). `lane.rs`: a closing lane clears its wake fd in `Shared::wakes` | `a_lane_that_fills_a_just_freed_file_keeps_its_counters` (one lane, three files, the lane fills the file freed first while the reclaim is parked in the hook): with the old order the next reclaim, asked for the old cut, frees 2 files instead of 1, one holding records above the cut |
| F2 | the cut's time ends at C9 (`Coord::cut_end`, before the C10 GC step); a cut without journal bytes (forced FULL, GC moves) is not pushed into the drain history; the budget is at least one ring file of journal bytes (new `JournalFill::file_bytes`, `EngineConfig::cut_min_bytes` overrides it for tests, `None` in the daemon), also with no sealed file | `a_cut_is_timed_to_c9_not_to_the_gc_step_after_it` (11000 ms instead of 1000), `cuts_without_journal_bytes_do_not_dilute_the_drain_rate` (1453 instead of 1800), `a_cut_is_at_least_one_ring_file_of_bytes` (a 50 ms target against a 1 s cut, and a target of 0) |
| F3 | `journal_full` is cleared before the reclaim, so a write that parks again while the reclaim goes on keeps it; the back-to-back rule is also hot while `journal_full` or ring waiters > 0, not only above the total fill trigger | `one_parked_lane_among_several_gets_back_to_back_cuts_below_the_fill_trigger` (four queues, one writing and parked, ring nearly empty, a backlog behind a bounded cut). The flag order is argued, not tested: the fake journal has no per-file reclaim to park a write in |
| F4 | the bounded model no longer takes its durability floor from the recovered tip: before a process ends (crash included) it reads `Engine::durable_cut()` of that process (stored at C6, a `#[doc(hidden)]` accessor) and requires the recovered device to hold the log up to it | the model itself; with the bound removed the partial-cut count assertion fails as before |
| F5 | the half-ring ceiling is in record bytes: half the ring's bytes scaled by `journal_charged / fill.used_bytes` (a ring file holds fewer record bytes than its length: batch padding, headers) | `the_first_cut_after_an_attach_over_a_full_ring_is_bounded_by_the_ceiling` (after a restart, a 3 800-record ring, files holding half as many record bytes as their length through the test wrapper's `fill_scale`: the first cut is 948 records), `a_cut_is_never_more_than_half_the_ring` (fails with the unscaled ceiling) |
| F8 | explicit lockers of the coordinator mutex (`commit_now`, `commit_bounded`, `grow`, the detach commit, the three GC entries) go through `Inner::lock_coord`, which counts waiters; while the hot loop is hot and a waiter exists, `coordinator_wait` is 2 ms instead of 0, so the unfair `std::sync::Mutex` is let go to the waiter | `a_thread_waiting_for_the_coordinator_is_counted` (a commit parked at C3, a second `commit_now` is seen waiting). The 2 ms sleep itself is argued: the hot thread unlocks between ticks and sleeps with the mutex free, so a blocked thread gets it at the next wake-up of the futex; no test of the starvation itself |
| F10 | the back-to-back test runs the thread's loop by hand (`Engine::background_tick`, hidden) on the manual clock and counts: three commits, the last two without a wait; no wall clock | `commits_follow_back_to_back_while_the_ring_is_above_the_fill_trigger` (fails when `coordinator_wait` is never zero) |
| F7 | `commit_bounded` is `#[doc(hidden)]`; `cut_freeing_file`'s doc and both implementations now agree: a sealed file with no record answers 0 (the MemJournal returned the next lane before) | |
| missing test 4 | `full::cut_for_tests`: lanes interleaved, the replayed stream, a floor above `c`, a floor at the first record, records above `c` | |

Each "fails with the fix removed" was run: F1 (the faithful old order: the reset after the slot is free and after the hook), F2 (three removals), F3
(fill-only rule), F5 (unscaled ceiling) and F2's floor (`.max(1)`) each failed the tests named; F8 with the counter not incremented hangs the test until its
20 s bound.

One consequence of F2 that the review did not name: with a floor of one ring file of bytes, "one record" cuts need a smaller floor. The bounded model
sets `cut_min_bytes = Some(1)` and keeps its about-one-record cuts (first run after the floor: 19 partial cuts in 100 cases, below the model's own assertion, which
caught it).

Accepted without a change:
- **F6** (`cut_for` holds all lane mutexes for the merge): once per cut, `O(records x lanes)`, a few milliseconds at the largest budget; a queue thread
  waits in `push` for that long once per cut. The fix (per-lane prefix sums) changes `JournalCharges`' representation for a cost no measurement shows. T6's
  stall profile will show it if it matters.
- **F9** (a discard on a full volume reaches its cut after `backlog / budget` cuts): `space_cut` asks for a cut every `SPACE_CUT_SPACING`, and the floor of
  one ring file makes each cut large; the added delay is bounded by a few cuts. Not changed.
- The CLI's text `lunix status` still shows no write-path line (it is in `--json`): ADR-33 says "in the status", the text layout is T10's with the goldens.

### Gate of the review round

`tools/ci.sh` (CPUs 8-15, `-j 4`, `*_DEBUG=0`, `CARGO_INCREMENTAL=0`) on `2541d6a`: green, ownership `jcut ok (30 changed paths)`. 2 103 tests passed, 0 failed,
28 ignored. (A first run of the same tree failed in `model_16k_one_record_cuts`, which found that the one-ring-file floor turned its one-record cuts into
whole cuts; `cut_min_bytes` is the fix.) origin/main did not move.

Kill loops by hand at the bounded cadence (`LUNIX_KILL_CUT_MS=1`, the child's commits are `commit_bounded`), all on `2541d6a`:

| loop | hits | points | reached | runs / kills | result |
|---|---|---|---|---|---|
| plain 64 KiB | first | 55 | 52 | 52 / 52 | ok (1 801 s) |
| integrity 64 KiB | first | 55 | 52 | 52 / 52 | ok (1 812 s) |
| encrypted 64 KiB | first | 55 | 52 | 52 / 52 | ok (1 805 s) |
| plain 16 KiB | all (first, middle, last) | 55 | 52 | 141 / 141 | ok (8 323 s) |

The 16 KiB first-hit loops of the first round (plain, integrity, encrypted) were run on `6c404b8`, before this round. The HITS=all loops of integrity and
encrypted (16 KiB and 64 KiB) were not run.

Remote disk this round: 2.9 GiB after the gate (no incremental cache), and the remote directory was deleted afterwards.

Commits: `2541d6a` (the fixes) and the commit of this section.

## Second review round (MERGE AFTER FIXES: the re-review of 2541d6a/ca0a50a)

Commits: `7081d8c` (N1-N5 and the nits), `ddc243a` (the ring-full product bug below), and the commit of this section.

| finding | what changed | test |
|---|---|---|
| N1 (blocker) | `stack_kill.rs`: with `LUNIX_KILL_CUT_MS` set the child's engine gets `cut_min_bytes = Some(1)` (the one-ring-file floor, 64 MiB here, had made every `commit_bounded` a whole cut); the child counts the commits and ticks that left requests uncut, the counting run prints it (`partial cuts`) and the loop fails when it is 0; the header comment says so | the loops themselves: 9-10 partial cuts in every loop |
| N2 | the closing lane clears its wake fd before it counts as closed (a successor's wake is never erased) | `a_reopened_lane_is_woken_per_file_after_its_predecessor_closed` (sequential: it checks the successor is signalled and the predecessor is not; the race itself is ordered by the code, not tested) |
| N3 | the one-ring-file minimum is scaled into record bytes by `held / used_bytes` like the ceiling (`cut_min_bytes` stays unscaled) | `the_one_ring_file_minimum_is_in_record_bytes` (fills occupying a quarter of their length: 60 instead of 210 with the unscaled floor) |
| N4 | `wanted` also fires while a write waits for the ring (`ring_waiters > 0`) | `a_tick_cuts_for_a_ring_waiter_after_the_flag_was_cleared` (fails without the clause) |
| N5 | new hidden hook `set_free_hook`: called right after the slot is on the free list, before the lane is signalled | `a_lane_that_fills_a_just_freed_file_keeps_its_counters` now parks there: with the reset placed after the hook call the next reclaim frees 2 files instead of 1 (the first round's original order, a reset directly after the unlock and before the hook, is a window no hook can sit in and is not caught; the test pins that nothing resets after the point where a lane can take the file) |
| nits | the duplicated comment in `journal.rs` removed; the one-file minimum is in `cut.rs`, `EngineConfig::cut_target` and the `cut_target_ms` row of `lunixd/src/config.rs`. `DECISIONS.md` is the lead's | |

### A write stalled for good after a kill and the recovery (found by M15)

Root cause. A process killed between C6 (the FULL is durable) and C9 (the journal reclaim) leaves ring files that hold only records at or below the
tip's `cut_seq`. Recovery replays none of them, so nothing is dirty and `Coord::wanted` returns false before it looks at `journal_full` (its first test is
`last_issued > tip.cut_seq`); and `commit_once` with nothing to cut is `Idle` and does not reclaim either. A lane whose files are all of that kind has no
room: its first write parks for the ring, the coordinator idles in `wait_timeout`, `journal_used` stays at that lane's files, the tip and the commit count stay
constant, and the write fails with EIO after the 60 s ring wait (or hangs longer in a cell that waits). `last_commit_error="other"` is not a failed commit: it
is the label the log line gives when the last commit did not fail for space (`full.rs`, `if nospace { "no space" } else { "other" }`); no commit was attempted.
M15's hypothesis (no trigger) is right, N4's `ring_waiters` clause alone does not cover it (it lives behind the same `dirty` test).

Fix. `Inner::reclaim_covered`, called by every tick: while `journal_full` or a ring waiter exists, `tail_after(tip.cut_seq)` and `reclaim` free the files below the
tip's cut (the FULL is durable, so they may go: DESIGN section 8.2) and signal the queues. It runs before the trigger check, so a dirty lane is helped too.

Tests.
- `jcut::a_lane_left_full_of_covered_records_by_a_crash_is_freed_for_its_parked_write` (engine, fake journal): a single lane filled exactly (504 records in two
  files), flushed, a crash after C6, a restart, a write that parks, ticks: the write completes. Without the fix it never does.
- `stack_ring_recover::a_write_after_a_kill_during_ingest_and_the_recovery_does_not_stall` (real stack, no root; in `tools/ci.sh`): twelve iterations, each on a fresh
  volume of four queues and two 1 MiB files per lane: a child fills every lane exactly with 256 KiB writes, makes them durable and commits; `LUNIX_CRASH_AT`
  kills it inside that commit at `engine.c6.committed`, `engine.c7.checkpoint`, `engine.c7.floor` or `engine.c8.index` (three each); the parent recovers and writes 96
  more times, each within 30 s. Without the fix the test fails after 60 s (EIO of the ring wait); with it twelve iterations take about 15 s. An earlier
  version that killed the child at random moments of a running ingest did not reproduce the stall without the fix (the coordinator keeps the ring from ever being
  full), which is why the kill is placed at the crash points on a deliberately full ring.
- `tamper.sh` itself needs root and the M15 harness, which is not on this branch: not run. The test above is the equivalent, 12 kills, 0 stalls.

### Gate of the second review round

`tools/ci.sh` (CPUs 8-15, `-j 4`, `*_DEBUG=0`, `CARGO_INCREMENTAL=0`) on `ddc243a`: green, ownership `jcut ok (32 changed paths)`. 2 103 tests passed, 0 failed,
28 ignored. `tools/ci.sh` gained the `stack_ring_recover` line.

Kill loops by hand on `ddc243a` at the bounded cadence (`LUNIX_KILL_CUT_MS=1`, `cut_min_bytes = 1`); every loop of the table ran with 9 or 10 partial cuts in its
counting run (commits and ticks that left requests uncut):

| loop | hits | points reached | runs / kills | partial cuts | result |
|---|---|---|---|---|---|
| plain 16 KiB | first | 52 of 55 | 52 / 52 | 9 | ok |
| integrity 16 KiB | first | 52 | 52 / 52 | 9 | ok |
| encrypted 16 KiB | first | 52 | 52 / 52 | 9 | ok |
| plain 64 KiB | first | 52 | 52 / 52 | 9 | ok |
| integrity 64 KiB | first | 52 | 52 / 52 | 10 | ok |
| encrypted 64 KiB | first | 52 | 52 / 52 | 9 | ok |
| plain 16 KiB | all | 52 | 141 / 141 | 9 | ok (8 711 s) |
| integrity 16 KiB | all | 52 | 141 / 141 | 9 | ok (8 576 s) |
| encrypted 16 KiB | all | 52 | 141 / 141 | 9 | ok (7 877 s) |
| plain 64 KiB | all | 52 | 141 / 141 | 10 | ok (7 902 s) |

A previous run of the HITS=all loops on `7081d8c` failed two of four (plain 64 KiB, integrity 16 KiB) in the loop's last coverage assertion, not in a kill:
`journal.mark.after_sync`, a time-driven point, was killed at two distinct hits of its 37 or 39 instead of three (the hit counts of marks differ from run to
run). All 141 kills of every loop had passed fsck and the prefix check. On `ddc243a` all four passed; I treat that assertion as flaky for time-driven points
under four concurrent loops, and did not change it.

Remote disk: 3.1 GiB at the largest sample (no incremental cache); the remote directory was deleted.

## Third review round (the spot check of 7081d8c/ddc243a; MERGE AFTER FIXES, tests and gate only)

Commit `b8841e2`, and the commit of this section. `tools/ci.sh` (CPUs 8-15, `-j 4`, `*_DEBUG=0`, `CARGO_INCREMENTAL=0`) on `b8841e2`: green, ownership
`jcut ok (32 changed paths)`, 2 113 tests passed, 0 failed, 28 ignored.

| finding | what changed |
|---|---|
| R1 | `stack_ring_recover` asserts after `vol.open()` that the ring is full (`used_bytes == capacity_bytes`), the precondition of the stall. With `reclaim_covered` removed the test fails (the first write fails at the 60 s ring deadline), with it it passes |
| R2 | the child records, for every crash point a partial commit or tick passed, the first and last hit number inside one (the crash-point counters before and after it); the loop kills at the first (in `LUNIX_KILL_HITS=all` also the last), requires every `engine.c*` point to be among them, counts the kills at such a hit, and fails when a point passed in a partial cut was never killed inside one. Results below |
| R3 | the success line of `reclaim_covered` is `debug`; the failure `warn` is logged once per distinct error (`Coord::reclaim_error`, cleared on success) |
| R4 | a run that exits before its hit retries at `observed - 1` (not at the observed count), and a point whose count varied between runs needs one distinct kill fewer |
| R5 | the N5 test comment and the sentence of the second round say what the test pins: a reset after the point where a lane can take the file; the original reset directly after the unlock is a window no hook can sit in |
| R6 | the stack test keeps each child's stderr in a file and prints it when the child ends wrongly; four iterations (one per crash point: `engine.c6.committed`, `engine.c7.checkpoint`, `engine.c7.floor`, `engine.c8.index`), about 5 s |
| outside the round | `FileJournal::reclaim`: on an I/O error the failing file and every candidate it had not processed go back to `Used` (they stayed `Recycling` until the next start). Test `a_failed_reclaim_gives_back_every_file_it_had_not_processed`: three sealed files, the second fails through the new hidden `fail_nth_reclaim_file`; the retry frees 2 files, and 1 with the old put-back (run: fails) |

### Kill loops on `b8841e2`

`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first`, `cut_min_bytes = 1`; "partial cuts" are the commits and ticks of the counting run that left requests uncut;
the last column are the kills whose hit lay inside such a commit or tick. Every commit-path point (`engine.c1` to `c9`, and the store, destage and index points
under them: 39 points in all) passed in a partial cut in every loop and was killed inside one.

| loop | runs / kills | partial cuts | kills inside a partial cut | points covered |
|---|---|---|---|---|
| plain 16 KiB | 88 / 88 | 9 | 39 | 39 of 39 |
| integrity 16 KiB | 91 / 91 | 9 | 39 | 39 of 39 |
| encrypted 16 KiB | 88 / 88 | 9 | 39 | 39 of 39 |
| plain 64 KiB | 88 / 88 | 9 | 39 | 39 of 39 |

The first commit of a process is whole (the drain history is empty): hit 1 of the `first` mode is not a partial one, which is why the inside hits are added.
The `LUNIX_KILL_HITS=all` loops, the integrity and encrypted 64 KiB loops and the second-round loops were run on `ddc243a`, not on this head (the head differs by the journal
error path and the log levels); not rerun, as asked.

Remote disk: below 3.2 GiB (no incremental cache); the remote directory was deleted.
