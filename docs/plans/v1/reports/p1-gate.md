# p1-gate: the pacing delay counts from a write's arrival (part 1 of the M15 latency fixes)

Branch `p1/gate` on main `e675b2f` (with T8 / seal). Remote: Linux 7.0.0, CPUs 0-7, `-j 8`. No on-disk format change, no interface method
changed (doc comments in `lunix-iface/src/space.rs`), `WritePathStatus` untouched. Part 2 (collector sizing) is parked, see the last section.
`docs/explanation/DECISIONS.md` is not edited: the proposed ADR-32 amendment is below.

## What was found

At `80e1cd1` the cap was already counted from a write's arrival for every write that reaches the gate (the engine passes `Mutation::arrived` to
`reserve_since`; `cap_at(arrived)` and the grant loop use it), so a write behind others in its queue is admitted at once when it reaches the front
past its cap. The defects were:

* the figures (`delay_max_ms`, `delayed_ms`, `delayed_writes`) counted from the moment the gate parked the request, and not at all for a write
  admitted at first contact after standing in its queue: the status said 0.2 to 0.9 s while a write had waited 10 s;
* a due write that reached the gate behind a younger head was admitted only at the next timer tick;
* the timer woke at the head's tokens or cap, not at the cap of a request behind it.

The stack test, run on `80e1cd1`, shows it: slowest write 10.0 s (the cap), `delay_max_ms` 922.

## Changes (`crates/lunix-space/src/ledger.rs`)

