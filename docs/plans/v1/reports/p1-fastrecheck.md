# p1-fastrecheck: C5-A, the recheck-only GC pass under pressure, and the gcbatch review follow-ups

Branch `p1/fastrecheck` from `origin/main` 4b2f87d. Design: `c5-recheck-design.md` sections 1, 3, 4, 5 and
milestone 1 of section 6; follow-ups: the round-2 section of the gcbatch review (R1-R5, N5). Builds and tests ran
only on the remote test host (kernel 7.0.0) through `tools/remote.sh`, CPUs 0-15, `-j 16`. **No on-disk format
change** (the list, the flags, the evidence lines, the manifest are written by the same calls as before; no
FREEZE item). The T6 performance measurement was not run.

## What was built

| item | what | where |
|---|---|---|
| cherry-pick | `destage_workers` daemon key, commit `abcb3ad` from `p1/pacea-t6`, as its own commit first | `lunixd/src/{config,daemon}.rs` |
| `Collector::set_pressure(raised)` | defaulted trait method; the engine calls it from `with_collector` (before every collector call) with `pressure_now() != Normal` (the 5 s hold is the tick's) | `lunix-iface/src/gc.rs`, `lunix-engine/src/gc.rs` |
| `PassKind { Whole, Pipelined, Recheck }` | replaces `whole: bool`; `due` returns the kind. `Recheck`: mark, condemn (D-A1: kept), deletes under the unchanged `recheck_verdict`, then `Finishing` (no compaction). `urgent_inner` sets `Whole` | `lunix-gc/src/collector.rs` |
| fast trigger | `cfg.pipeline`, `fast_recheck = Some(delay)`, pressure (or `fast_recheck_always`), no abort pending, `last_end + max(delay, 3 x last_mark)` passed, a listed object whose grace ended at least `delay` ago and after the last mark began | `collector.rs` `fast_due` |
| `last_full_end` | the regular triggers (`waited`, the "nothing changed" reset, `abort_pass`) are measured from it; `last_end` stays the end of any pass and carries the duty guard (a regular pass never follows any pass closer than 3 x last mark). A regular pass that is due wins. A recheck pass leaves `last_tip`, `last_pins`, `last_pass_changed` and `last_full_end` alone and only raises `last_pass_busy` | `collector.rs` |
| `GcConfig` | `fast_recheck: Option<Duration>` (None), `fast_recheck_always: bool` (false), also in `GcConfig::daemon()` | `lunix-gc/src/config.rs` |
| daemon keys | `gc_fast_recheck` (false), `gc_fast_recheck_delay_ms` (1000, 100 ..= 10000); `OpenParams::gc_fast_recheck`; a set `p.gc` keeps its own value when the param is `None` | `lunixd/src/{config,daemon}.rs`, `stack/mod.rs` |
| metrics | `GcStatusSnapshot::reclaim: Box<GcReclaimStats>` (`#[serde(default)]`): `fast_passes`, `regular_passes`, `fast_segments_deleted`, `fast_bytes_freed`, `recheck_lag_ms_{sum,max}`, `recheck_lag_count` (per deleted object, every pass kind: delete time minus grace start minus grace). `kind` on the `gc pass done` log line. `lunix gc --status` prints two extra lines, only when there is something to show | iface, collector, `lunix-cli/src/render.rs` |
| crash point | `gc.recheck.before_delete`, one site (`start_deletes`), in `CRASH_POINTS` | `lunix-gc/src/points.rs` |
| docs | `gc.md`: the pass kind, its rules, the point, the status lines, R2 wording; `lunixd` key table | `docs/reference/gc.md`, `config.rs` |
| R1 | `unmark_condemned_all`: contract case lines (empty set, unflagged segment, duplicate, reopen), a `lunix-index` proptest `Op::Uncondemn`, a directed index test; a crash-order sim test (below) | `lunix-iface/src/contract/index.rs`, `lunix-index/tests/batch.rs`, `lunix-gc/src/tests/batch.rs` |
| R2 | "listed and unflagged" in `gc.md` | docs |
| R3 | `store_list` takes `&mut self` and clears `list_dirty`; the delete arm skips its own write when the flush already stored the list | collector |
| R4 | `debug_assert!` in `finish_pass`: both queues empty and `!list_dirty` (the Recheck kind relies on it) | collector |
| R5 | `RETIRE_BATCH` is `pub` (`#[doc(hidden)]` re-export `lunix_gc::RETIRE_BATCH`), the kill loop uses it | `lunix-gc/src/lib.rs`, `stack_gc_kill.rs` |
| N5 | not done (see open questions) | |

## Tests

* `lunix-gc/src/tests/recheck.rs` (8 tests, 424 lines): a scene with a regular pass at 1 s (condemns X), one at 55 s
  (condemns Z, ends the regular schedule), a compaction candidate L at 58 s, `busy_interval` 20 s.
  - the headline: at 62 s with pressure a Recheck pass deletes X (grace 61 s) without any compaction (`compaction_read`,
    `compaction_written`, `segments_compacted`, `bytes_moved` are 0, no move, no output; a dry run still sees L as a
    candidate), keeps Z (its grace runs), the lag figure is exactly 1 000 ms, the oracle checks (roots, ledger) hold; then
    the regular pass at 80 s compacts L (**the `last_full_end` point on a schedule**);
  - no pass without pressure, with the option off, after pressure went away; `fast_recheck_always` needs none;
  - the delay (61.9 s no, 62 s yes) and the duty cap (mark of 2 s: 62.9 s no, 63 s yes);
  - the regular pass wins when both are due (fast passes stay 0, compaction runs);
  - an abandoned (failed delete) recheck pass switches the fast path off until a regular pass succeeded;
  - a request during a recheck pass: the pass in progress ends as Recheck, the next one is the whole sequence (waits
    out a grace);
  - sustained pressure (garbage every second for 180 s of fake time): recheck passes run, at least 5 regular passes
    anyway, the low-live segment is compacted within 60 s;
  - a Recheck and a Pipelined pass delete the same objects.
* `lunix-gc` oracle: `Op::Pressure(bool)` in the random workload; two new oracles `oracle_fast_recheck_{16k_id16,64k_id32}`
  with `fast_recheck = Some(1 s)`; the existing oracles gain the op too.
* crash sweep: `every_crash_point_with_recheck_passes_{16k,64k,lean}` (a recheck arm with `fast_recheck_always`);
  `gc.recheck.before_delete` must be hit by at least 5 workloads there and is not required elsewhere.
* R1 crash-order: `a_failed_list_write_after_the_uncondemn_flags_leaves_them_listed_and_unflagged` (batch.rs): the list
  write fails from its n-th call on, twice (the flush and the abort's retry); the run in which the flag swap happened is
  found by the testkit's `uncondemn_calls`; asserts flags cleared, list on disk still names the five live segments,
  then recovery (reopened: `reconcile` re-flags, the recheck un-condemns again; not reopened: the next flush stores
  the list), all five kept, roots read, ledger equal. **Proof it bites:** with the list store moved before the
  `unmark_condemned_all` in `flush_index`, the test fails ("no run failed the list write after the flag update");
  restored afterwards.
* engine: `the_collector_is_told_the_pressure_before_every_step` (`pressure.rs`, recorder collector: normal, raised,
  urgent, held, 5 s later normal, one call per step).
* daemon: `the_fast_recheck_keys_parse_and_are_validated`.
* iface: ctl proptest covers the new status field; socket snapshot unchanged (default `reclaim` prints nothing).
* kill loops (`stack_gc_kill.rs`): a recheck arm (`gc_kill_loop_recheck_{plain_16k,plain_64k,integrity_16k}`): the child's
  `GcConfig` has `fast_recheck: Some(1 s)`, `fast_recheck_always`, `pass_interval` 10 s, `busy_interval` 5 s (so
  recheck passes happen between regular ones); kills at the first, a middle and the last hit of `gc.recheck.*`,
  `gc.delete.*`, `gc.commit_delete.*`; the plain arms list `gc.recheck.before_delete` in `EXPECTED_UNREACHED`.

## Gate

* `tools/ci.sh` on the remote host: green (fmt, clippy `-D warnings`, `cargo test --workspace`: 2 333 tests, 0 failed,
  the crashpoints legs, the lunixd stack tests in release), and green again after the last edit.
* Kill loops, release, `LUNIX_KILL_HITS=all`, `stack_gc_kill`, `lunix-fsck` built with crashpoints, all green (one run,
  5 loops concurrently, 5 215 s): `gc_kill_loop_plain_16k`, `gc_kill_loop_integrity_16k`, `gc_kill_loop_plain_64k`
  (every `gc.*`, `index.retire.*`, `store.*` point; `gc.recheck.before_delete` unreached there, as expected) and the new
  recheck arm `gc_kill_loop_recheck_{plain_16k,integrity_16k}` (its own run, plus `plain_64k` in an earlier run):
  killed at `gc.recheck.before_delete` (first and a later hit) and at every `gc.delete.*` and
  `gc.commit_delete.*` hit (first, middle, last), recovery equals the oracle, fsck clean at rest. `stack_kill` (the
  separate commit-path kill loop) was not re-run: nothing in this change touches it.
* Test counts: 2 333 in the workspace run (the gc library tests are 99, of which 12 are new: 8 in `recheck.rs`, the crash
  sweeps x3, the oracles x2, the R1 crash-order test; plus the new index, contract, engine, daemon-config tests).

## Deviations from the design

* The fast trigger needs "a grace ended at least `delay` ago" and the **duty guard from `last_end`**; the design's
  `delay.max(last_mark * DUTY_FACTOR)` is used as written for the fast pass; the regular pass uses the guard
  `last_end + last_mark * DUTY_FACTOR` and its old `min_gap` from `last_full_end`.
* `next_kind` is not a field: `due` returns the kind.
* `GcStatusSnapshot::reclaim` is boxed: the field made `ctl::Response` 240 bytes and tripped
  `clippy::large_enum_variant` in `lunix-iface`.
* The kill loop's recheck arm needs only two distinct kill hits of `gc.recheck.before_delete` (the count varies from 2
  to 5 between runs, so a "last hit" is retried at its own last and collapses into the middle one).
* The CLI renders the counters (`recheck-only`, `recheck lag`) only when there is something to show.

## Open questions

* N5 (a directed test with a non-atomic index wrapper failing the n-th single call through `reconcile`) is not done: no
  non-atomic index wrapper exists in the testkit and the work does not touch the default body.
* D-A1 as decided (condemn kept in the fast pass). With `fast_recheck_always` off, a recheck pass never runs without
  pressure, so a volume with the key on and no pressure behaves as before.
* Evidence retention (`EVIDENCE_KEEP` 1 024) is unchanged: at a recheck pass per 3-4 s under sustained pressure the
  local history covers about an hour. Raise the constant if the lead wants longer history.
* The regular grace trigger (`busy_interval`, 5 s) still wins whenever a regular pass is due, so the advantage of the
  recheck pass is exactly the time between its delay (1 s) and the end of the regular gap (5 s); the T6 measurement
  decides whether that is worth it.
