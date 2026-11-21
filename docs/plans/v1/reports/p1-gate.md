# p1-gate: two space-gate fixes from the M15 latency diagnosis

Branch `p1/gate` from main `80e1cd1`. Remote: Linux 7.0.0, CPUs 0-7, `-j 8`. No on-disk format change; `WritePathStatus` untouched; no
interface method changed (one doc comment in `lunix-iface/src/space.rs`). `docs/explanation/DECISIONS.md` is not edited: the proposed ADR-32
amendment is below.

## What was found, and what was changed

### 1. The pacing delay counts from arrival

**The diagnosis, corrected.** At `80e1cd1` the cap was already counted from a write's arrival for every write that reaches the gate: the engine
passes `Mutation::arrived` to `reserve_since`, the ledger's `cap_at(arrived)` and the grant loop use it. A write behind others in its queue is not
in the gate; when it reaches the front it carries its old arrival and is admitted at once if it is past the cap. The stack test below, run on
`80e1cd1`, shows it: 32 writes queued behind a slow band, slowest write 10.0 s (the cap). What was wrong:

* the figures (`delay_max_ms`, `delayed_ms`, `delayed_writes`) counted from the moment the gate parked the request, and not at all for a write
  admitted at its first contact after standing in its queue: the status said 0.2 to 0.9 s while a write had waited 10 s;
* a due write that reached the gate behind a younger head was parked and admitted only at the next timer tick;
* the timer woke at the head's tokens or cap, not at the cap of a request behind it.

So of the 8 to 17 s in the M15 histogram, what is not a stop is the cap working as designed (up to 10 s) plus service, and the stops (up to
47 s) own the rest. The measured 9.5 s fio maximum of profile 4 is the cap, not a leak.

**Changes** (`crates/lunix-space/src/ledger.rs`):

* `note_delay` counts from `arrived` (the `parked_at` field is gone). Delay figures are the wait from arrival to admission, for waits for tokens
  and for space alike.
* `note_direct_delay`: a write admitted at first contact (tokens, cap, no pacing) after standing at least 1 ms in its queue is a delayed write,
  counted in the space figures, or in the ring pace's when only it paces. `Retry` (a ring-refused write, which paid its pace the first time) and
  `Sync` (FUA bypass) are not counted.
* `park`: a request that fits and is past its cap on first contact is granted at once, with what stands before it (`grant_waiters`), as the
  deadline case already was.
* `next_wake`: the earliest of the deadline, the head's tokens or cap, and the cap of every request that fits.
* Token accounting is unchanged and now tested: a cap admission overdraws the bucket, the debt is at most one second of the rate (`overdraw` and
  `refill` clamp), the pace holds on average while `depth x size / rate` is under the cap; above it the admitted rate is `depth x size / cap`
  (the cap's purpose), never more. The ring pace composes the same way (own counters, own cap clock); `Retry` pays no tokens.

### 2. The collector is sized to the ingest from the soft mark

**The diagnosis, corrected.** The pressure already started at the entry to the band (`slowdown`). What it lacked is a size: the budget and rate
were fixed multiples (4 x) of the volume's `gc_rate`, whatever the ingest, and three further limits sat under them: the collector's share of
the coordinator after a cut (a third, at most 1 s), the step's time (4 x 50 ms) and `compact_per_pass` (256 MiB per pass, passes at least 5 s
and three marks apart, so about 17 MB/s of reclaim at WA 1.6: the 4-20 MB/s measured).

**Changes**

* `lunix-engine/src/pressure.rs`: `gc_demand(snapshot, amp_percent)` = `ingest_rate x amp / 100 x (2 - x)`, `x = headroom / soft`: once the
  ingest is known the collector is asked for the ingest times the amplification at the soft mark, rising linearly to twice that at the limit
  (zero outside the band). `Pressure::budget` and `Pressure::params` take it: the budget's bytes are `ceil(demand / gc_rate)` x the base (floor 4,
  cap 64), its time at most 8 x, the compaction rate `max(gc_rate x gc_pressure_rate_factor, demand)`; uncapped at stop and full as before.
  The levels keep their meaning (normal, raised in the band, urgent at stop and full), and the grace is still the floor grace.
