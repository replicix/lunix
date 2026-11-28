# p1-c5def: prerequisites for making `gc_condemn_on_cut = on` the default (2026-11-28)

Branch `p1/c5def` from `origin/main` 6774b437. The first run failed clause F as the design wrote it; the maintainer corrected the check (2026-11-28), the pin cell was rerun and passes, and **the default is flipped** (last commit). Everything else passed on the first run.
Spec: the lead's design note `c5-default-on-design.md` (sections 2.1-2.5 built, section 3 gate). Real-daemon measurement: branch `p1/c5def-t6` (harness `tests/perf/c5def.sh`, `bench/phase1/perf/c5def.py`
on top of the defcheck harness; results with paths scrubbed in `bench/phase1/perf/results/c5def/`, `tables.md`, `summary.json`). The binaries of the T6 are the release build of `p1/c5def` at b9b2f018 (code identical to the head; the head adds docs and this report).

## What was built

| item | where | result |
|---|---|---|
| 2.1 | `refs.rs` | no copy at mark start (`Open { sums, since }`); `rebase` decides a matching mark by two weighted sums and the entry total in one pass over the mark's list; any difference, and the first base, take the exact path (`mark + since`); debug builds run the exact comparison at every rebase and assert it agrees (`debug_assert`) |
| 2.2 | `SegmentStore::size` (defaulted), `LocalVolume`, `MemStore`, `SealStamp`; `propose_inner` | one lookup by id after the cheap rejects, `seg_cache` removed, `delete()` only lost the cache line |
| 2.3 | `Collector::set_shadow_judge` (defaulted), `Gc::note_dead`, engine open | **landed**; the lists `named`, `live`, `listed` are built only in mode `shadow`; `proposal_named` counted from the proposed side |
| 2.4 | `tests/refs.rs` | discard flood test, plus an offer-phase measurement (`LUNIX_REFS_FLOOD`) |
| 2.5 | `tests/refs.rs` | `Op::Pass` (requested pass) and `Op::Grow` in the proptest scripts, coverage asserted in all five drivers |

No lock, lock order, durable write or on-disk/`volume.json` format changed; nothing under `propose_inner` below the reject loop; `rebase` stays under the coordinator mutex.

## Bench (release, `lunix_engine::table_costs`, remote host, 20 reps per figure)

| per call | 128k segments | bound | 1M segments | bound | before (p1-refs2) |
|---|---|---|---|---|---|
| mark start | 0.02 us | <= 10 us | 0.03 us | <= 10 us | 0.21 / 8.8 ms |
| `rebase`, table right | 0.39 ms | <= 1 ms | 3.02 ms | <= 8 ms | 13.7 / 306 ms |
| `ready()`, half the segments waiting (consumed as the engine does, 1,000 per cut) | 0.0015 ms | <= 0.02 ms | 0.0024 ms | <= 0.02 ms | 7.09 ms at 1M (before the queue) |
| `rebase`, slow path (one segment differs) | 13.2 ms | expect <= 40 | 291 ms | expect <= 500 | - |

