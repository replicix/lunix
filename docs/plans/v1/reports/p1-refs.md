# p1-refs: C5-B shadow stage (the per-segment reference table) and the fastrecheck follow-ups

Branch `p1/refs` from `origin/main` 6ba014b (not moved at hand-back). Design: `c5-recheck-design.md` section 2
(shadow parts), sections 3-5, milestone 2 of section 6; follow-ups R2-1..R2-4 and N6 of the fastrecheck review.
Builds and tests ran only on the remote test host (kernel 7.0.0) through `tools/remote.sh`, CPUs 0-15, `-j 16`.
**No on-disk format change.** The table is memory only; nothing is condemned, listed, flagged or deleted because of it
(no `propose_dead`, no `on` mode: the enum has `Off` and `Shadow` only). The T6 soak was not run.

## What was built

| item | what | where |
|---|---|---|
| `SegRefs` | per-segment entry counts by hint (signed while `Rebuilding`), `went_zero` / `zero_since`, snapshot at mark start, `rebase` (`counts - snapshot + mark`, drift counted, logged, repaired, zeros cleared on drift), `ready` (lag `2 x (retain_last - 1)`, skip set, max 64, oldest cut first), shadow offer and judgement, counters | `lunix-engine/src/refs.rs` |
| `RefMap` | `BlockMap` wrapper for the cut only (`Inner::cut_map`: `DestageCtx::map` and the re-hint transaction). Ops are forwarded to the real transaction at once (so `MapFull` still comes from `set`) and recorded; at commit the old entries are read (`get`, one per chunk), the count changes predicted, the prediction checked against `TxnReport.sets` / `rehints_applied` (mismatch: `Drifted`, `report_mismatches`), then applied. A dropped or failed transaction records nothing; a prediction that cannot be read marks the table `Drifted` and never fails the cut | `refs.rs`, `coord.rs`, `engine.rs` |
| `seal_cut` | after `co.tip = full` (C6): zeros of the cut get `zero_since = (seq, now)` | `coord.rs` |
| shadow step | at every `gc_step`: `ready` (skip: `pending_named`, new segments of `co.moved`) is counted and remembered, never offered | `coord.rs` |
| `Collector` | defaulted `take_mark_started`, `take_tip_refs`, `take_mark_condemned` (the third is an addition to the design: the mark's own newly dead segments, needed to compare table and mark) | `lunix-iface/src/gc.rs`, `lunix-gc/src/collector.rs` |
| mark | `Liveness::tip_refs` (entries per segment of root 0, run-length summed), `Mark::take_init_tip`; counted only with `GcConfig::tip_refs` (default off: no extra work in the mark) | `lunix-gc/src/mark.rs`, `config.rs` |
| engine wiring | `with_collector` takes the three reports after every call (snapshot, rebase, judge) and publishes `GcStatusSnapshot::refs: Box<RefsStats>` | `lunix-engine/src/gc.rs` |
| key | daemon `gc_condemn_on_cut = "off" \| "shadow"` (quoted string, default off); `EngineConfig::gc_condemn_on_cut: CondemnOnCut`; a non-off value also sets `GcConfig::tip_refs` in `open_volume`. Without a collector the table does not exist | `lunixd/src/{config,daemon}.rs`, `stack/mod.rs`, `lunix-engine/src/config.rs` |
| metrics | `RefsStats`: `state` (off/rebuilding/ready/drifted), `segments`, `zero_waiting`, `ready`, **`drift_segments`**, `rebases`, `report_mismatches`, `shadow_would_propose`, `shadow_confirmed`, `shadow_unconfirmed`, `shadow_revived`, `shadow_missed`, `condemn_lag_ms_{sum,max}` + count, plus the three the coordinator asked for: **recall** (`confirmed / (confirmed + missed)`), **idle wait** per offered segment (`idle_wait_ms_{sum,max}`, `idle_wait_count`, `idle_waiting`: the time from the offer to the first collector call that finds it between passes) and **`first_ready_ms`** (attach to the first ready segment). `lunix gc --status` prints `refs table`, `refs shadow`, `refs first ready` when the table exists | iface, `lunix-cli/src/render.rs` |
| ownership | `[refs]` entry (the fastrecheck paths it touches are listed in it) | `tools/ownership.toml` |
| docs | `gc.md`: "Reference table, shadow mode"; key table of `lunixd/src/config.rs` | |

### Follow-ups folded in

| id | done |
|---|---|
| R2-1 | the `fast_due` doc comment states the current rule (delay from the grace end, duty `3 x mark` from the last pass end) |
| R2-2 | `a_kill_inside_a_recheck_pass_is_recovered` asserts the pass kind in progress from the collector (`Gc::pass_kind()`, test-only), not `regular_passes == 2` (kept as a second check) |
| R2-3 | `the_recheck_lag_stays_about_a_second_when_the_regular_passes_take_every_grace_end`: the 20 s spacing run (`fast_passes` was 0 in it), asserts `recheck_lag_ms_max <= 1500` (observed 0), at least 6 regular passes in 100..260 s and `L` compacted within 40 s. The 25 s test shares the new `sustained()` helper |
| R2-4 | `GcReclaimStats` docs: `regular_passes` = all passes minus `fast_passes`; the difference of the lag figures includes urgent passes, which judge with the floor |
| N6 | `a_grace_that_ends_during_the_recheck_passs_own_mark_is_kept` (S2 rule; the next pass then deletes it, a regular one: it wins when both are due); `an_abandoned_pass_switches_recheck_passes_off` extended: after a regular pass succeeded, a recheck-only pass runs again (delay boundary at 115.9 / 116.0 s, before the regular pass at 120 s). The other two N6 items (urgent pass in the middle of a recheck pass; the file sets of the equivalence test) were not done |

## Tests

Unit (`refs.rs`): zero ready after `0 / 2 / 4` cuts for `retain_last 1 / 2 / 3`; revival and dip inside one transaction;
skip, max and oldest-first; a `Rebuilding` or `Drifted` table offers nothing; first mark is the base, matching mark is no
drift, differing mark counted and repaired (deltas since the snapshot kept), snapshot of another tip ignored; shadow
offer judged only by a mark that began at or after it, confirmed / unconfirmed / revived / missed; idle wait and
first-ready time; `RefMap` over `FlatMap`: set over hole / entry, hole over entry and hole, re-hint applied / skipped,
several ops on one chunk, dropped transaction, `MapFull` commit; proptest (64 cases): random transactions (sets, holes,
moves, stale re-hints, drops) with the table equal to a chunk-by-chunk count of the map after every commit and no
report mismatch.

Engine (`tests/refs.rs`, real `Gc`, manual clock, 24 cases each): dedup-heavy (3 patterns over 12 chunks) and
compaction-heavy (distinct partial overwrites, threshold 0.9) workloads over `retain_last` 1..3, pipelined or not, with
`fast_recheck_always` on in the pipelined runs: `drift_segments == 0`, table `Ready`, at least one rebase, and **the
segment set, the collector counters and `condemned_pending` after every step, and the final device, equal those of the
same run with the mode off**. Negative control: a collector that adds one entry to every mark's counts is detected
(`drift_segments >= 1`). A fixed overwrite workload offers what the marks condemn (11 offered, 10 confirmed, 1 not, 1
condemned by the mark only: recall 91 %; table `Ready`, 30 rebases, drift 0). Off mode and no collector: no table.

Collector (`lunix-gc` `tests/refs.rs`): a mark's `tip_refs` equals a count of the map (dedup in it), `take_mark_started`
is the tip, the condemned segments are unreferenced and ascending; nothing counted without `tip_refs`. Daemon key
parse test; CLI render test; control-protocol proptest carries `refs`.

New tests: 14 + 5 (engine) + 2 (gc refs) + 2 new and 2 changed (recheck) + 1 + 1 (+ the extended ctl proptest); `ci.sh`
runs 2385 tests (2333 on main).

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` | green (2385 tests passed, fmt, clippy `-D warnings`, the crashpoint feature builds, the lunixd stack tests in release with `lunix-fsck`, ownership: 24 paths) |
| GC kill loops, release, `LUNIX_KILL_HITS=all`, `lunix-fsck` with crashpoints, one process per loop, all six concurrently on the full host, **every child with `gc_condemn_on_cut = shadow` and `gc_fast_recheck` on** | `gc_kill_loop_plain_16k`: 24 points (23 reached), 68 kills, 4 826 s, pass. `gc_kill_loop_integrity_16k`: 68 kills, 4 883 s, pass. `gc_kill_loop_plain_64k`: 68 kills, 5 160 s, pass. `gc_kill_loop_recheck_plain_16k`: 5 points, 14 kills, 1 364 s, pass. `gc_kill_loop_recheck_plain_64k`: 14 kills, 1 272 s, pass. `gc_kill_loop_recheck_integrity_16k`: 14 kills, 1 295 s, pass |
| `refs_drift_segments == 0` at every recovery | asserted after every kill's recovery (after `settle_gc`, on both opens: the table must be `Ready`, `drift_segments == 0`, `report_mismatches == 0`), at the end of the counting run's child and before each reopen step; all passed |
| table = mark on randomized workloads, shadow changes no decision | engine `tests/refs.rs` (above) |
| cut time with shadow vs off | see below: -0.30 % median, -0.15 % mean (target within 3 %) |

The plain arms of the kill loops set `fast_recheck` without `fast_recheck_always` and see no pressure, so only the
recheck arms run recheck-only passes (as in fastrecheck); both are with the table on.

### Cut time

`stack_refs_cost.rs` (`#[ignore]`, release): 1 GiB volume of 64 KiB chunks on the real stack (radix map, pipelined
destage), filled, one whole GC pass rebases the table (`Ready` asserted in the shadow arm), the collector then sleeps
(pass interval 1 h) so no mark shares the coordinator; per cut 2048 random chunks (128 MiB) are overwritten and the wall
time of `commit_now` is taken; 20 cuts per arm, 4 repetitions, arms interleaved and alternated, a fresh volume each. Off:
median 102.97 ms, mean 107.03 ms; shadow: median 102.67 ms, mean 106.87 ms (about 1 % between repetitions). The table
costs one `get` per chunk on leaves the commit loads anyway. The measurement does not include the extra mark work (the
run-length count of the tip root, cheap but not timed on a big map).

