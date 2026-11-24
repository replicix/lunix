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
| metrics | `RefsStats`: `state` (off/rebuilding/ready/drifted), `segments`, `zero_waiting`, `ready`, **`drift_segments`**, `rebases`, `report_mismatches`, `shadow_would_propose` (an upper bound: the active stage only takes offers that meet an idle collector), `shadow_offered_idle` / `_busy`, `shadow_confirmed`, the **lead** over the mark's own condemn (`shadow_confirmed_lead`, `shadow_lead_ms_{sum,max}`, `shadow_confirmed_no_lead`), `shadow_unconfirmed_live` / `_kept` / `_listed`, `shadow_revived`, `shadow_early_by_mark`, `shadow_missed`, `condemn_lag_ms_{sum,max}` + count, **recall** (derived: `confirmed / (confirmed + early + missed)`), **idle wait** (`idle_wait_ms_{sum,max}`, `idle_wait_count`, `idle_waiting`, only for offers made during a pass), `first_ready_ms` and `first_mark_condemn_ms`. `lunix gc --status` prints `refs table`, `refs shadow`, `refs first ready` when the table exists | iface, `lunix-cli/src/render.rs` |
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

## Review fixes (second round, commit after 747cb2d)

| id | done |
|---|---|
| B1 | `tainted`: set by `drifted()` (which also drops `zero_since` / `went_zero`), cleared by `mark_started()`; `rebase()` keeps `Drifted` / `Rebuilding` when tainted, and the mark is judged as not-ready. Unit tests that fail without it (drift between `mark_started` and `rebase`, and the same while `Rebuilding`) |
| S1, S2 | per-offer record (tip, time, first idle moment); offered idle / busy; lead over the mark's condemn; unconfirmed split into live-by-id / kept / listed (`MarkCondemned` carries `live` and `listed`); a segment the mark condemns while the table knew its zero is `early_by_mark`, not missed, and leaves `zero_since`; revival and judgement clear the idle-wait entry; `first_mark_condemn_ms`. Recall by bytes is not done |
| S3 | `Lying` map under `RefMap`: a dropped re-hint (report mismatch at the commit, `Drifted`, nothing offered; the next mark finds exactly the 2 segments and the table equals a count again) and a wrong `sets` (mismatch, `Drifted`, silent) |
| S4 | kill loops: the recovered volume waits for `Ready`, writes, **cuts**, settles, then asserts `rebases >= 2`, drift 0, mismatches 0 (first run of the new check failed in five loops because no cut followed the writes within `settle_gc`, so no mark compared; fixed by the explicit cut); the child logs warnings and the parent fails on any "reference table" line of a child log, killed or not |
| S5 | engine proptests through an explicit `TestRunner` (48 cases, `LUNIX_REFS_CASES`), scripts with `Reopen` (crash and recovery: the `Rebuilding` path) and `Urgent` (a mark with no offer), coverage asserted (dedup hits, compaction, offers, confirmations, reopens, urgent passes). The unit proptest over `RadixMap` is not done |
| S7 | one switch: `Collector::set_tip_refs`, called by `Engine::open` when the table exists; the daemon no longer sets `GcConfig::tip_refs`; tested with a collector built without it |
| S8 | the gate is the cut time per journal byte (see below); `map_ns` per chunk, the snapshot clone and `ready()` are measured and reported |
| nits | `refs: Option<Box<RefsStats>>` (no allocation with the mode off), `serde(default)` removed from `reclaim` and `refs`, `ownership.toml` rebuilt from main plus `[refs]`, `mark_started` gated by the flag, `heap_bytes` counts `tip_refs`, docs (N5, N9, N11-N14), CLI prints recall, lead, first ready, first mark condemn and idle wait. Not done: N2 (one `take_mark_report`), the N1 comment, the `pending` shortcut for ascending chunks |

## Tests

Unit (`refs.rs`, 22 tests incl. the proptest): zero ready after `0 / 2 / 4` cuts for `retain_last 1 / 2 / 3`; revival and dip inside one transaction;
skip, max and oldest-first; a `Rebuilding` or `Drifted` table offers nothing; first mark is the base, matching mark is no
drift, differing mark counted and repaired; B1 (twice); the classes of unconfirmed offers, the lead and the early mark;
idle wait and first-ready time; revival and judgement clear idle waits; `RefMap` over `FlatMap`: set / hole / re-hint cases,
several ops on one chunk, dropped and failed (`MapFull`) transactions; the two negative controls; proptest (64 cases):
the table equals a chunk-by-chunk count of the map after every commit.

Engine (`tests/refs.rs`, real `Gc`, manual clock): dedup-heavy and compaction-heavy proptest drivers (48 cases each, `retain_last` 1..3,
pipelined or not, `fast_recheck_always` on, scripts with restarts and urgent passes): drift 0, table `Ready`, and **the segment set,
the collector counters and `condemned_pending` after every step, and the final device, equal those of the same run with the mode
off**; coverage asserted. Negative control: a collector that skews every mark's counts is detected. One switch test; a fixed
workload as a smoke test (the counters move; it says nothing about a real volume). Collector (`lunix-gc` `tests/refs.rs`): the mark's
`tip_refs`, tip, `live` and `listed` against a count of the map; `set_tip_refs` on and off.