The slow path (13 ms at 128k, 0.29 s at 1M) is once per attach and once per drift; `gc.md` says so. `note_dead` at 128k live segments: 6.18 ms with the shadow lists, 0.0001 ms without.
2.2: a proposal of 64 on a volume of about 190 segments: `list` calls 0, `size` calls 1..64, before and after a deleting pass.
Flood (2.4, N = 20,000 chunks each in its own segment, one 320 MiB discard): zero_waiting peak 20,000; one proposal call (64 segments) before the first regular mark drained the rest; with passes held 30 s apart and a tick
every 100 ms (the daemon's rhythm) the proposals took **0.033 % of the 30 s window** (155 calls, mean 63 us, max 103 us, 9,920 segments condemned and deleted, zero_waiting 0 at the end). The 2 % stop condition was not reached. Limit: in-process, no fsync of the
list or flags; in the T6 (tmpfs store) the real mean per call was 0.40-0.51 ms, which at 10 calls per second is 0.5 %.
A first run of the same measurement gave 2.6 %: it was the default `size` (listing) of a test store wrapper; the wrapper now forwards `size`, as `LocalVolume` does.

## Tests and negative controls

* `any_difference_takes_the_exact_path` (a count off by k, a swap, +1/-1, a missing and an extra segment; table equals `mark + since`, `zero_since` empty), `the_sums_follow_the_counts` (proptest over apply, seal, start, rebase, failure, Rebuilding), `deltas_after_the_mark_start_survive_the_rebase`.
  Controls (run): both weights 1 -> the debug assertion "the sums matched a differing mark" fires (`any_difference_takes_the_exact_path`, `a_dropped_rehint_is_found_...`); no `since` update in `apply` -> 6 tests fail (the new delta test, `the_sums_follow_the_counts`, and four existing rebase tests).
* `a_proposal_looks_up_sizes_by_id_and_never_lists_the_store` (does not compile before `size`; control run: one `list()` back in `propose_inner` -> "no listing on the proposal path", left 1 right 0), `a_failed_size_lookup_fails_the_call_and_lists_nothing`, store test for `size` (sealed = listed size; `.open`, deleted, never existed: `None`).
* 2.3: `without_the_shadow_judgement_a_mark_condemns_and_reports_no_lists` (control run: ignoring the switch fails it), `a_proposal_a_retained_older_root_still_names_...` now runs with the switch on and off.
* 2.4/2.5: witnesses instead of controls (flood: `zero_waiting >= N/2` seen; drivers: `passes > 0 && grows > 0`).

## Gate (one build; pre-flip, the ON arm sets the key)

| clause | result |
|---|---|
| 3.1 CI | `tools/ci.sh` green: 2,555 tests passed, 0 failed, ownership ok; engine `--test refs` with `LUNIX_REFS_CASES=256` release: 10 passed |
| 3.2 oracles | `LUNIX_GC_CASES=400`: 17 passed (every `oracle_*`, unfiltered and pgrace included) and the two `should_panic` controls fail as designed. Hold-deferral share per oracle: 0.9, 1.2, 3.0, 0.0, 10.5, 0.0, 1.9, 1.6, 1.2, 3.4, 4.1, 2.9, 8.8, 7.7, 3.6 % (0-10.5 %, histories with pins and lost moves; the orphanhold2 report had 0-8.8 % for the ten oracles then run, the pgrace oracles were added since) |
| 3.3 kill loops | all six pass, `LUNIX_KILL_HITS=all`, release: condemn plain 16K 30 kills/30 recoveries (`gc.propose.*` killed at hits 1,2,3 each), recheck plain 16K (shadow) 15, plain 64K 74, integrity 16K 74, pgrace plain 16K 27, pgrace encrypted 16K 27; fsck clean at rest, 0 "reference table" lines, `drift_segments == 0`, `report_mismatches == 0`, `shadow_unconfirmed_named == 0`, `rebases >= r0 + 2` after every recovery, `propose_errors == 0`, `proposals rejected early 0`. The first launch lost four of the loops to an ssh session timeout; they were rerun to completion, the other two ran once |
| 3.4 A safety | **PASS**, all 20 blocks: fio 0 errors, verify clean, fsck 0, 0 reference-table lines, drift 0, mismatches 0 (rebases 65-142 in ON blocks), rejected early 0, named 0, propose errors 0, un-condemned 0 of 6,750-15,797 condemned, hold footer errors 0, exhausted 0, auth failures 0, `pgrace_min_wait_ms` 2,000-2,193, 0 EIO/ENOSPC lines |
| B rep 1 | **PASS**. 64K ON (b01, b04, b05): first delete 0.08 s, 0 stops, longest wait 0.9 / 1.6 / 1.8 s, throttled 4.9 / 4.7 / 3.7 s, 12.5 / 11.3 / 12.2 GiB in 5 s. OFF (b02, b03, b06): first delete 9.0 / 9.2 / 8.0 s, 0 GiB: the cell measures the gap. 16K ON: first delete 1.8-1.9 s, 0 stops (OFF 2.1-2.2 s) |
| C cut cost | **PASS**. 64K: ON 1,529 us/MiB, OFF 1,541 at the pooled cut size 233.6 MiB: **-0.8 %** (15 windows per arm, OFF se 0.88 %). 16K: 1,733 vs 1,699 at 217.5 MiB: **+2.0 %** (15 windows, OFF se 1.08 %). Zipf (reported): +1.1 % (10 windows). Map ns per chunk ON/OFF: 282/95 (64K), 446/242 (16K), 530/334 (zipf) |
| D steady, reps 2-5 | **PASS**. 64K: IOPS 63,671 vs 63,621, p99.9 0.625 vs 0.629 s, stops 0/0, longest wait 1.54 vs 1.71 s, mark share 3.12 vs 3.10 %. 16K: 64,574 vs 65,231 (-1.0 %), 0.730 vs 0.705 s, 0/0, 1.00 vs 1.01 s, mark share 7.95 vs 7.67 % (+0.28). Zipf: 70,984 vs 71,428, 0.369 vs 0.367 s, 0/0, 0.64 vs 0.69 s, mark 2.58 vs 2.49, compaction 99 % of OFF; evidence objects per minute max 25 |
| E proposal cost | **PASS**. Worst rep share of wall time 0.089 % (limit 2), mean 396-510 us per call (limit 5 ms), `propose_us_max` 0.83-1.6 ms (limit 20 ms); orphan deferrals 0, cuts requested 0 in all blocks |
| F pin cell | **PASS as corrected (rerun), first run FAIL as written, see below** |

### F: the pin cell (plain volume, pin held through reps 1-3, removed before rep 4)

First run (b19): `zero_waiting` peaked at 498 while `refs.segments` was 202-315, so the clause "`zero_waiting` never above `segments`" failed as written. The two are disjoint (`SegRefs::apply` drops zero counts from `counts`; `segments` is `counts.len()`), so the check was wrong, not the volume.
Corrected check (maintainer, 2026-11-28): while pinned `zero_waiting` <= the segment files that exist at that sample, and its per-rep peak levels off over reps 1-3; the rest of F unchanged. The sampler now lists the volume's segment directory at every 2 s sample (`nseg` in `series.jsonl`).
Rerun, ON and its OFF partner, the same code (release build of the b9b2f018 code; head differs by docs and the flip), results `bench/phase1/perf/results/c5def-pin/` on `p1/c5def-t6`:

| rep (ON, b01) | pin_blocked | offered / condemned | zero_waiting peak | segment files (max) | samples above the file count | IOPS | stops |
|---|---|---|---|---|---|---|---|
| 1 pinned | 365 | 0 / 0 | 425 | 1035 | 0 | 64,715 | 0 |
| 2 pinned | 342 | 0 / 0 | 484 | 1050 | 0 | 61,531 | 0 |
| 3 pinned | 345 | 0 / 0 | 524 | 981 | 0 | 60,706 | 0 |
| 4 | 5 | 2,690 / 2,690 | 256 | 958 | 0 | 60,171 | 0 |
| 5 | 0 | 2,634 / 2,634 | 242 | 875 | 0 | 60,968 | 0 |

`zero_waiting` is at most half the files (524 of 981+) at every sample. The pinned peaks 425, 484, 524 grow by 59 then 40 (+14 %, +8 %), a decelerating rise that stays far below the file count; I read that as levelling off (the check fails only if each rep is more than 10 % above the one before). Other ON clauses: after the release `zero_waiting` is below 64 again 9.2 s later (limit 30 s), condemned > 0 in rep 5, orphan deferrals 0, hold marks 0, clause A clean. OFF partner (b02, same cell, mode off): clause A clean, 0 stops, IOPS 57.5-64.3 k (ON 60.2-64.7 k); reported, not gated.
The first run's b19 data (not kept: see below) showed the same shape (peaks 428, 482, 498, bounded by about 706 referenced plus waiting segments).

### Shipping defaults pair (`throttle_pace_store_bytes` at its default, off; one block each, 64K w4k)

| | first delete s | stops | longest wait s | throttled s | GiB in 5 s | rep 1 IOPS | reps 2-5 IOPS | p99.9 s | stops 2-5 |
|---|---|---|---|---|---|---|---|---|---|
| ONS (key `on`) | 0.08 | 0 | 0.8 | 3.8 | 12.53 | 71,971 | 69,678 | 0.621 | 0 |
| OFFS (key `off`) | 8.80 | 1 | 5.8 | 9.5 | 0.00 | 64,836 | 71,791 | 0.612 | 0 |

ONS passes all five rep-1 clauses and A; OFFS fails four of them (first delete, stops, wait, the 6 GiB clause). Steady state is not clearly different (ON 2.9 % lower IOPS in reps 2-5, n = 1 each). Not gated.
Also run (optional cell, reported): restart in rep 3 of an ON block (b20): stop 2.5 s, start 0.8 s, attached 3.3 s after the stop began, rep 1 passes, 0 stops, clause A clean.

### After the flip

`tools/ci.sh` on the flipped tree: every step green (2,560 tests passed, 0 failed; no test needed an explicit mode); its ownership step failed once only because `[c5def]` lacked `lunix-engine/src/config.rs` (added; `check-ownership.sh` ok, 22 paths). No other step was rerun.

## Deviations from the design

* F: the design's check was wrong (disjoint quantities); replaced by the maintainer's corrected check and rerun (above). Everything the design asked for was built, and 2.3 landed.
* The first T6's results directory and its harness files were lost when the worktree was removed (`/tests/perf` and `/bench/phase1/perf` are in the local `.git/info/exclude`, so the new files were never committed). The tables in this report are from that run's `tables.md`; the harness was recreated and force-added in `p1/c5def-t6` for the rerun, and only the rerun's results are kept.
* 2.2 test: a volume of about 190 segments (64 dead per batch), not 2,000; the figure that matters (0 listings, at most 64 lookups) does not depend on the volume size. The negative control is the run with a listing put back, not a run on 6774b437 (the test needs `size`).
* 2.5: a requested pass in a manual-clock test must wait out its grace, so the test collector wrapper moves the clock a grace per step while a `Pass` op runs (same in every arm). Small test-support additions: `Vol::create_sized` (segment target) and a forwarded `size` in `GStore` (`tests/common/mod.rs`, which `[c5def]` now owns).
* 2.4: the flood volume uses a segment target of 1 byte (one segment per chunk) and 64 chunks per cut, because the in-memory index names one run per cut and a commit takes at most 4,096.
* The offer-phase share is an in-process figure (no fsync); the real-daemon proposal cost is clause E.
* Interface additions, both defaulted: `SegmentStore::size`, `Collector::set_shadow_judge`. Flip: `gc_condemn_on_cut` defaults to on at `lunixd/src/config.rs` (and its key table and parse test), `lunixd/src/stack/mod.rs`, `lunix-engine/src/config.rs` (and the enum's `#[default]`, a fourth site); ADR-32 addition of 2026-11-28, DESIGN 11, `gc.md`.
* rust-skills / digest: no conflict with project conventions.

## Clean-up

Remote tree, tmpfs and results copy removed; no daemon, fio or ublk device left. Commits signed, no trailers. No pricing.