* `note_delay` counts from `arrived` (the `parked_at` field is gone), for waits for tokens and for space.
* `note_direct_delay`: a write admitted at first contact **while the pace is active** (tokens or cap) after standing at least 1 ms in its queue
  is a delayed write, in the space figures, or in the ring pace's when only it paces. Not counted: `Retry`, `Sync` (FUA bypass), and any
  admission when nothing paces (review finding 3: a wait behind a ring-parked head, a slow append or an earlier record of one large request
  is not the gate's delay).
* `park`: a request that fits and is past its cap on first contact is granted at once with what stands before it, not at the next tick.
* `next_wake`: also wakes at the cap of the requests that fit, scanned like `due_behind` (up to the first request that does not fit, whatever its
  state; review finding 8).
* Token accounting is unchanged (debt at most one second of the rate) and now tested; the ring pace composes the same way (own counters, own cap
  clock), `Retry` pays no tokens.
* Docs: `ThrottleFigures::delayed_writes` and `delay_max_ms` in `lunix-iface/src/space.rs`; one sentence of DESIGN section 4.3.

## Proposed ADR-32 amendment (for DECISIONS.md, the lead's file)

> **Amendment (milestone gate).**
> - **The delay cap counts from arrival, and so do the figures.** `throttle_max_delay_ms` is counted from a write's arrival at its queue, the
>   time it stood behind other requests of the queue included. A request that reaches the gate past its cap is admitted at once, together with
>   what stands before it; the timer wakes at the earliest cap among the requests before the first one that does not fit. A write is not held
>   by the **pace** longer than `throttle_max_delay_ms` plus its own service; a write that waits for space (a request behind one that does
>   not fit) still waits up to `full_timeout`. `delay_max_ms`, `delayed_ms` and `delayed_writes` count from arrival: for requests that waited
>   in the gate, and for requests admitted at first contact after standing in their queue while the pace is active (under 1 ms is not a delay).
>   With nothing pacing, a wait in a queue (behind a ring-parked request, a slow append) is not a gate delay and is not counted; `Retry` and FUA
>   in the band are not counted either.
> - Unchanged: past the cap a request is admitted if it fits and the bucket carries the debt, at most one second of the rate. Above the cap's
>   reach (`depth x size / rate` over the cap) the admitted rate is `depth x size / cap` and the hard mark does the pacing. The ring pace composes
>   the same way; a `Retry` with a fitting head pays no tokens.
> - **Known limit (not changed).** The cap counts the wait for space too: after a stop of 10 s or more every queued write is past its cap and is
>   admitted at once when headroom returns, the debt clamped at one second of the rate. Counting from `max(arrived, the gate's last exit from
>   stop)` is a separate decision.

## Tests

| rule | test | fails when |
|---|---|---|
| figures from arrival; N writes behind a slow head: none paced beyond the cap | `pace.rs::n_writes_queued_behind_a_slow_head_none_waits_beyond_the_cap` (4 queues x 40 writes at 64 KiB/s, cap 5 s: longest <= cap + 10 ms, `delay_max_ms` 4990..=5020), `a_write_that_reaches_the_gate_late_is_counted_from_its_arrival` | origin moved to first contact: 10 of 34 pace tests fail |
| direct admission after a queue wait under the pace is a delay | `a_write_that_stood_in_its_queue_and_is_admitted_by_tokens_counts_the_wait` | `note_direct_delay` a no-op: fails |
| **no delay counted at an open gate (finding 3)** | `a_write_that_stood_in_its_queue_at_an_open_gate_is_no_delay_of_the_gate` (arrived 3 s ago, Bulk and Sync: figures stay 0) | call restored in the no-pace branch: it fails (34 passed, 1 failed) |
| a due write is admitted at first contact | `a_due_write_that_stands_behind_a_young_head_is_admitted_on_first_contact`, `past_the_cap_the_rate_rises_to_depth_over_cap_and_no_further` | grant in `park` removed: both fail |
| the timer wakes at a request behind the head | `ledger::tests::next_wake_is_the_earliest_cap_among_the_requests_that_fit` | `due` removed from `next_wake`: fails |
| debt after cap admissions: one second of the rate, paid | `after_cap_admissions_the_debt_is_one_second_of_the_rate_and_is_paid` | debt unbounded or cap admissions uncharged: fails (with two throttle unit tests / the cap test) |
| the pace holds on average short of the cap | `the_pace_holds_on_average_when_the_queues_are_short_of_the_cap` | invariant test |
| ring pace: cap from arrival, own counters; `Retry` | `the_ring_pace_alone_keeps_the_cap_and_counts_from_the_arrival`, `a_retry_after_a_ring_refusal_pays_no_tokens_and_counts_no_gate_delay` | origin mutation fails the first |
| real stack figure | `stack_throttle.rs::deep_overwrites_report_their_wait_from_arrival` (32 x 256 KiB in flight against a slow band, 25 s; asserts only `delay_max_ms + 5 s >= the slowest wait the test saw`, no duration bound, no stop count; data reads back) | fails on `80e1cd1` (`delay_max_ms` 922 against a 10 s wait) |

The mutation runs (patch on the host copy, tests, revert) for the earlier rules were done in the first round; finding 3's was repeated in this one.

## Gates

Final tree = main `e675b2f` (with T8) + this branch, CPUs 0-7, `-j 8`.

* `tools/ci.sh`: green, `check-ownership: gate ok (7 changed paths)`, `ci: all green`; 2 233 tests passed, 0 failed, 30 ignored (sum of the
  `test result` lines).
* Kill loops by hand, one at a time, on the same code (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`,
  `cargo test --release -p lunixd --features testsupport,crashpoints --test stack_kill`):

| loop | points reached | runs / kills | result |
|---|---|---|---|
| plain 16 KiB | 55 of 58 | 57 / 57 | ok (1 813 s) |
| integrity 16 KiB | 55 of 58 | 60 / 60 | ok (2 040 s) |

Remote disk: the target directory was cleaned before CI (it stayed under about 12 GiB with the debug and release builds of CI; the 10 GiB figure
was exceeded by CI's own debug build, as in the first round). The whole remote directory was removed at the end; nothing of mine is left.

## Deviations

1. The finding-1 diagnosis of the brief ("the cap is counted only from the head") holds for the figures only; see "What was found".
2. `tools/ownership.toml` gained a `[gate]` entry, trimmed to the paths changed.
3. Main was merged into the branch earlier (no force-push); the final tree contains `e675b2f`.
4. The end-to-end M15 check (8G cap, xfs 16K integrity, `128:8`, two runs per build, all cells passed) showed stops 2 and 0 before, 1 and 6 after
   with part 2 included; it did not show a gain and is not evidence for part 1 either. The first stop in each run is at band entry with
   `reclaim_rate=0`.

## Parked: part 2 (collector sizing)

Branch `p1/gate-gc` (`6bc041c`) keeps it unchanged: `gc_demand` and `gc_write_amp_percent`, budget and rate sized to the ingest, the share after
a cut (all of the cut, at most 2 s), `lunix-gc` `per_pass_bytes`, the no-yield zone (`near_band`), `MemSpace::set_throttle_figures`,
`gate_equilibrium.rs` and its jstall and DESIGN text. Review findings against it:

1. **HIGH, no-yield zone** (`headroom < 2 x soft`): undoes the ADR-33 GC yield for limits up to 2 GiB and above 70 % full otherwise, the regime
   of the T6 ring stalls; no hysteresis; not measured with ring-wait figures. If wanted, a narrower zone with hysteresis (below `soft + soft/4`,
   released above `soft + soft/2`) and the M16 write cells as the gate.
2. **HIGH, per-pass cap lifted** with no bound on the space compaction takes from the cut (`u64::MAX` at stop and full; 1 GiB already at the
   floors in the band): a pass can fill the reserved region while no source is deletable for the grace. Keep `compact_per_pass`, or bound it by
   the free reserved region; shorten the gap between passes instead.
4. **MEDIUM, no evidence**: stops 2, 0 against 1, 6; the sizing did not act in the cell (demand under the floors); `ingest_rate` is the admitted
   rate, so the demand shrinks deep in the band; the doubled share takes coordinator time from the cuts. Only `gc_demand` with
   `gc_write_amp_percent` is inert at the defaults.
   (Finding 6: `gate_equilibrium.rs` proves its own model; 9: stale `docs/reference/gc.md` and `coord.rs` comments.)

**The lead-zone idea**: the measured stop is the grace dead time at the first fill (`reclaim_rate=0` at band entry; `L = R x tau`). Start the
pressure and the floor grace before the band, from about `soft + ingest x (tau_floor + pass period)` of headroom, so garbage condemned earlier
is deletable earlier; needs its own milestone with M15 and M16 measurements over enough runs.