`ci.sh`: 2398 tests (2333 on main).

## Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-11, `-j 12`) | green, 2398 tests, fmt, clippy `-D warnings`, crashpoint builds, lunixd stack tests in release with `lunix-fsck`, ownership |
| GC kill loops, release, `LUNIX_KILL_HITS=all`, `lunix-fsck` with crashpoints, eight processes concurrently on CPUs 0-11, every child with `gc_condemn_on_cut = shadow` and `gc_fast_recheck` on | all eight pass. plain 16K: 68 kills, 5321 s. integrity 16K: 68, 4916 s. plain 64K: 68, 4735 s. integrity 64K: 68, 4695 s. encrypted 16K: 68, 5000 s. recheck plain 16K: 14, 1500 s. recheck plain 64K: 14, 1347 s. recheck integrity 16K: 14, 1381 s. The 23 reachable points of the plain arms and the 5 of the recheck arms were killed at first, middle and last hit |
| drift 0 at every recovery | after every kill's recovery the table is `Ready` with `rebases >= 2`, `drift_segments == 0`, `report_mismatches == 0`, and no child log has a "reference table" warning |
| table = mark, shadow changes no decision | engine `tests/refs.rs` |

The first run of the eight loops (before the explicit cut in `verify`) failed the new `rebases >= 2` assertion in five of them at the
first kill (only the base mark had run): a harness defect, not a table defect; the three recheck loops had passed.

### Cost

The host was **not quiet**: another milestone's CI ran on CPUs 12-15 and this one on 0-11. The figures compare shadow with off inside
the same run, arms interleaved and alternated over 4 repetitions of 20 cuts (128 MiB of journal per cut, a 1 GiB volume
overwritten twice before the base pass, the collector asleep during the timed cuts), on the real stack.

| quantity | 16 KiB | 64 KiB |
|---|---|---|
| **cut time per journal byte (the gate)**, median, off / shadow | 220.06 / 222.19 ms per 128 MiB (1.719 / 1.736 ms per MiB): **+0.97 %** (mean +0.88 %) | 155.83 / 157.23 ms (1.217 / 1.228 ms per MiB): **+0.90 %** (mean +0.19 %) |
| `map_ns` per chunk (a `RadixMap`, the destager's transaction shape, leaves cached), raw / wrapped | 49 / 148 ns: +99 ns (+199 %) | 42 / 123 ns: +81 ns (+189 %) |
| `mark_ms` of the base passes, off vs shadow (`tip_refs` on) | about 120 to 130 ms in both (no visible difference) | about 34 to 38 ms in both |

`map_ns` per chunk roughly triples (no bound was set); at 64 KiB chunks and 2048 chunks per cut it is 0.2 ms of a 156 ms cut, at 16 KiB 0.8 ms
of 220 ms. The map is warm in this measurement: the table's `get` per chunk hits the leaves the commit loads anyway; on a volume whose
leaves exceed the map cache the `get` can miss, which was not measured. The mark-start snapshot (a clone of the counts, under the
coordinator mutex) costs 0.01 ms at 16 k segments, 0.09 ms at 128 k and 10.8 ms at 1 M segments (a 8 MiB-segment volume of 8 TiB). `ready()` costs
0.000 to 0.005 ms with a few hundred waiting segments, 0.245 ms with 64 k waiting at 128 k segments, and 8.3 ms with 500 k waiting at
1 M (a scan and sort at every `gc_step`; only a table in a long `Drifted` or `Rebuilding`-then-flood state holds that many).

## Deviations and open choices

* A third defaulted trait method, `take_mark_condemned`, and `GcConfig::tip_refs` (counting is off unless the daemon's
  key is on) are additions to the design; `GcConfig` is therefore in the `[refs]` paths, as is `stack_refs_cost.rs`.
* A table state `Drifted` (design 2.2) is kept: a transaction report that disagrees with the prediction.
* The shadow "offer" is made at every `gc_step` whatever the collector does; "idle" is the engine's last published collector
  state. The offer runs before `with_collector`'s tip check (correct for counting; the active stage must move it inside).
* Recall by bytes, the `RadixMap` variant of the unit proptest, N2, N1 and the ascending-chunks shortcut are not done.
* Recall is only measured where the table is `Ready` at the mark's start; the first mark after an attach is the base.
* Not run: the encrypted 64 K kill loop, `stack_kill`, T6. Nothing in this change touches the commit path
  kill points; the cut path gains one read per chunk.
* rust-skills: no conflicts with the project's conventions. `refs.rs` is large because of its tests (569 product lines).

## Remote disk use

The remote tree of this milestone was cleaned to the source tree and the logs (under 10 GiB) at hand-back.
