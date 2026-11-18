# Report p1-pace: ADR-32 T3, T4 and T5 (the paced band, GC pressure, the throttle status) and the gcrate follow-ups

Branch `p1/pace` from `origin/main` 03b858e. Spec: ADR-32 (accepted 2026-11-18, M1-M9), the note
`docs/explanation/research/2026-11-18-write-backpressure.md` (§5.3, §5.6, §7, §8, §9, §11), and the final review of
`p1/gcrate` (findings F-1, F-2, F-4, F-5). Builds and tests ran only on the remote test host (kernel 7.0.0) through
`tools/remote.sh`, CPUs 8-15, `-j 8`. No root run, no on-disk format change.

## What was built

| item | what | where | tests |
|---|---|---|---|
| T3 curve, bucket | `ThrottleConfig` (soft mark 15 % of the limit clamped to 1 ..= 64 GiB and to half the limit; rate `max(r_min, r_mid x/(1-x))`, `r_mid = max(gc_rate, 16 MiB/s)`), a token bucket (capacity one record or 50 ms, debt at most 1 s of the rate), counters, a state clock and a 10 s rate meter, all plain state machines | `lunix-space/src/throttle.rs` | unit tests in the file: the documented values at x = 0.25, 0.5, 0.75, 0.9; monotonic, unbounded at the mark, `r_min` at the limit; bucket admits rate x time within one bucket; debt bound |
| T3 gate | `Ledger::reserve` decides admit, `Paced`, `Parked` or refuse; `grant_waiters(now)` admits the head when it fits and the bucket has its bytes or its delay cap passed; the timer thread sleeps until the head's token time or cap; `ReserveOutcome::Paced`, `ReserveClass::{Bulk, Sync}` (FUA write of at most 256 KiB, admitted at once, charged to the bucket); `reserve_since` takes the class | `lunix-space/src/{ledger,gate}.rs`, `lunix-iface/src/space.rs` | `lunix-space/tests/pace.rs` (13 tests, incl. the 3 000-sequence property test with the band on: `used` within the limit, no request older than its deadline, ledger exact), `model.rs` follows the tick that grants |
| T3 engine | the queue passes the class, handles `Paced` as a wait that is not a refusal (no refusal flag, no window turn); `space_refusing` counts only the requests waiting for space | `lunix-engine/src/{queue,full}.rs` | stack test below |
| T3 config | `lunixd.toml`: `throttle`, `throttle_soft_percent`, `throttle_soft_min_bytes`, `throttle_soft_max_bytes`, `throttle_min_rate`, `throttle_max_delay_ms`, `gc_pressure_rate_factor` (validated as in the note §8; `full_timeout_ms` and `store_max_percent` already existed); reference table in `config.rs` | `lunixd/src/{config,daemon}.rs`, `stack/mod.rs` | `the_throttle_keys_parse_and_are_validated` |
| T4 | `Pressure` (`normal`, `raised`, `urgent`) from the gate's state at each coordinator tick, 5 s hold; under pressure 4 x step budget (duration and bytes), `gc_rate x gc_pressure_rate_factor` (uncapped when `stop`/`full`), grace = floor (10 s + 2 x `read_timeout`), never longer than configured. `gc_grace_ms` default unchanged. All collector calls (steps, urgent pass) are handed the boosted params | `lunix-engine/src/{pressure,coord,gc,engine,config}.rs` | `lunix-engine/tests/pressure.rs` (exact figures per state, the hold, restart of the hold, factor, grace never grows, urgent pass), `lunix-gc` `a_grace_shortened_between_condemn_and_verdict_deletes_nothing_early` (the directed test the note asks of gcrate) |
| T5 | `SpaceSnapshot::throttle` (`ThrottleFigures`) and the `throttle` object of `volume_status` (`ThrottleStatus`: state, since, headroom, soft, resume, limit, rate, paced/space waiters, delayed writes and ms, longest wait, capped, bypassed FUA, holes admitted, ENOSPC, stops, fulls, throttled ms, ingest and reclaim rates, `gc_pressure`, amplification); `lunix status` prints one line when the state is not `open`; log lines of the note §7 from the engine (`ThrottleWatch`: on, 30 s line, off, wait for space, full, no longer full; transitions closer than 5 s merged) | `lunix-iface/src/{space.rs,ctl/msg.rs}`, `lunix-engine/src/pressure.rs`, `lunixd/src/daemon.rs`, `lunix-cli/src/render.rs` | `pressure.rs` unit tests (log lines and coalescing), `ctl.rs` round trip, `status_prints_one_throttle_line_when_the_state_is_not_open` |
| Stack tests | `a_volume_entering_the_band_is_paced_and_not_refused` (fresh data to 3 MiB of headroom: no write refused, state `slowdown`, delayed writes, delay under the cap, a FUA write and a FLUSH not delayed, `bypassed_fua`, GC raised, read-back, fsck); `gc_pressure_raises_gc_budget_and_rate_and_the_slow_gc_cell_is_paced_not_refused` (16 MiB working set rewritten for 40 s on a 128 MiB cap with a configured grace of 60 s: no write fails, `used` inside the cap plus reserve at every sample, the first delete comes inside the run and with the gate not open, which only the floor grace allows, then the gate opens and the pressure returns to normal, read-back, fsck) | `lunixd/tests/stack_throttle.rs` | measured: band cell 59 MiB written, 72 delayed writes, longest wait 177 ms; slow-GC cell 2 192 writes, no ENOSPC, longest wait 4.1 s, peak 131 MiB of 128 MiB, first delete at 15 s |
| F-1 | the mark-time measurement is tested on a clock that moves inside the mark unit (`Hooks::advance_at`): 7 s of mark gives `last_mark` 7 s and a 21 s gap; time spent in delete units counts for nothing; the phase is matched by variant (`Phase::is_mark`) and the slots have names (`slot::MARK` ...) | `lunix-gc/src/collector.rs`, `tests/{sim,pipeline}.rs` | `a_pass_measures_its_mark_time_on_the_view_clock_and_not_its_deletes` (fails with the accumulation removed, checked by hand) |
| F-2 | the abandoned-pass test now lets a grace end after the abandoned pass's mark began, so only `last_aborted` holds the next pass back: none at +5 s, one at +31 s | `tests/pipeline.rs` | fails with `last_aborted` dropped from `due` (checked by hand) |
| F-4 | the scripted collector panics after 1 000 steps (a runaway share fails, no hang); new tests: the collector goes idle mid share (3 share steps, not 10), a quiesce during a share (no collector step after the one that saw it, in the share or in the final commit) | `lunix-engine/tests/gc_pacing.rs` | the two tests |
| F-5 | the daemon-schedule ladder cell asserts that a scheduled pipelined pass finished before any ladder run (`stats.passes` > 0, `compact_runs` = 0) | `lunixd/tests/stack_gc_ladder.rs` | the cell |
| docs | DESIGN §2, §4.3, §18, §19, §20; GLOSSARY "Space ladder" and "Soft band"; `docs/reference/gc.md` ("GC under write pressure"); the `lunixd.toml` reference (table in `config.rs`); `lunix-space` crate docs | | |