* `gc_write_amp_percent` in `lunixd.toml` (default 200, 100 ..= 1000, validated; measured 150 to 170), through `OpenParams` and
  `EngineConfig::gc_write_amp_percent`.
* Under pressure the collector's share after a cut is all of the cut's time, at most 2 s (was a third, at most 1 s).
* `lunix-gc/src/config.rs::per_pass_bytes`: one pass may compact `max(compact_per_pass, 4 s of the compaction rate)`. At the daemon's 256 MiB
  and 64 MiB/s the two are equal: the normal pass is unchanged; a raised or sized rate lifts the cap with it.
* GC yield to ring-parked writes (jstall): unchanged in normal times; it no longer applies while the headroom is under twice the soft mark
  (`near_band`), so a ring-bound writer cannot carry the volume into the band with a collector slowed to an eighth. In the band pressure still
  outranks the yield, as before. The collector is never stopped by the yield (the floor of 4 MiB/s and one step per share are unchanged).

## Proposed ADR-32 amendment (for DECISIONS.md, the lead's file)

> **Amendment (milestone gate).**
> - **The delay cap counts from arrival, and so does the figure.** `throttle_max_delay_ms` is counted from a write's arrival at its queue, the time
>   it stood behind other requests of the queue included. A request that reaches the gate past its cap is admitted at once, with what stands
>   before it; the timer wakes at the earliest cap of any request that fits. `delay_max_ms`, `delayed_ms` and `delayed_writes` count from arrival
>   and include writes admitted at first contact after standing in their queue (under 1 ms is not a delay). A write never waits in the band longer
>   than `throttle_max_delay_ms` plus its own service. Unchanged: past the cap a request is admitted if it fits and the bucket carries the debt,
>   at most one second of the rate. Above the cap's reach (`depth x size / rate` over the cap) the admitted rate is `depth x size / cap` and the
>   hard mark does the pacing. The ring pace composes the same way; a `Retry` pays no tokens and counts no gate delay.
> - **GC's duty is sized to the writers from the soft mark.** From entry to the band the compaction rate and the step budget are at least the
>   ingest rate times `gc_write_amp_percent` / 100 (default 200, `lunixd.toml`, 100 ..= 1000), rising linearly to twice that at the limit, on top
>   of the floors (4 x budget, `gc_rate x gc_pressure_rate_factor`, floor grace; uncapped at stop and full). The collector's share of the
>   coordinator after a cut rises from a third to all of the cut (at most 2 s) while the gate is not open, and one pass may compact four seconds
>   of the compaction rate if that exceeds `compact_per_pass`. The collector does not yield to ring-parked writes within twice the soft mark.
> - **Limit of the mechanism (measured).** A sized collector cannot reclaim what is not yet old enough: garbage waits one grace (floor grace
>   30 s) and a pass or two. A cap `L` still fixes the sustained write rate at `(L - live) / tau`, and a volume that enters the band faster than
>   `tau` allows (a first fill of a small cap, `soft / ingest < tau`) stops once; the band's overshoot is the delay feedback of the dead time.

## Tests (each rule has one that fails without it)