## Deviations and open choices

* A third defaulted trait method, `take_mark_condemned`, and `GcConfig::tip_refs` (counting is off unless the daemon's
  key is on) are additions to the design; `GcConfig` is therefore in the `[refs]` paths, as is `stack_refs_cost.rs`.
* A table state `Drifted` (design 2.2) is kept: a transaction report that disagrees with the prediction.
* The shadow "offer" is made at every `gc_step` whatever the collector does; the idle wait is derived from the engine's
  last published collector state, not from the collector's own idle moments.
* Recall is only measured where the table is `Ready` at the mark's start; the first mark after an attach is the base.
* Not run: the encrypted and 64 K integrity kill loops, `stack_kill`, T6. Nothing in this change touches the commit path
  kill points; the cut path gains one read per chunk.
* rust-skills: no conflicts with the project's conventions. `refs.rs` is large because of its tests (569 product lines).

## Line counts (product / tests, rustfmt)

Product: `refs.rs` 569; engine `coord/engine/gc/config/lib` 92; collector, mark, config 127; iface 129 (mostly docs); lunixd
42; CLI 36. Tests: `refs.rs` (unit) 484, `tests/refs.rs` 367, gc `tests/refs.rs` 99, `recheck.rs` +141, `stack_refs_cost.rs` 170,
kill loop +29, others ~60. Docs: `gc.md` +29.

## Remote disk use

The remote tree of this milestone was 27 GiB after the release builds and the kill loops; it was cleaned to the source
tree and the logs (under 10 GiB) at hand-back. The test host is idle (no process of this milestone running).