## Deviations from the ADR, and why

1. **The gate has no `tracing`.** The log lines are written by the engine (`ThrottleWatch` over the gate's snapshot at each tick), so no
   dependency changed. A transition is therefore logged within one coordinator period of the state change, and the state's
   "since" is the gate's next observation of it (a reservation that crosses the soft mark is noticed by the next tick, the timer or
   the next band reservation). The Stop line has no `want` field (the note lists one); it carries waiters, limit, used and `full_timeout_ms`.
2. **Section 9.2's single deterministic engine test is split.** The gate side (curve, bucket, cap, FUA, latch, property test) is in
   `lunix-space/tests/pace.rs` on a manual clock; the engine side (budgets and params per state, the 5 s hold) is in
   `lunix-engine/tests/pressure.rs` with a gate that reports a scripted state (`MemSpace::set_throttle_state`); FLUSH and FUA not waiting
   are in the stack cell. There is no engine test that runs a real gate under a scripted collector with `Y` freed per virtual second.
   The slow-GC stack cell runs 40 s with a rewrite, not 120 s with `gc_rate` 1 MiB/s and a short grace.
3. **The note's "Two volumes on one simulated file system" stack test and the replayed-E1 variants were done by bstop**; the two-volume
   cell is not written.
4. **`ingest_rate` and `reclaim_rate`** are cumulative counters sampled once a second and averaged over the oldest sample inside 10 s;
   "freed" is a release of segments, packs, runs, commits or cache (not the journal). `holes_admitted` is `ops_discard + ops_write_zeroes`
   of the engine (every such request is admitted from the reserved region). `delayed_writes`/`delayed_ms` count requests that waited in
   the gate and were admitted (for space or for the pace); `delay_max_ms` also counts those that failed.
5. **The delay cap counts from the request's arrival** (`reserve_since`), and is the smaller of `throttle_max_delay_ms` and the volume's
   `full_timeout`, so a pace never fails a request and never latches the volume. A configured `throttle_max_delay_ms` above `full_timeout_ms`
   is a config error; the default 10 s under a shorter `full_timeout_ms` is not.
6. **`on_full = error`** keeps a paced request that fits (pacing is not a refusal) and fails the rest; **the quiesce** admits a paced request
   that fits. A tick now also grants what fits (the timer thread's job), also with the band off; `model.rs` follows it.
7. **GC pressure applies to every collector call** (a requested pass too), through `with_collector`; only `run_pass`'s own step budget is not boosted.
8. **The band is on by default in the existing stack tests**; they pass unchanged except `fill_until_parked`, which now counts only requests
   waiting for space. The `lunix-space` tests other than `pace.rs` build their gates with the band off (`common::config`).
9. **DECISIONS.md is not edited** (not in this milestone's paths): ADR-32's "Not implemented yet" status line is stale.

## For the maintainer

- The band is `r_mid = max(gc_rate, 16 MiB/s)` of the volume at open; `lunix` has no live `gc_rate` change, so the gate never needs a new `r_mid`.
- The sustained-rate cell and the M15/M16 harness changes (T7) are not part of this milestone.

## Gate

Full `tools/ci.sh` on the host: every step green (tool tests, fmt, clippy, workspace tests, the crashpoints builds, feature edge,
lunixd stack tests in release with lunix-fsck): **1975 passed, 0 failed, 28 ignored** (summed over the `test result` lines of all
steps). The ownership step failed once on `crates/lunix-cli/src/fsck.rs` (a test-fixture literal; added to `[pace]`); after the fix
`tools/check-ownership.sh` reports `pace ok (43 changed paths)`. The full gate was not rerun for that one-line table change.
The remote target directory was deleted afterwards.