| rule | test | fails when |
|---|---|---|
| figures from arrival; N writes queued behind a slow head wait at most the cap | `pace.rs::n_writes_queued_behind_a_slow_head_none_waits_beyond_the_cap` (4 queues x 40 writes at 64 KiB/s, cap 5 s: longest wait <= cap + 10 ms, `delay_max_ms` 4990..=5020) and `a_write_that_reaches_the_gate_late_is_counted_from_its_arrival` | origin moved to the first contact: 10 of 34 pace tests fail (these, the ring-pace, debt, old-behind-young and `past_the_cap` tests) |
| direct admissions after a queue wait are delays | `a_write_that_stood_in_its_queue_and_is_admitted_by_tokens_counts_the_wait` | `note_direct_delay` made a no-op: this test fails |
| a due write is admitted at first contact | `a_due_write_that_stands_behind_a_young_head_is_admitted_on_first_contact`, `past_the_cap_the_rate_rises_to_depth_over_cap_and_no_further` | the grant in `park` removed: both fail |
| the timer wakes at a request behind the head | `ledger::tests::next_wake_is_the_earliest_cap_among_the_requests_that_fit` | `due` removed from `next_wake`: fails (only this one) |
| debt after cap admissions is one second of the rate and paid | `after_cap_admissions_the_debt_is_one_second_of_the_rate_and_is_paid` (probe write waits 1.9-2.1 s: not 1 s uncharged, not 22 s unbounded) | debt unbounded: it and two throttle unit tests fail; cap admissions uncharged: it and `the_delay_cap_admits_at_exactly_the_cap_and_the_debt_is_bounded` fail |
| the pace holds on average short of the cap | `the_pace_holds_on_average_when_the_queues_are_short_of_the_cap` (200 s, 4 queues: bytes between `rate x (T-2)` and `rate x T + 1 MiB + 4 records`, no cap admission) | (invariant test, guards the clamp and the bucket) |
| ring pace: cap from arrival, own counters; `Retry` | `the_ring_pace_alone_keeps_the_cap_and_counts_from_the_arrival`, `a_retry_after_a_ring_refusal_pays_no_tokens_and_counts_no_gate_delay` | origin mutation fails the first |
| demand sizing | `pressure::tests::the_demand_is_...`, `the_budget_and_the_rate_are_sized_...`, `tests/pressure.rs::in_the_band_the_collector_is_sized_to_the_ingest_and_grows_through_the_band`, `the_write_amplification_is_the_tunable_of_the_sizing`, `the_sizing_ends_with_the_pressure` | `gc_demand` forced to 0: 3 of them fail |
| steady state in the band, no stop; the old figures stop | `lunixd/tests/gate_equilibrium.rs` (real gate and real pressure rules on a manual clock, simulated store and collector; see below) | demand forced to 0: `sized_to_the_ingest_...` fails (the second test asserts the old figures stop) |
| share under pressure | `jstall.rs::under_gate_pressure_the_collector_gets_up_to_two_seconds_after_a_cut_not_one` | divisor left at 3: fails |
| no yield near the band | `jstall.rs::the_collector_does_not_yield_within_twice_the_soft_mark_of_the_band` | `near_band` never set: fails |
| per-pass cap follows the rate | `lunix-gc config::tests::the_per_pass_cap_follows_the_rate_above_the_default_and_never_falls_below_it` | cap ignores the rate: fails |
| config key | `lunixd config` unit tests (`gc_write_amp_percent` 150 parsed, 99 and 1001 refused, default 200) | |
| stack: no write beyond the cap + service, no stop, data verifies | `stack_throttle.rs::deep_overwrites_at_a_small_cap_wait_at_most_the_cap_and_the_gate_never_stops` (real gate, engine, store, collector; 32 x 256 KiB writes in flight at 64 KiB/s inside the band, 45 s): slowest 12.9 s (cap 10 s + commit and append), `delay_max_ms` 10 001, 0 stops, 55 cap admissions, every block reads back, fsck clean | on `80e1cd1` it **fails**: slowest 10.0 s but `delay_max_ms` 922 (the figure from the front, not from arrival) |

