# Report p1-bstop: ADR-32 T1, T2 and T6 (the bounded stop, the discard path, a default cap)

Branch `p1/bstop` from `origin/main` 3e6fb81. Spec: ADR-32 (accepted 2026-11-18, M1-M9) and
`docs/explanation/research/2026-11-18-write-backpressure.md` (§2, §5.5, §9, §10, §11). FREEZE rule
change F1 implemented (on-disk-formats.md §21 row; no golden changes). Builds and tests ran only on the
remote test host (kernel 7.0.0) through `tools/remote.sh`, CPUs 8-15, `-j 8`. No root run.

## What was built, per path

| path | fix | where | tests |
|---|---|---|---|
| P1 no default deadline | a null `space.full_timeout_ms` resolves to the daemon's `full_timeout_ms` (new `lunixd.toml` key, default 60 s, 1000 ..= 3 600 000, a warning above 100 s); every parked request carries `arrived` and a deadline. The first expiry fails every request waiting for space and **latches the volume full**: writes fail at once (`NoSpace`, answered `ENOSPC`) until `limit - used` reaches the resume mark (max(4 segments, 1 % of the limit), at most half the limit) | `lunix-space` `policy.rs`, `ledger.rs`, `gate.rs`; `lunixd` `config.rs`, `stack/mod.rs`, `main.rs` | `bounded.rs`: `a_null_full_timeout_is_the_daemon_default_of_60_s`, `the_daemon_default_can_be_set_and_volume_json_wins_over_it`, `the_latch_refuses_at_once_and_opens_at_the_resume_mark_not_below`, `the_resume_mark_is_four_segments_or_one_percent_and_at_most_half_the_limit`, `on_full_error_refuses_at_once_and_does_not_latch`; `behaviour.rs`: `an_expired_head_fails_the_requests_behind_it_and_latches_the_volume_full`; `model.rs` (5 000 sequences, now with deadlines, the latch, re-judging and `begin_close`; after every step no request is pending past its deadline); stack: `e1_parked_writes_fail_with_enospc_within_full_timeout_and_the_volume_latches` |
| P2 no re-judging | `reconfigure` re-judges: deadlines from each arrival with the new timeout, grant what fits (a larger `cache_max` grants and opens the latch), then `on_full = error` fails the rest now, else the overdue ones expire (and latch). `lunix space` also sets the engine's ring-full timeout | `ledger.rs` `rejudge`, `gate.rs`; `daemon.rs` `space` | `reconfigure_to_error_fails_every_parked_request_now`, `a_new_full_timeout_counts_from_each_arrival`, `a_shorter_full_timeout_fails_the_overdue_requests_at_once`, `a_larger_cache_max_grants_the_parked_and_opens_the_latch`; engine `a_ring_wait_follows_a_live_change_of_full_timeout`; stack `e1_a_switch_to_on_full_error_releases_the_parked_writes_at_once` |
| P3 quiesce cannot drain | `LunixSpace::begin_close` (M5): grants what fits, fails the space waiters, parks nothing after. `QuiesceAll` calls it right before each device's `quiesce` | `gate.rs`; `daemon.rs` `quiesce_all` | `begin_close_grants_what_fits_fails_the_rest_and_parks_nothing_after`; stack `e1_the_quiesce_rule_fails_the_parked_writes_and_the_final_commit_runs` (non-root: the ublk drain itself is the M15 full-volume cell) |
| P4 discard behind parked writes | DISCARD and WRITE_ZEROES reserve their 64-byte records with `reserve_headroom` (never park), queue only behind other hole requests parked for the ring (`parked_holes`), never behind parked writes. T2: while writes are refused or parked for space, the coordinator cuts hole records at most once a second, then writes `retain_last + 1` FULLs, one a second (the window turn, M6) | `lunix-engine` `queue.rs`, new `full.rs`, `coord.rs` (`tick`: 6 lines) | engine `discards_and_write_zeroes_overtake_a_write_parked_for_space`, `a_discard_is_admitted_from_the_reserve_on_a_refusing_volume`, `a_full_volume_cuts_its_discards_and_turns_the_retained_window`, `a_volume_that_admits_writes_does_not_cut_for_a_discard`; stack `a_discard_on_a_full_volume_brings_the_space_back_and_writes_succeed` (latched 64 MiB-cap volume, discard of all written data, a write succeeds 19.8 s later, 66 MB freed, fsck clean) |
| P5 deadlines on the ladder's thread | two threads: `lunix-space-timer` (expiry, refresh, ladder re-asks; only the gate's lock and the probe) and `lunix-space-ladder` (the steps, may block). The ladder's `want` is every waiting reservation plus the resume mark less the headroom. The ladder's urgent pass (`run_urgent`) backs off: 2 s after the last urgent pass of either path, doubling to 60 s while it frees nothing, at most the floor grace after one that condemned, 2 s after one that freed; a successful commit does not reset it | `gate.rs`; `lunix-engine` `gc.rs`, `full.rs` | `a_blocking_ladder_step_delays_no_deadline`, `the_ladder_wants_every_waiting_reservation_plus_the_resume_mark`; `gc_control.rs` adjusted (below) |
| P6 ring-full waits | a write refused by a full ring fails at once when the engine is stopped or read-only (`EIO`), else after `full_timeout` from its arrival (`ENOSPC` if the last commit failed for space, else `EIO`); the background coordinator wakes the queues every period while the ring is full; an engine failure or a poison signals the queues. A stopped or read-only engine fails every parked request (gate- or ring-parked) at once; a gate waiter that expired on a read-only volume is `EIO` | `queue.rs`, `full.rs`, `engine.rs`, `coord.rs` (two `signal_queues` lines) | engine `a_write_parked_for_a_full_ring_fails_at_its_full_timeout`, `a_stopped_engine_fails_the_writes_parked_for_the_ring_at_once`, `a_stopped_engine_fails_the_writes_parked_for_space_at_once`, `a_read_only_volume_fails_its_parked_writes_with_eio`, `a_waiter_expired_for_space_is_enospc` |
| T6 default cap (M2) | daemon `create` writes `space.cache_max = store_max_percent × size` (new `lunixd.toml` key, default 200, 0 or 100 ..= 10 000), at least size + 2 × `commit.dirty_bytes`; `lunix volume create --store-max-percent N` overrides (0: no cap); documented in the help text and on-disk-formats §21 | `daemon.rs`, `config.rs`, `lunix-cli` `cli.rs`, `cmd.rs`, `lunix-iface` `ctl/msg.rs` | `a_new_volume_gets_twice_its_size_as_cache_max_by_default` (daemon unit), `ctl.rs` validation cases, help snapshot |

