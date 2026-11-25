# p1-condemncut: C5 condemn-on-cut made active (`gc_condemn_on_cut = on`)

Branch `p1/condemncut` from `origin/main` fcacaf41 (not moved at hand-back). Design: `c5-recheck-design.md` section 2 (all
active parts), sections 3-5, milestone 3 of section 6; the two rounds of the refs review and `p1-refsoak-check.md` item 9
(must / should / later). Builds and tests ran only on the remote test host (kernel 7.0.0) through `tools/remote.sh`,
CPUs 0-15, `-j 16`. **No on-disk format change.** The evidence reason is the existing `unreferenced`; the table is memory
only. `recheck_verdict`, `Judge::verdict` and the delete path are unchanged (no line of them moved). T6 was not run.

## What was built

| item | what | where |
|---|---|---|
| key | `gc_condemn_on_cut = "off" \| "shadow" \| "on"` (default `off`); `CondemnOnCut::On` | `lunixd/src/config.rs`, `lunix-engine/src/config.rs` |
| `propose_dead` | `Collector::propose_dead(view, &[DeadSince]) -> bool` (defaulted: consumes nothing). `Gc` takes it only between passes (`Phase::Idle`, no `ctx`, list loaded); else `false` and the engine offers again. Rejects the whole offer while a pin exists; per segment: missing, `since > keep_floor` (`proposal_rejected_early`), above the high-water mark, named by a retained commit, an unnamed output, already listed (`_kept`), over `propose_max` or the list's room (`_room`). Then, in this order: `gc.propose.before_list_write`, `list_condemned` (list written and synced, `gc.condemn.after_list_write`), `flag_segments` (one durable index update, `index.after_condemn_flag`), `gc.propose.after_flags`, graces start. Nothing deletes, nothing is a pass (`passes`, `last_end` and the schedules do not move). It reads only `named`, the high-water mark and `keep_floor`, not the rollback floor or the manifest (no delete decision) | `lunix-gc/src/collector.rs` |
| carried evidence | the `condemn` lines (reason `unreferenced`) of proposals go to `Gc::carried`; the next pass (any kind, urgent too) starts its `PassCtx` with them. No evidence object per proposal. A crash loses them (documented) | `collector.rs`, `gc.md` |
| verdict bookkeeping | `proposed: BTreeMap<SegId, CommitSeq>` (memory only): an un-condemn counts `proposal_uncondemned` and logs the `Keep` reason, a delete counts `proposal_deleted` and bytes | `collector.rs` |
| N1 + N2 | one `Collector::take_mark_report() -> Option<MarkReport { started, finished: Vec<MarkFinished { refs, began_in_call, condemned }> }>` replaces `take_mark_started` / `take_tip_refs` / `take_mark_condemned`. The engine processes the finished marks in order (taking the snapshot first when a mark began and ended in the call), then the mark still running. `rebase` puts a snapshot of another tip back (R2-6) | `lunix-iface/src/gc.rs`, `collector.rs`, `lunix-engine/src/gc.rs`, `refs.rs` |
| offer inside `with_collector` | `gc_step` passes a closure `offer(c, view, skip); c.step(..)` to `with_collector`, so the offer runs after the `map_at_tip` check, under the same view (retain window from `view.params.retain_last`). Shadow and on share it; `skip` is the pending re-hints' segments | `lunix-engine/src/coord.rs` |
| pin rule | no segment is offered while `view.pins.roots()` is non-empty or unreadable (`pin_blocked` counts the steps), in shadow too, so `shadow_unconfirmed_named` is exact with pins; the collector rejects the whole offer again (`proposal_rejected_pins`) | `coord.rs`, `collector.rs` |
| table | `ready()` reads a per-cut queue (`CutZeros`, lazy deletion, head index): its cost no longer grows with the zeros waiting; `consume` (condemn lag, `proposals_taken`), `note_ready`, `note_collector` (`first_proposal_ms`, `first_proposal_delete_ms`) | `refs.rs` |
| `pending` shortcut | an ascending transaction (the destager's) keeps no `pending` map in `RefTxn::predict` | `refs.rs` |
| gate instruments | `first_proposal_ms`, `first_proposal_delete_ms`, `proposal_*` counters in `GcReclaimStats` (`proposal_rejected_early` must be 0, `propose_errors`, `proposals_deferred`, `mark_condemned_segments`), uncoalesced collector log lines for every change of the pressure level and of the grace (`gc: the engine's pressure level changed`, `gc: the grace in force changed`), cumulative cut counters in `write_path` (`cuts`, `cut_bytes_sum`, `cut_us_sum`: cuts that drained journal bytes, exact cost per journal byte); `lunix gc --status` prints them | `iface`, `collector.rs`, `lunix-engine/src/{cut,coord}.rs`, `lunixd/src/daemon.rs`, `lunix-cli/src/render.rs` |
| `GcConfig::propose_max` | 64, 0 takes none | `lunix-gc/src/config.rs` |
| crash points | `gc.propose.before_list_write`, `gc.propose.after_flags` in `CRASH_POINTS` | `points.rs` |
| docs | `gc.md` (modes, proposals, carried lines, instruments, the two crash points), key table of `lunixd/src/config.rs` | |
| ownership | `[condemncut]` | `tools/ownership.toml` |

Not done (as decided): DESIGN section 11 and the ADR-32 line (files other branches own); pressure grace below the 30 s floor;
the "later" items of item 9 (rebase merge, snapshot overlay, regular pass at the onset, compaction sources by re-hint, graces
at a restart).

## 1. The hung cost test (`the_shadow_table_cut_cost_is_reported`)

**Cause: not found; there is no hang to find. The test is slow and says nothing for long stretches in some invocations.**
Measured on the tree of this branch, host quiet:

- `LUNIX_REFS_CUTS=2 LUNIX_REFS_REPS=1`: finished in 478 s (both sizes, both arms).
- Defaults with `LUNIX_REFS_REPS=1` (20 cuts of 128 MiB): finished in 909 s, the four arms 3.5 to 4 minutes each (two fills of
  the 1 GiB volume: 140 s at 16K; the base pass 10.7 s; the cuts). The defaults have `REPS=4`: 16 arms, **about 61 minutes**.
  The 55 minutes of the refsoak run is a run that had not finished.
- Every phase prints (the per-cut INFO line of the engine appears every 2 s of the fill, a `fill` line every 32 MiB) and
  neither arm ever stopped in a phase: `fill`, `base pass` (`run_pass` of a whole pass: 10.7 s, the grace), the measured cuts
  and the close all moved in all 5 arms I watched.
- Added anyway: a `Progress` (phase + tick counter) and a watchdog thread per arm that aborts the process naming the phase
  when it makes no progress for `LUNIX_REFS_STALL_S` (default 600) seconds; per-phase lines; the module doc now says what a run
  costs. A real stall now ends with a named phase instead of silence. No product bug was found, so nothing blocks this
  milestone on this point; if the stall recurs the watchdog line is what to send.
- What could still be an environmental stall (not shown, not reproduced): the space gate reads the host file system's free
  space (`StatvfsProbe`); on a nearly full disk the fill would wait in `stop` with nobody to free space (`background = false`).

## 2. Tests (26 new test functions; line counts at the end)

| what | where |
|---|---|
| **"Any proposal is safe"**: `Op::Propose(seed, pool)` in the GC oracle, so in all seven oracles (16K/64K, id 16/32/0, plain/encrypted maps, the two recheck-only ones): random subsets of the dead segments or of every segment (live ones too), a random `since` (mostly the earliest cut, so only the recheck judges), missing ids, offered with pins, mid-pass, after crashes, faults and urgent passes. After every proposal and every pass every root and pin reads its model content; the volume converges. Coverage asserted: proposals condemned, un-condemned by a recheck and deleted are all non-zero. At 400 cases per oracle (`LUNIX_GC_CASES=400`) the five non-fast oracles passed with 8135-9458 proposals condemned, 6101-7509 of them wrong and un-condemned, 719-1450 deleted. **Negative control** (done, restored): a verdict that trusts a proposed segment (delete without the mark's live check) fails all seven oracles within the default 48 cases ("root N chunk M: unreadable") | `lunix-gc/src/tests/oracle.rs`, `sim.rs` (`propose`, `dead_segments`, `all_segments`) |
| `propose.rs` (13): order and durability (list, flags, grace; no evidence object; `passes` unchanged; carried lines named `unreferenced`; the next pass writes them), schedule untouched, busy collector refuses and takes it later, list not loaded, **pins reject the whole offer** and it is taken after the unpin, `since` above `keep_floor`, named / listed / missing, room and `propose_max` 0, **a wrong proposal is un-condemned by the recheck, unflagged and never deleted**, **N6: an urgent pass in the middle of a recheck-only pass deletes nothing live** (kind asserted `recheck`, state `Deleting` reached, then urgent), crash images at `gc.propose.before_list_write`, `gc.condemn.after_list_write`, `gc.propose.after_flags` (each reopens, reads, converges), the mark report (began / finished / `began_in_call`), **equivalence**: correct proposals vs none over 10 seeds end with the same segments (number and bytes: the ids of later segments depend on the packs and runs the passes' timing made) | `lunix-gc/src/tests/propose.rs` |
| crash sweep with proposals (3 tests): every crash point including the two new ones is hit by at least 5 workloads at 16K, 64K and lean, the workloads offering dead or all segments; the sweeps without proposals skip the two points | `lunix-gc/src/tests/crash.rs` |
| engine, active mode over random workloads with **pins** (`Op::Pin` on the newest commit, `Op::Unpin`), restarts and urgent passes (`the_active_mode_converges_to_the_same_files_and_obeys_the_pin_rule`): `on` against `off` read the same data and end with **the same segment ids** (unique data, no compaction, 14 more ticks and six more cuts after the unpin); the exact ground-truth oracle of the lag rule runs at every offer in `propose_dead` too (checked > 0, skipped 0); `proposal_rejected_early == 0` and `propose_errors == 0` at every tick; **no proposal condemned in a tick with a pin** (1073 pinned ticks, `pin_blocked` 373 in 400 cases); 1591 condemned, 1570 deleted, 0 un-condemned (no ids move without compaction). The shadow tests now have pins in their scripts too (`shadow_unconfirmed_named` is exact with pins). 600 cases passed. **Negative controls** (done, restored): lag 0 fails the oracle and the early-rejection check; removing the pin rule fails the oracle ("offered at tip 2 but a retained root names it") and the `pin_blocked` coverage | `lunix-engine/tests/refs.rs` |
| `refs.rs` units: the ready queue drains 10 020 zeros 64 at a time, oldest cut first, each once; a segment in `skip` does not hold back the ones behind it; a revival drops a queued zero; first-proposal timings; pin counter; an ascending transaction (the destager's shortcut) is predicted exactly (proptest); the R2-6 snapshot put back on a tip mismatch | `lunix-engine/src/refs.rs` |
| cut counters: `cuts`, `cut_bytes_sum` (equals the sum of `last_cut_bytes`), `cut_us_sum`; a cut without journal bytes is not counted | `lunix-engine/tests/refs.rs` |
| **pin rule on the real stack**: a snapshot pin sealed with the stack's own `MetaCodec` over the tip commit; six cuts of overwrites; 14 s of ticks: `proposal_condemned == 0`, `proposals_offered == 0`, `pin_blocked > 0`, data equal; remove the pin: proposals condemn and delete, data equal (28 s) | `lunixd/tests/stack_refs_pin.rs` (not in `tools/ci.sh`'s stack list) |
| **RadixMap**: the wrapper over the real `RadixMap` (8 leaves), 300 seeded runs of 14 transactions (ascending and unordered, sets, holes, re-hints that apply and stale ones, dropped transactions): the table equals a count of the map after every commit and no report mismatched. A seeded loop, not the `proptest` crate: `lunixd` has no `proptest` dev-dependency and dependency changes go through the lead | `lunixd/tests/stack_refs_radix.rs` (not in `tools/ci.sh`'s stack list); `lunix_engine::ref_map_counted` (testsupport) |
| daemon key parses `on`; the control-protocol test carries the new fields | `lunixd/src/config.rs`, `lunix-iface/tests/ctl.rs` |

## 3. Gate

| gate | result |
|---|---|
| `tools/ci.sh` (CPUs 0-15, `-j 16`) | green on the final tree: fmt, clippy `-D warnings` (workspace, and `lunixd` with `testsupport,crashpoints`), 2437 tests passed, 0 failed, 30 ignored, the crashpoint legs, the lunixd stack tests in release, ownership (`[condemncut]`, 30 changed paths all owned). One earlier run failed `wiring::roundtrip_plain_16k` once (`open: Store(Busy { what: "volume lock" })`, a path this change does not touch); 12 repetitions of the file passed and the next full run was green. Not reproduced, not investigated |
| kill loops (release, `LUNIX_KILL_HITS=all`, `gc_condemn_on_cut = on`, `gc_fast_recheck` on, seven loops at once on CPUs 0-15) | **all green**, fsck clean at rest, no "reference table" warning in any child log, `drift_segments == 0`, `report_mismatches == 0`, `shadow_unconfirmed_named == 0`, `proposal_rejected_early == 0`, `propose_errors == 0` after every recovery and at the end of every child, `rebases >= r0 + 2`: plain 16K 72 kills, 73 recoveries, 5219 s; integrity 16K 72 / 73, 5208 s; plain 64K 72 / 73, 5012 s; integrity 64K 72 / 73, 5415 s; encrypted 16K 72 / 73, 5563 s; recheck plain 16K 15 / 16, 1527 s; **condemn plain 16K (new arm) 27 / 28, 3031 s** |
| the new crash points in the kill loops | `gc.propose.before_list_write` and `gc.propose.after_flags` were reached by the counting run of every arm (asserted) and killed at their first and second hit in the five plain / integrity / encrypted loops and at the first, second and third in the condemn arm; the condemn arm also kills at `gc.condemn.after_list_write`, `index.after_condemn_flag`, `gc.recheck.before_delete` and the deletes. After a kill at a point a proposal passes, the condemn arm overwrites and cuts on the recovered volume and asserts a proposal condemned **and** deleted a segment (3 condemned and 1 to 3 deleted in each of the 13 recoveries it ran; first proposal 22 to 53 s after the attach) |
| the arms | `Plain`: a pass every second (proposals between passes); `Recheck`: the fastrecheck arm; `Condemn`: regular passes every 20 s with recheck-only passes between, so the proposals do most of the condemning. All three have the daemon's pipelined schedule |

The kill loops ran on the tree before one test-only edit: `gc.propose.*` points need one kill at the first hit instead of
the first, middle and last (`want.min(1)`, as `gc.recheck.*` already has `min(2)`): a proposal needs the table ready, two
cuts of lag and an idle collector, so a run has two or three hits and the count moves with the timing; a run that reaches
fewer hits falls back to its own last and the set of hits collapses. The loops passed without the relaxation (the counts held).
After the edit the plain 16K loop was run again on the new build restricted to `gc.propose.` (`LUNIX_KILL_POINTS`): 4 kills at hits 1 and 2 of both points, 5 recoveries, fsck clean, green (365 s).

### The cost test with the new counters

The test now reads the engine's cumulative counters (`cuts`, `cut_bytes_sum`, `cut_us_sum`) over its measured cuts and prints the
cost per journal MiB from them next to the sampled figure, per arm and per size from all cuts (`counters: N cuts, B journal
bytes, X ms per MiB of journal`). A functional run with `LUNIX_REFS_CUTS=2 LUNIX_REFS_REPS=1` (not a measurement; the counters
cover the in-engine C1-C9 time, so they are lower than the wall time of `commit_now` the test also prints): 16K off 1.413,
shadow 1.257 ms per MiB (-11.0 %); 64K off 0.875, shadow 0.891 (+1.75 %); two cuts per arm, so only that the counters work.
The earlier runs of this branch's tree (REPS 1, 20 cuts, host quiet): 16K sampled median +-0.1 %, 64K +0.5 %.

## 4. Findings outside this change

* **The recheck-only oracles fail on `origin/main` at 400 cases** (`LUNIX_GC_CASES=400 cargo test --release -p lunix-gc --lib
  oracle_fast_recheck`, both of them, on fcacaf41 without this branch): after an interleaved requested pass the ledger and the
  files differ (packs 8033 against 6728 bytes at 64K; at 16K packs 317474 against 303658). No data is lost and the 48 cases of
  `tools/ci.sh` pass; the ledger of an object whose delete failed after the unlink is released only when a later recheck finds
  it gone, and a recheck-only pass may not look at it. Not investigated further; it is in front of any 2 000-case gate run.
* A recheck mark that finds a segment dead which no earlier mark condemned does not condemn it (a whole sequence's recheck mark
  only judges): after the last cut an idle volume does not collect it until the tip changes. Seen while building the
  convergence test (it appends writes for that reason); not new.

## 5. Deviations, open choices, for the lead

1. The cause of the hung cost test is "none found" (section 1). If the lead has the log of the 55 minutes, the per-arm line
   `refs cost: 16 KiB off arm: base pass took` and the `fill` lines show where it stood.
2. `shadow` mode now withholds offers while a pin exists (it did not): `shadow_unconfirmed_named` is exact with pins, and the
   count of offers in a pinned volume falls. The soak scripts that read `shadow_*` on pinned cells will see fewer.
3. The collector's pin check and the engine's are both there (two rejections of one rule): the engine withholds (the segments
   keep waiting), the collector consumes and rejects (the marks find them). Both are cheap; the engine's reads the pins file at
   every step that has something ready (`PinSource::roots`). If that shows in the T6 profile, cache it per tip.
4. A segment the collector rejects as named, above the mark, an output or listed is consumed (the engine forgets it): the marks
   find it when it is due. `proposals_deferred` counts only calls refused as busy or not loaded, which the engine offers again.
5. `tools/ci.sh` does not run `stack_refs_pin`, `stack_refs_radix` or the cost test; both new tests pass by hand (28 s, 0.2 s).
   Adding them to the stack list is a one-word change in a file this milestone does not own.
6. DESIGN section 11 and the ADR-32 amendment line (design section 4) are not written (other branches own those files).
7. The pressure-onset line is the collector's own (`set_pressure` changes and grace changes, INFO, not coalesced); the first
   proposal time is from the table's creation (attach), as `first_ready_ms` is.
8. `GcStatusSnapshot` and `WritePathStatus` gained fields on the control socket (WIP, no compatibility duty).

## rust-skills

No conflicts. Test-only `Collector` wrappers forward `propose_dead` (a default that consumes would swallow every offer
silently; there is no wrapper in product code).

## Sizes

Product (non-test code), lines added: about 740 (`collector.rs` 240, of which about 100 is `propose_inner` and the trait
methods; `refs.rs` 190; `iface/src/gc.rs` 109; `coord.rs` 66; render 34; `engine/gc.rs`, `cut.rs`, `lib.rs`, `config.rs`,
`deps.rs`, `lunixd` 70) against about 100 removed. The design said about 250 for this milestone; the rest is item 9 (N1/N2,
the queue, the shortcut, the instruments, the pin rule in the engine). Tests: about 1 900 lines (`propose.rs` 552, kill
loop 220, `stack_refs_pin.rs` 195, `stack_refs_radix.rs` 188, engine tests 278 + unit tests 148, oracle 80, sweep 57, cost
test 112, sim 52). Docs: `gc.md` +72, this report. `git diff --stat` at hand-back: 31 files, +2 915 / -267 (the new files are `propose.rs`, the two stack tests and this report).

## Remote disk

The remote tree of this milestone was 31 GiB at the end of the work (the debug and release trees of the ci legs; the largest
is `target/`); it is deleted at hand-back (`target`, `target-kill`, `target-kill2`): what stays is the 31 MiB source and the logs.

## Review fixes (safety review `01-p1-condemncut-review.md`, merge after fixes)

Done on this branch (B1 is on `p1/orphandeps`; the B1 sim test, confirming script plus control, is added after that merge):

| item | what |
|---|---|
| S1 | three tests in `propose.rs`: nothing proposed goes before its grace ends (passes every second, `GRACE - 1 s`: all still exist, listed, flagged; then gone), the grace restarts at the load across a crash at `gc.propose.after_flags`, an urgent pass less than the grace floor after a proposal deletes none. The scene's clock starts at 1 000 s so that a grace start of zero is "long ago". **Negative control** (done, restored): grace start `Duration::ZERO` fails four tests (these three and the in-flight recheck test) |
| S2 | `GcConfig::propose_unfiltered` (test switch): a proposal skips the pin and `since` rejects. Three oracles use it (16K id16, 64K id32, 16K id16 with recheck-only passes); the witness asserts a segment only a pin reaches and one only the oldest retained FULL reaches were condemned by a proposal (124/123/153 and 160-526 per run at 48 cases). `delete()` under `cfg(test)` asserts the pass's mark found the segment dead (sees a wrong delete even when another copy hides it from the reads). At 400 cases (`LUNIX_GC_CASES=400`, rerun after this round): the five filtered non-fast oracles and `oracle_unfiltered_proposals_64k_id32` pass (pin-only witnesses 1167, oldest-FULL-only 1 700-3 600). Three fail: the two recheck-only oracles (the known unrelated ledger bug of section 4, `p1/recheckoracle`), `oracle_unfiltered_proposals_with_recheck_passes_16k_id16` (the same ledger signature, segments this time), and **`oracle_unfiltered_proposals_16k_id16`: "op 287 after a crash: root 13 chunk 9: unreadable" (segment 0xf)**, a real unreadable chunk after a crash and reopen with the proposal filters off. It is the shape of B1 (a wrong proposal flags a live segment, dedup writes a second copy, the first is deleted, the second is not durable when the process dies); I have not confirmed that. The 48 cases of `ci.sh` pass. Repro: `LUNIX_GC_CASES=400 cargo test --release -p lunix-gc --lib oracle_unfiltered_proposals_16k_id16`. To be rerun on the tree with `p1/orphandeps` merged |
| S3 | `GcReclaimStats::proposal_named` (a proposed segment a later mark found named by an older retained root, expected 0), asserted in `assert_refs_clean` and in the engine proptest; a test with the filter off shows it counts and the segment is kept. The recheck arm of the kill loops now runs `shadow` (its `shadow_unconfirmed_named` is a witness again); the others run `on`. `gc.md` says the `shadow_*` figures are not kept in `on` |
| S4 | `gc.propose.after_list_write` between the list and the flags, in `CRASH_POINTS`, the sweeps, and required (first hit) in every arm; the condemn arm requires a second kill at a later hit of the three `gc.propose.*` points (it got hits 1, 2, 3 of all three). The ratio un-condemned / condemned is printed |
| S5 | measured: a segment listing (`scandir` + `stat`, warm, 10 000 files) 12.5 ms, 64 000 files 100 ms. Now one listing per tip (dropped at every delete), the pins file read by the engine at most once a second and not at all while the collector is busy (the collector reads it again for each offer it takes). The call is 13 ms on the kill-loop volume (a few dozen segments): 3 calls took 38 ms, longest 13 ms; that is the durable list write and the index flag update, not the listing |
| S6 | `propose_calls`, `propose_us_sum`, `propose_us_max`; `mark_condemned_bytes`; `cut_map_ns_sum`, `cut_chunks_sum` (map-transaction time and chunks of all cuts); per-event lines: `refs: an offer was taken` (median and maximum of the time since each count reached zero, per offer) and `gc: a proposed segment was deleted` (bytes and recheck lag). Medians are computed from those lines; the status keeps sums, counts and maxima |
| S7 | `the_active_mode_is_safe_where_ids_move_dedup_and_compaction`: `on` against `off` over the dedup and compaction pools with pins, restarts and urgent passes: data equal, the oracle at every offer, `proposal_rejected_early == 0`, `proposal_named == 0`, no proposal in a pinned tick. 1 500 cases of each pool: 6 251 / 7 753 condemned, 6 198 / 7 635 deleted, **0 un-condemned**: the by-id class never occurred (the compaction re-hints always landed); the witness `uncondemned > 0` is therefore reported, not asserted, and the collector tests and the oracle cover a wrong proposal by offering one. The stale-pin cache needed one fix in the test (a pin made within a second of an offer trips the oracle, which sees the offer before the collector rejects it): `Op::Pin` and `Op::Unpin` advance the manual clock |
| deviations | `tools/ci.sh:97` now runs `stack_refs_pin stack_refs_radix` (and `[condemncut]` owns `tools/ci.sh`); `gc.md`: a long-lived snapshot or pin disables C5 |
| nits | N1 `last_pass_changed` set when a proposal condemns; N2 four unused ownership paths removed; N3 `gc.md`; N4 `proposals_deferred` documented as calls; N5 `gc.md` (lines lost with an unwritable evidence object); N6 the warning says how many segments were dropped; N7 doc fixed. N8 left |

Gate on this tree: `tools/ci.sh` green (CPUs 8-15, `-j 8`, 2 448 passed, 0 failed, ownership 34 paths); kill loops with `on` and `gc_fast_recheck` on, `LUNIX_KILL_HITS=all`: condemn arm 30 kills / 31 recoveries (3 403 s, all three `gc.propose.*` points killed at hits 1, 2 and 3), plain 16K 74 / 75 (5 315 s), integrity 16K 74 / 75 (5 384 s), and the recheck arm (now `shadow`) 15 / 16; all with fsck clean, `proposal_named == 0`, `proposal_rejected_early == 0`, `propose_errors == 0`, drift 0.