`gate_equilibrium`: writer 100 MiB/s through one queue (256 in flight), 1 GiB live, garbage reclaimable after the grace, collector work bounded by
the rate it is handed, its step budget (20 steps/s) and a 120 MiB/s device ceiling, WA 1.6, `gc_rate` 1 MiB/s, 10 GiB limit, 900 s. Sized:
0 stops, 0 `ENOSPC`, rests in `slowdown` at a headroom of 1.33 GiB (x = 0.82, the curve's point for the 75 MiB/s the collector reclaims), tail
admitted rate 75 MiB/s, longest wait 210 ms. The old figures (rule removed): 1 604 stops, 4 MiB/s. The model is mine: it shows the sizing rule
decides between equilibrium and stops when `gc_rate` is small against the ingest; it does not show that real stops go away (see the end-to-end
check).

Mutation runs: logs of each mutation (patch applied on the host copy, tests run, patch reverted) were kept in the scratchpad; the results are the
"fails when" column. M3 (next_wake) is caught only by its unit test; no pace test sees it.

## Gates

* `tools/ci.sh` on the head of `ca22aa3` (CPUs 0-7, `-j 8`): green, `check-ownership: gate ok (20 changed paths)`, `ci: all green`; 2 220 tests
  passed, 0 failed, 28 ignored (sum of the `test result` lines). The commit after it changes only casts in a test file and this report
  (`gate_equilibrium.rs` re-run: 2 passed; clippy `-D warnings` on `lunixd` clean).
* Kill loops, by hand on the same code, one at a time (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`,
  `cargo test --release -p lunixd --features testsupport,crashpoints --test stack_kill`):

| loop | points reached | runs / kills | kills inside a partial cut | result |
|---|---|---|---|---|
| plain 16 KiB | 52 of 55 | 54 / 54 | 40 of 41 points | ok (1 733 s) |
| integrity 16 KiB | 52 of 55 | 57 / 57 | 40 of 41 | ok (1 907 s) |

## End-to-end check (M15 harness, `p1/m15` @ a92d27c, the same cell on both builds)

`tests/exit/fio-verify.sh`: xfs, `CHUNKS=16384 MODES=integrity PROFILES="128:8"` (QD 128 x 8 jobs), `CACHE_MAX=8G` (not 12G: the 12G store plus the
volume exceeds the 10 GiB remote budget), `PER_PROFILE_MIB=2048 WLOOPS=12 RUNTIME=240`, daemon on CPUs 0-3, fio on 4-7, 4 queues, release binaries
of `80e1cd1` (before) and of this branch (after), root run through the wrapper's lock and leftovers checks, one at a time. All four cells
**passed** (0 verify errors before and after the detach, no `ENOSPC`, no `full`, fsck clean).

| run | gate stops | gate-side longest wait (status `delay_max_ms`) | fio longest completion | delayed writes | cap admissions |
|---|---|---|---|---|---|
| before 1 | 2 | 23 296 ms (from the front of the queue) | 37 990 ms | 110 509 | 100 |
| before 2 | 0 | 333 ms (from the front) | 14 234 ms | 142 101 | 0 |
| after 1 | 1 | 13 137 ms (from arrival) | 21 142 ms | 280 210 | 112 |
| after 2 | 6 (3 episodes logged) | 15 449 ms (from arrival) | 36 404 ms | 250 908 | 264 |

**This does not show an improvement in stops, and the report does not claim one.** Two runs per build vary more than the difference
(before 2 and 0, after 1 and 6). What the runs do show:

* the figure now says what a write waited: the two "after" maxima (13 and 15 s) are stop episodes of 15 to 20 s (the log lines `writes wait for
  space` bracket them), and `delayed_ms` is 4.5 to 27 s per throttle episode instead of 0.03 to 0.16 s per 10 000 writes;
* the stop at the first band entry is in every run that has one: `write throttle on ... reclaim_rate=0` and `writes wait for space` 15 s later at
  `used` = the limit. At entry no garbage is yet older than the grace (floor 30 s), so no collector, however sized, has anything to free;
  at 200 MB/s the 1.3 GiB between the soft mark and the limit last 6 s. This is the ADR's `L = R x tau` limit: with the cap at 8 GiB
  and 2.3 GiB live the sustainable rate is about 150 MB/s, the cell offers 200+;
* the reclaim rate in the band was 120 to 280 MB/s in all four runs (the sizing's demand was met by the base figures in this cell: demand
  200 to 400 MiB/s against a floor of 256 MiB/s), so the sizing is not what separates the runs;
* the later stops of after 2 and before 1 follow cap admissions (`delay_capped` 264, 100): 1 024 writes in flight against a headroom
  of a few hundred MiB; the cap admits them faster than the curve, and the hard mark does the pacing (ADR-32, consequences). That is the
  designed overrun, not a regression of this change, but the extra cap admissions in after 2 are a visible effect of counting all waits from
  arrival and of the longer collector share; the sample is too small to say which.

## Deviations, choices, open items

1. **The finding-1 diagnosis was refined** (above): the cap clock was already the arrival for writes that reach the gate; the defects are the
   figures, the late-contact admission and the timer. The brief's wording "counted only once a write reaches the head" holds for the figures only.
2. **The finding-2 diagnosis was refined**: the pressure already started at `slowdown`. The change is the sizing, the share, the per-pass cap and
   the no-yield zone. **It is not shown to remove stops** (end-to-end above). The limit that remains is the grace dead time at the first fill of a
   small cap; a lead zone (pressure and floor grace from `soft + ingest x (tau_floor + pass period)` instead of at `soft`) would address it and
   is not implemented: it moves the start of pressure before the band, contrary to the brief's "at the soft mark". Recorded as an open choice.
3. **`tools/ownership.toml` gained a `[gate]` entry** (there was none). Files outside the brief's list that the entry owns: `lunix-gc`
   (`collector.rs`, `config.rs`: the per-pass cap), `lunix-iface/src/space.rs` (one doc comment), `lunix-testkit/src/space.rs` (`MemSpace::
   set_throttle_figures`), `lunix-engine/tests/jstall.rs` (two tests), `lunix-engine/src/{config,coord,engine,lib}.rs`.
4. **The sizing changes normal behaviour only through `near_band`** (no yield within 2 x the soft mark) and the share while pressure holds;
   the daemon's default pass and rate are unchanged by `per_pass_bytes`. A volume with `gc_rate` above 64 MiB/s now gets 4 s of its rate per
   pass instead of 256 MiB in normal times too.
5. **The 8G cap of the end-to-end cell** and the 2 GiB profile (disk budget); the 12G of the brief was not run.
6. **A pre-existing failure outside CI:** `lunix-engine --release --features testsupport --test queue`
   (`the_background_coordinator_commits_under_load_without_losing_a_write`) fails on `80e1cd1` too; CI runs it in the debug profile, where it
   passes. Not caused by this change.
7. **`ci.sh` was not edited** (not in the `[gate]` list): `gate_equilibrium` runs in `cargo test --workspace` (no `testsupport` needed);
   the new stack test is in `stack_throttle`, already in the CI list.
8. **No rust-skills conflicts.** Tests use a few `as` casts on counters and float arithmetic (`gate_equilibrium.rs`); library code uses
   `try_from` and saturating arithmetic.
9. **No new counters** in `ThrottleFigures`; the delay figures changed meaning (documented in `lunix-iface/src/space.rs`).

Remote disk: the target directory grew to 24 GiB over the CI runs (debug and release builds), above the 10 GiB budget, until `cargo clean
--profile dev` cut it to 3.6 GiB mid-run; the end-to-end store peaked at 7.7 GiB (cap 8 G). At the end the whole remote directory of this
milestone was removed (the root-owned scratch of the end-to-end runs through the wrapper's `root` subcommand); no mount, device or process of
mine is left. `root.lock.holder` still names this milestone (the file is rewritten by the next root run).