**Which path E1 took.** P1 first: the store was at its limit with nothing to reclaim, the head write
parked with no deadline (`on_full = wait`, null timeout) and 128 requests queued behind it. Then P3: the
restart was a quiesce, which drains only when no started request is held; nothing completed the parked
one, `QUIESCE_DEV` timed out and the device refused I/O, so only a kill and a forced delete ended it.
`on_full = error` took effect only after that kill (read at open), so P2 did not apply. The stack
tests replay each exit: the deadline (P1), a live switch (P2), the quiesce rule (P3) and a kill with
`on_full = error` written before the reopen (each reissued write answered in under a second, nothing
parks, at least 12 of 16 `ENOSPC`).

## Gate

`tools/remote.sh run bash tools/ci.sh` on the host, green including ownership (see "Test counts").

### Test counts

New: `lunix-space/tests/bounded.rs` 12, `lunix-engine/tests/full.rs` 10, `lunixd/tests/stack_throttle.rs`
5 (release, in ci.sh's stack list), one daemon unit test, four `ctl.rs` validation asserts. Changed:
`behaviour.rs` (one test replaced, see deviations), `model.rs` (the reference model follows ADR-32),
`gc_control.rs` (one clock advance), the `VolumeCreate` literals in five test files. Full `ci.sh`:
**1853 passed, 0 failed, 27 ignored** (summed over every `test result` line, all steps), `ci: all
green`, `check-ownership: bstop ok (35 changed paths)`.

## Deviations from the note

1. **`begin_close` is called by the daemon, not by `Engine::quiesce`.** The ublk layer calls
   `engine.quiesce()` after the drain, so a call there is too late; an engine hook before the drain
   would be a `BlockEngine` change. `QuiesceAll` calls `space.begin_close()` before `device.quiesce`.
   The close is one-way: a quiesce that fails leaves the gate refusing instead of parking (the device
   refuses new I/O after a `QUIESCE_DEV` timeout anyway).
2. **The stop and full states are read in the engine without a new snapshot field** (I2 is T5's): a
   queue-side flag (a reservation parked, refused or expired, none admitted since) or the gate's waiter
   count. No `lunix-iface` trait or `SpaceSnapshot` change.
3. **`on_full = error` does not latch**: it refuses per request as before ("the latch without the
   wait" read as "no wait"); the latch is entered only by an expiry.
4. **The resume mark is clamped to half the limit** (the note has no clamp; a 64 MiB test cap would
   otherwise never reopen).
5. **The ladder's back-off is its own** (`FullState::ladder_backoff`), sharing the last-pass time with
   `space_short`: `space_short`'s pause is reset by every successful commit, which would make the
   ladder run a whole mark every 2 s on a volume that keeps committing.
6. **The default cap has a floor**: at least size + 2 × `commit.dirty_bytes`, so a small volume's first
   fill (up to `dirty_bytes` in the journal, counted twice during the cut) does not reach the cap. For
   volumes above 2 GiB at the defaults this is exactly 200 %.
7. **No log lines in `lunix-space`**: it has no `tracing` dependency (a dependency change for the
   lead). The engine logs the window turn and the ring-full expiry (rate-limited); the gate's
   transition lines are T5.
8. **A hole record that finds the reserved region exhausted fails `ENOSPC`** instead of parking.
9. **An expiry fails every space waiter, also those that would fit** (per §5.5); the old behaviour test
   `an_expired_head_unblocks_the_smaller_request_behind_it` asserted the opposite and was replaced by
   `an_expired_head_fails_the_requests_behind_it_and_latches_the_volume_full`. `gc_control.rs`'s
   `a_cut_asked_for_by_an_urgent_pass_is_one_full_on_an_idle_volume` now advances the manual clock past
   the ladder back-off before its second `run_urgent` (otherwise the call is skipped and the assertion
   holds vacuously).
10. **Stack test sizes**: a 128 MiB volume with a 64 MiB cap, discarding everything written (the note
    says half the working set), small journal ring files (finding 1).
11. `DESIGN.md` (§2, §4.3, §18, §19), GLOSSARY and the plan's gates are not edited: outside this
    milestone's paths; `docs/reference/gc.md` (ladder back-off) and on-disk-formats §21 are.

## Finding for the lead (not fixed here)

1. **`JournalReserved` comes back a whole ring file at a time.** `FileJournal::reclaim` frees only used
   files whose ordinal is below the tail; the active file of each lane (64 MiB, `FILE_LEN`) keeps its
   records charged after a cut covers them, until it rolls. On a latched full volume no write is
   admitted, so it never rolls: up to `lanes × 64 MiB` of the cap cannot be won back by a discard or
   GC. The first stack run of the discard test showed it (cap 64 MiB, `JournalReserved` 60.6 MB after GC
   had freed every segment); with 1 MiB ring files the same test passes in 20 s. It matters for
   small caps and many queues, and it can keep a volume latched. Options: account the journal per cut
   (release what a cut covers at C9, the file reuse being the journal's business), or roll the active
   files at a cut while the gate is not open. Owner: `lunix-journal` (and the accounting walk-through
   in `lunix_iface::space`); a candidate for T3.

## Interface changes (lunix-iface), for sign-off

- `ctl::VolumeCreate` gains `store_max_percent: Option<u32>` (`#[serde(default)]`), validated in
  `Request::validate` (0, or 100 ..= 10 000).
- `SpaceGate::reserve_since(kind, bytes, wake, arrived: Duration)`, a **default method** (it calls
  `reserve` and ignores `arrived`), added for review M1: a parked request's `full_timeout` counts from
  `arrived`. `LunixSpace` and the testkit's `MemSpace` implement it; no implementer has to change.
  `SpaceSnapshot`, `BlockEngine` and `GcControl` are unchanged.

Public API of implementation crates (not iface): `lunix_space::{DEFAULT_FULL_TIMEOUT,
SpaceConfig::full_timeout, LunixSpace::{begin_close, is_full, fulls, resume_mark, full_timeout}}`;
`lunix_engine::{EngineConfig::full_timeout, Engine::{set_full_timeout, window_turn_left,
begin_close}}`;
`lunixd::stack::OpenParams::full_timeout`; `lunixd.toml` keys `full_timeout_ms`, `store_max_percent`.

## Left for T3-T5

- T3: the paced band (curve, bucket, delay cap), the FUA class (I1), the `throttle*` `lunixd.toml`
  keys, the `gc_pressure_rate_factor` key; finding 1.
- T4: GC pressure from the engine tick (budgets, rate, floor grace by gate state).
- T5: the `throttle` status object and `SpaceSnapshot` state (I2, I3), the gate's log lines (needs
  `tracing` in `lunix-space`); then the engine's stop/full test of deviation 2 can read the snapshot.
- DESIGN §2, §4.3, §19, §20 and GLOSSARY text; the plan's M8/M15/M16 gates (T7).

## Merge notes (p1/gcrate)

`tools/remote.sh` carries gcrate's id check verbatim (identical hunk). Expected small conflicts:
`tools/ownership.toml` (both entries before `[m15]`), `tools/ci.sh` (stack list: add both
`stack_gc_rate` and `stack_throttle`), `coord.rs` `tick` (6 lines) and two `signal_queues` lines,
`engine.rs` (one field, the background loop, two methods), `stack/mod.rs` (the `SpaceConfig` and
`full_timeout` lines).

## rust-skills conflicts

None. Atomics use `Relaxed` where they are hints (`conc-atomic-ordering`), `AcqRel`/`Acquire` for the
hole sequence the coordinator compares against the tip.

## Commits

One signed commit on `p1/bstop`, no trailers: `bstop: ADR-32 T1, T2, T6: bounded stop, discard path,
default cache_max`.

## Remote disk use

15 GiB under the milestone's host directory after removing the incremental caches (target: debug
about 13 GiB with the several feature sets ci.sh builds, release 1.7 GiB). **Over the 10 GiB budget**;
the target can be deleted at any time (the next gate rebuilds it).

## Review response (01-p1-bstop-review.md, verdict "merge after fixes")

Every finding is fixed on `p1/bstop`; the second commit carries them. Where the text above and this
section differ, this section holds. Gate after the fixes: full `tools/ci.sh` on the host, **1874
passed, 0 failed, 27 ignored**, `ci: all green`, `check-ownership: bstop ok (42 changed paths)`.
The remote target was cleaned afterwards (`cargo clean`).

| finding | fix | test |
|---|---|---|
| **M1** a queued write got a fresh `full_timeout` at the gate | new default iface method `SpaceGate::reserve_since(.., arrived)`; the queue passes `Mutation::arrived`; `LunixSpace` computes `deadline = arrived + full_timeout` and expires (and latches) at once a request already past it; `MemSpace` follows | engine `writes_queued_behind_a_parked_one_wait_at_most_full_timeout_from_their_arrival` (three writes on one queue: each ends at its arrival + 5 s); space `a_request_that_waited_in_its_queue_counts_its_wait_from_its_arrival` |
| **M2** the window turn armed only in a blocked tick | no counter: `FullState::hole_commit` is the `commit_seq` of the last commit that took a hole record, set in `cut_and_commit` for every commit (any caller); at open it is the tip and the highest replayed hole record is noted, so the first cut after a restart sets it. While refusing, `space_cut` owes a forced FULL while fewer than `retain_last + 1` commits followed it | `the_window_turns_when_a_normal_commit_cut_the_discard_before_the_stop` (cases 1-3, `error` and `wait`), `the_window_turns_after_a_restart` (case 4), `a_full_volume_cuts_its_discards_and_turns_the_retained_window` |
| **M3** ring-parked writes outlived a quiesce | `Engine::begin_close()` (inherent), called beside `space.begin_close()` in `quiesce_all`: a ring wait answers at once when a commit attempt fails after the close (`ENOSPC` if for space), else within `CLOSE_RING_BOUND` (5 s) | `begin_close_answers_a_ring_wait_once_a_commit_fails_for_space`, `begin_close_answers_a_ring_wait_within_its_bound_when_no_commit_fails`; the stack quiesce test calls both |
| **M4** `JournalReserved` of the active ring files | the engine keeps every record's charge by `write_seq` (a FIFO per lane, one for the replayed records) and releases at C9 every charge at or below the cut; the journal's `ReclaimReport::bytes` is no longer used for the ledger. No journal code or format change. The ledger now equals what a restart charges | `journal_charges_are_released_per_cut`; `stack_throttle.rs` now runs on the production 64 MiB ring files and the restart test asserts 16 of 16 refused (was 12) |
| **M5** a discard refused below the reserve | hole records are charged with `SpaceGate::charge` (no limit check); deviation 8 is gone | `a_discard_is_admitted_with_no_reserved_region_left` |
| **M6** a grow kept the old cap | `resize` raises a **default** cap to the default for the new size; a cap is the default when it equals `default_cache_max(old size, daemon store_max_percent, dirty_bytes)`, which is what `create` wrote; any other value (another `--store-max-percent`, a `lunix space --cache-max`) is explicit and stays; no cap stays no cap. A warning when the effective percentage is below 200 (create and daemon start) | `control.rs`: the grow raises the cap, a later explicit cap survives the next grow |
| m1 | `limit == 0`: the latch opens only with at least one byte of headroom. The engine logs the first refusal of an episode with `used` and `limit` (the gate has no `tracing`; its lines stay T5). Compacting below `live_threshold` under the latch is T4's | `at_a_zero_limit_the_latch_stays_closed` |
| m2 | a ring wait lasts `max(full_timeout, 60 s)` while commits succeed, `full_timeout` once a commit attempt failed since the write arrived | `a_ring_wait_with_commits_running_lasts_at_least_60_s_then_fails_eio`, `a_ring_wait_fails_enospc_at_full_timeout_after_a_commit_failed_for_space` |
| m3 | "refusing" is the gate's waiters, or the queue flag confirmed by `used + 1 MiB > limit`; the turn no longer depends on the flag | the M2 tests; `a_volume_that_admits_writes_does_not_cut_for_a_discard` |
| m4 | the timer asks the ladder while latched | `the_timer_asks_the_ladder_while_the_volume_is_latched_with_no_waiter` |
| m5 | the ladder thread pauses 100 ms after a run that freed nothing, whatever asks meanwhile | `refused_writes_in_a_loop_do_not_spin_the_ladder` |
| m6 | the ladder back-off returns to 2 s when a write is admitted after a refusal | (one line in `set_blocked`; covered by the stack discard test) |
| m7 | `commit_nospace` stays true until a commit succeeds | the ENOSPC ring test (a `Later` failure after the space one) |
| m8 | each queue publishes `ring_parked`; the coordinator wakes only those queues | (behavioural; the ring tests) |
| m9 | a switch to `on_full = error` opens the latch | `a_switch_to_on_full_error_opens_the_latch` |
| m10 | gone with M5 | |
| m11 | doc line on `EngineConfig::full_timeout` | |
| tests | P6 `ENOSPC` and the ring-parked hole `ENOSPC` branches through a test-destager hook (`Retry::AfterSpace` now fails with `NoSpace`, `tests/common/mod.rs`); `stack_throttle.rs`: `full_timeout` 10 s, a park counts only after 2.5 s (a cut in flight double-counts for a moment), every "at once" bound is 5 s, the deadline is checked from both sides | |
| deviation 11 | DESIGN §2, §4.3, §19 and the GLOSSARY "Space ladder" now describe the bounded wait and the latch | |

Updated deviations: 2 (the flag is now confirmed against the ledger), 6 (the cap's floor needs no
ring-file term once M4 is fixed), 8 (gone), 10 (the stack tests use the production ring files),
11 (done). The journal finding is fixed in the engine as the review recommended.

## Re-review response (01-p1-bstop-rereview.md, verdict "merge after fixes")

The third commit carries these fixes. N2 is left as the lead asked (a maintainer decision on a
`volume.json` change is pending); only its warning is added.

| finding | fix | test |
|---|---|---|
| **N1** no window turn inside the latch band | `SpaceSnapshot` gains `full: bool` (the latch, refreshed at each snapshot); `space_refusing` is `waiters > 0 \|\| full \|\| (flag && used + 1 MiB > limit)`. `MemSpace` gets an opt-in latch (`MemSpaceConfig::latch(resume)`) | engine `the_window_turns_inside_the_latch_band` (latched, 1.5 MiB freed, headroom between 1 MiB and the mark, discard, cut and turn, the extents go, the latch opens, a write goes in) |
| **N2** the grow rule infers the default | unchanged, pending the maintainer. A grow that keeps a cap below `new size + 2 × commit.dirty_bytes` now logs a `warn` saying the volume will refuse writes before it is full | |
| **N3** fixed 5 s ring bound at quiesce | `Engine::begin_close(ring_bound)`; the daemon passes `close_ring_bound(quiesce_timeout)` = two thirds of it, at least 1 s. Only `NoSpace` and `Io` commit failures count, before and after the close; `Busy` and the like never shorten or end a ring wait. The one-way close is documented | `a_slow_but_healthy_cut_at_quiesce_is_waited_for` (10 s of a 20 s bound, then the cut lands: success); unit `only_space_and_io_failures_count_for_a_ring_wait`; daemon unit `the_ring_bound_at_quiesce_is_two_thirds_of_quiesce_timeout` |
| `reserve_since` | **required** (no default); implemented by `LunixSpace`, `MemSpace`, fsck's `OfflineSpace` and the iface's `TSpace`. A contract case `a_wait_counts_from_the_arrival` runs against every gate that has the suite | contract (`lunix-space`, testkit, trivial) |
| m1 | a request past its `full_timeout` on first contact with the gate (it waited in its queue or for the ring) is refused `NoSpace` without parking and **without the latch** | `a_request_that_waited_in_its_queue_counts_its_wait_from_its_arrival`, the contract case |
| m2 | with N3 | |
| m3 | the "refused" line is rate-limited (one per 5 s); the ladder back-off starts over only after writes were admitted for 5 s, not on every flap | unit `the_ladder_back_off_starts_over_only_after_writes_were_admitted_a_while` |
| m4 | with `reserve_since` above | |
| m5 | iface walk-through step 5, `ReclaimReport::bytes` and `Inner::journal_charged` docs, `lunix-space` crate docs and walk-through test comment now say "per cut" | |
| m6 | DESIGN §4.3 says a restart of a refusing volume turns the window once and its rollback points become empty FULLs | |
| m7 | `push` has a `debug_assert!` on the lane; `take_upto` splits the backlog off under the lane mutex and sums outside; the memory bound is documented | |
| m8 | `enospc >= 15`; the deadline test's lower bound allows for `PARKED` | |
| m9 | `volume.json` is stored before the gate is reconfigured | |

Also in this round, a pre-existing flake outside the milestone's code: `wiring.rs`
`journal_damage_is_reported_and_wiped_and_refuse_keeps_it` and
`a_torn_last_record_is_crash_residue_under_refuse` fail when the host is slow between a FLUSH and the
next write for more than the journal's 50 ms `mark_idle`: a journal mark takes a `write_seq` and
shifts the numbers they assert. It failed 2 of 5 runs of `cargo test --workspace --test wiring` on
`origin/main` 3e6fb81 as well (measured on the host) and three CI runs of this round in a row.
Fixed as a test bug: both write their records with `open_unmarked` (marks off), the helper the R4
tests already use for the same reason (`wiring.rs`, `tests/common/volume.rs`, added to `[bstop]`);
then 5 of 5 runs of `wiring` and `logging` passed.

Gate of this round: full `tools/ci.sh` on the host, `ci: all green`, 1868 passed, 0 failed, 27
ignored (summed over the `test result` lines; parallel test output interleaves, so the sum is
approximate), `check-ownership: bstop ok (50 changed paths)`. The remote target was deleted
afterwards.

## Follow-up p1/bstop-fu (01-p1-bstop-final-review.md, verdict MERGE)

Cut from `main` 871e187. It also carries N2, decided by the maintainer on 2026-11-18 ("Store % in
volume.json", FREEZE sign-off with the choice; "no need for backwards compatibility").

| finding | fix | test |
|---|---|---|
| m-A | `set_blocked(true, now)` resets the ladder back-off to 2 s when writes had been admitted for 5 s, before it zeroes that fact | unit `a_new_episode_after_a_long_admitted_stretch_starts_at_the_minimum` |
| m-B | `close_ring_bound` is two thirds of `quiesce_timeout` with no floor: always strictly inside it | the daemon unit test checks 600 ms, 1 s and 3 s |
| m-C | the contract's late-refusal step runs on a fresh, unlatched gate and asserts it stays unlatched with no waiter | contract `a_wait_counts_from_the_arrival` |
| m-D | `full.rs` module doc names `SpaceSnapshot::full`; `note_refused` has its own doc comment again; the long doc line is wrapped | |
| m-E | `model.rs` checks `LunixSpace::snapshot().full` against the model after every step | the 5 000-sequence model test |
| m-F | the `full.rs` module doc says a `MapFull` or `Corrupt` failure does not shorten a ring wait, and what the wait then is | |
| N2 | `volume.json` gains the required key `space.cache_max_percent` next to `space.cache_max`; at most one is non-null. `create` writes the percentage (`store_max_percent`, 200 by default; none for 0). `lunixd::stack::effective_space` resolves it against the size (percent of the size, at least the size + 2 × `commit.dirty_bytes`) for the gate, at open (after the engine knows the size), on `lunix space` and on every grow; the gate refuses an unresolved percentage. `lunix space --cache-max` writes an absolute cap and clears the percentage; an absolute cap stays on a grow, with the warning when below the new size + 2 commits. A grow changes nothing in `volume.json` (it keeps the percentage), so there is nothing to store before the gate is reconfigured. `VolumeJson::from_json` refuses a file without either key (no serde default, no migration). Golden `14-volume.json` gains `"cache_max_percent": null`; on-disk-formats §21 rows; FREEZE.md rule change F2. `lunix space` prints "N % of the size" | `f14_local.rs` (range, both set, either key missing); daemon units `a_percentage_cap_follows_the_size_and_an_absolute_one_stays`; `control.rs` (create writes 200 %, the limit follows the grow, an absolute cap replaces it and stays on the next grow) |

Gate of the follow-up: full `tools/ci.sh` on the host, `ci: all green`, 1892 passed, 0 failed, 27
ignored (summed `test result` lines), `check-ownership: bstop ok`. The remote target was deleted
afterwards.
