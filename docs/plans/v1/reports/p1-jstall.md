# Report p1-jstall: the T6 write-stall fixes and the write-path instrumentation (ADR-32, ADR-33)

Branch `p1/jstall` from `origin/main` 2083779. Brief: the maintainer's approval of 2026-11-20 on the ranked fixes of
`t6-stall-analysis.md`. Builds, tests, the benchmark and the loops ran only on the remote test host (kernel 7.0.0) through `tools/remote.sh`, CPUs
8-15, `-j 4`, `CARGO_PROFILE_{DEV,TEST,RELEASE}_DEBUG=0 CARGO_INCREMENTAL=0`. No root run, no dependency change, **no on-disk format change** (the
reader rule of on-disk-formats §5.1 is checked, not changed).

## What was built

| # | item | what | where | tests |
|---|---|---|---|---|
| 1 | Ring pace | While the ring is at three quarters full, or a write waited for it in the last 5 s, the coordinator hands the space gate the drain rate of its last four cuts (journal bytes per second) through the new `SpaceGate::set_ring_pace`; it ends below half full with nothing waiting for 5 s. The gate's band takes the slower of the space curve and the ring pace: same bucket, same delay cap counted from arrival (`min(throttle_max_delay, full_timeout)`), same bypass of a FUA write of at most 256 KiB, same refusal rules. It is **no space state** (the throttle state, GC pressure and status `rate` do not follow it). Fed at each tick and right after each committed cut. No history, no rate, no pace | `lunix-engine/src/{cut,coord}.rs` (`RingPacer`, `update_ring_pace`), `lunix-space/src/{ledger,gate}.rs` (`pace_rate`), `lunix-iface/src/space.rs`, testkit `MemSpace` | `lunix-space/tests/pace.rs` (4: rate on an open gate and no space state, long-run rate, FUA bypass and delay cap, end of the pace and the slower curve), engine `jstall.rs` (`writes_are_paced_to_the_drain_rate...`, `a_write_that_waited_for_the_ring_keeps_the_pace_on...`), `RingPacer` unit tests |
| 2 | T9: ZERO_RANGE | A freed ring file is zeroed with `fallocate(ZERO_RANGE\|KEEP_SIZE)` over `[4096, hi)`; `EOPNOTSUPP`, `ENOSYS` or `EINVAL` turns it off for the journal's life (one `info` line) and the rewrite is used; any other failure rewrites that file. `JournalConfig::zero_range` (default on) selects the rewrite for tests and the benchmark. The order is unchanged: zero, `fdatasync`, header, `fdatasync`, rename, directory sync, and only then the lane's wake, so §5.1's "a valid header means zeros past the last write" holds. `FileJournal::reinit_counts()` | `lunix-journal/src/{ring,sys,config,journal}.rs` | `lunix-journal/tests/reclaim.rs`: `a_file_reinitialised_by_zero_range_...` and `..._by_rewriting_zeros_...` (a reused file holds one record and zeros after it; after a crash the recovery returns exactly the records above the cut, no damaged range, nothing ignored; the first asserts ZERO_RANGE was used when the file system does it); `sys.rs` classifies the errnos |
| 2b | Reclaim off the coordinator | **Not done**, see Deviations 2 | | |
| 3 | GC yields | While a write waits for the ring (`ring_waiters > 0`) and the gate applies no space pressure, the share after each cut is one collector step (the cut's own C10 step stays) and the compaction rate is an eighth of the configured one, at least 4 MiB/s, never above the configured rate; rate 0 stays 0. Under `raised` or `urgent` pressure nothing changes | `lunix-engine/src/{coord,pressure}.rs` (`gc_yields_to_ring`, `yielding`, `GC_YIELD_STEPS`) | engine `jstall.rs`: `the_collector_yields_to_the_cuts...` (2 steps per cut against 5 or more, rate 8 MiB/s against 64), `the_collector_keeps_moving_under_sustained_parking` (25 cuts, at least 2 steps each, never under the floor), `space_pressure_keeps_the_collector_whole...` (4 x rate and the whole share while parked); `yielding` unit test |
| 4 | First cut | With no drain history the budget is one ring file of journal bytes (the existing floors and the half-ring ceiling stay), not the half ring | `lunix-engine/src/coord.rs` | `jcut.rs`: `the_first_cut_after_an_attach_is_one_ring_file` (replaces the half-ring test) |
| 5 | Progress deadline | A parked write fails with `EIO` (`ENOSPC` as before after a commit failed for space) after `max(full_timeout, 60 s)` (`full_timeout` after a failed commit) **without ring progress**: counted from the later of its arrival and the last progress, a file freed or a cut that released journal bytes (an empty FULL is not progress). Every parked write of a queue that waits for the ring is judged (the front, the writes queued behind it, a small FUA write that overtook), at each wake and on the **ring timer thread** (new, `lunix-ring-timer`, one tick per `background_period`, takes no lock but the queue list's) which signals the queues while a cut holds the coordinator | `lunix-engine/src/{full,queue,engine,coord}.rs` | engine `jstall.rs`: `a_slow_but_progressing_ring_never_fails_a_write` (4 cuts 50 s apart, the write 200 s old, then admitted), `a_ring_without_progress_fails_a_write_at_its_deadline_within_one_timer_tick` (alive 1 ms before, failed at the tick at the deadline), `a_cut_that_released_nothing_is_no_progress`, `every_parked_write_is_judged_not_only_the_head`, `the_ring_timer_wakes_a_parked_queue_while_a_cut_holds_the_coordinator` (the real thread; the cut parked at C3; the queue's wake fd; wall clock only as a 20 s bound) |
| 6 | `write_path` status | `ring_wait_max_since_attach_ms` (renamed from `ring_wait_longest_ms`, now from arrival), `ring_wait_max_60s_ms` (from arrival, five-second steps), `ring_parked_writes` (all parked requests of queues that wait for the ring), `freed_files_by_lane`, `write_latency_log2_us` (32 buckets, `2^(b-1) <= us < 2^b`, arrival to acknowledgement of a write; a FUA write after its flush; failures not counted), `ring_pace_rate`. All atomics; the histogram has one set of lines per queue, summed at status time; the journal keeps the per-lane counters and `Journal::freed_files_by_lane` reads them. Nothing takes the coordinator mutex | `lunix-engine/src/{cut,full,queue,engine}.rs`, `lunix-iface/src/{journal.rs,ctl/msg.rs}`, `lunix-journal/src/{journal,shared}.rs`, testkit `MemJournal`, `lunixd/src/daemon.rs` | engine `jstall.rs`: `the_recent_longest_wait_counts_from_arrival_and_ends_after_60_s`, `every_parked_write_is_counted_not_only_the_waiter_at_the_head`, `freed_ring_files_are_counted_by_lane`, `the_write_latency_histogram_counts_writes_...`; journal `freed_files_are_counted_by_lane`; `RecentMax` and histogram unit tests; `status_lockfree` (new fields while the coordinator is held); the ctl round trip |

DESIGN §4.1 (Reclaim), §4.3 (every wait has a deadline) and §8.1 (the first cut, the ring pace, GC yields) are updated. `DECISIONS.md` is the lead's.

## ZERO_RANGE microbenchmark (the box4 file system)

`journal-zero-bench` (new, `lunix-journal/src/bin`): two journals of one lane and four 64 MiB files (the production geometry), `zero_range` on and
off, on the host's file system (an ext-family one, `statfs` type "ext2/ext3"), alternating every round so the shared device's drift hits both;
the other workload on the host kept the disk busy. Every file is full at the start; each round reclaims the oldest file (timed: the work the
coordinator waits for), appends 256 records of 4 KiB at queue depth 1 into the re-initialised file ("after reuse"), fills 32 MiB with 64 KiB
records at depth 8, takes the same 256 again ("mid file"), then fills the rest. 12 rounds.

| | reclaim of one full file, ms (p50 / p99 / max) | 4 KiB append after reuse, us (p50 / p99 / max) | same, mid file, us | fill, MB/s |
|---|---|---|---|---|
| `ZERO_RANGE` | **6.6** / 10.7 / 10.7 | 1124 / 2859 / 5288 | 1101 / 2916 / 4823 | 157.7 |
| rewrite | 213.1 / 240.7 / 240.7 | 1108 / 2762 / 8902 | 1119 / 2585 / 4996 | 157.7 |

Reclaiming a file costs 6.6 ms instead of 213 ms (a 32-fold drop: the whole per-file time of the coordinator is now an `fdatasync`, a header write,
a rename and a directory sync), and the feared cost of unwritten extents in the append path is not there on this file system: the first appends into
a reused file are as fast as the mid-file ones and as the ones after a rewrite. The T6 estimate (0.50-0.58 s per wake on the 64 MiB files of a
saturated device) is the rewrite's number under load. On a file system without `ZERO_RANGE` (tmpfs, some network file systems) the first reclaim
turns it off and the old path runs; that fallback is exercised by the rewrite test and the `zero_range = false` runs, not by a real refusing file
system.

## Failing without the rule

Each rule was removed in the source (one edit on the remote copy, restored after the run) and its tests run; the table is what failed.

| rule removed | tests that failed |
|---|---|
| gate: the ring pace is not applied (the band's own rate only) | `a_ring_pace_admits_at_its_rate_on_an_open_gate_and_is_no_space_state`, `a_ring_pace_holds_the_long_run_rate`, `ending_the_ring_pace_admits_what_it_held_and_a_slower_curve_still_rules`, `the_ring_pace_never_bypasses_the_fua_class_and_never_exceeds_the_delay_cap` |
| engine: the drain rate is not handed to the gate | `writes_are_paced_to_the_drain_rate_while_the_ring_is_nearly_full`, `a_write_that_waited_for_the_ring_keeps_the_pace_on_for_a_few_seconds` |
| journal: a freed file is not zeroed | both reuse tests (ZERO_RANGE and rewrite): stale bytes past the new records, a damaged range after the crash |
| journal: `ZERO_RANGE` is never used | `a_file_reinitialised_by_zero_range_...` (it asserts the file was zeroed by the range) |
| GC does not yield | `the_collector_yields_to_the_cuts_while_a_write_waits_for_the_ring` |
| GC gets no share step while parked (starved) | `the_collector_keeps_moving_under_sustained_parking`, `the_collector_yields_...` |
| GC yields under space pressure too | `space_pressure_keeps_the_collector_whole_while_a_write_waits_for_the_ring` |
| the first cut after an attach is the half ring | `the_first_cut_after_an_attach_is_one_ring_file` |
| the deadline counts from arrival | `a_slow_but_progressing_ring_never_fails_a_write` |
| an empty cut counts as progress | `a_cut_that_released_nothing_is_no_progress` |
| only the front of a queue is judged | `every_parked_write_is_judged_not_only_the_head` |
| the ring timer thread does not signal | `the_ring_timer_wakes_a_parked_queue_while_a_cut_holds_the_coordinator` |
| no 60 s window | `the_recent_longest_wait_counts_from_arrival_and_ends_after_60_s` |
| the wait counted from the first refusal (both sites) | the same test (one site alone does not fail it: the two sites back each other up) |
| only queue heads counted as parked | `every_parked_write_is_counted_not_only_the_waiter_at_the_head` |
| freed files not counted by lane (the fake, the file journal) | `freed_ring_files_are_counted_by_lane`, `freed_files_are_counted_by_lane` |
| no latency histogram | `the_write_latency_histogram_counts_writes_from_arrival_to_acknowledgement` |

Two harness findings: a first mutation made the long-run pace test loop for ever and one made the timer test hang inside `thread::scope` (the
parked cut was never released); both tests now fail instead of hanging (a bound in the first, the release before the assertion in the second).

## Gate

`tools/ci.sh` on the host on `1a04bcd` (the code and tests; the commit after it changes only this report): green, ownership included (`jstall ok (32 changed paths)`), `ci: all green`; 2 150 tests passed, 0 failed, 28 ignored (the sum of every `test result` line; the same
tree before the last test-only commit summed 2 166, the difference is lines the interleaved test output ran together in the log). The tests of the milestone: 14 in
`lunix-engine/tests/jstall.rs`, 7 engine unit tests (`RingPacer`, `RecentMax`, histogram, `yielding`), 4 in `lunix-space/tests/pace.rs`, 3 in
`lunix-journal/tests/reclaim.rs`, 1 in `sys.rs`. origin/main did not move (2083779), so no rebase.

By hand, `LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`, `cut_min_bytes = 1`, the child's commits `commit_bounded`
(`cargo test --release -p lunixd --features testsupport,crashpoints --test stack_kill`), the four loops at once on `bb62440` (the same code as
the gate head: `1a04bcd` changes tests only):

| loop | points | reached | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|---|---|
| plain 16 KiB | 55 | 52 | 54 / 54 | 10 | 40 of 41 points | ok (1 760 s) |
| integrity 16 KiB | 55 | 52 | 55 / 55 | 10 | 40 of 41 | ok (1 929 s) |
| encrypted 16 KiB | 55 | 52 | 54 / 54 | 10 | 40 of 41 | ok (1 815 s) |
| plain 64 KiB | 55 | 52 | 54 / 54 | 10 | 40 of 42 | ok (1 769 s) |

`stack_ring_recover` is a line of `tools/ci.sh` and passed in it.

## Deviations and choices

1. **Interface additions in `lunix-iface`** (not on disk; the lead's sign-off, as for jcut): `SpaceGate::set_ring_pace` and `Journal::freed_files_by_lane`
   are provided methods with a default body (no implementer outside the real gate, `MemSpace`, `FileJournal` and `MemJournal` changed);
   `WritePathStatus` gained fields, lost `Copy` (it holds two vectors) and renamed `ring_wait_longest_ms`. Per the no-compat rule there is no alias.
2. **Reclaim's per-file work stays on the coordinator.** Moving it to a thread of its own would make `Journal::reclaim` return before its files
   are free (its contract, the report counts, `reclaim_covered`, the tests and the C9 charge release rely on it) and would need a second state
   between "recycling" and "free" in the recovery of an interrupted reclaim. With ZERO_RANGE the per-file work is 6.6 ms (above), so there is
   little left to take off the wake path; not worth the contract change. The per-file wake still follows the rename and the directory sync.
3. **`ring_wait_*` now count from arrival**, not from the first refusal (a write can wait in the gate or behind another write first); the lifetime
   figure changed meaning with the rename, as the brief allows. `ring_waiters` keeps its meaning (writes refused a ring slot, one per queue front
   plus an overtaker); `ring_parked_writes` is the new count of everything parked in a queue that waits for the ring.
4. **Pace trigger.** Three quarters on, below half off, 5 s hold after a waiting write: constants in `cut.rs`, not configuration keys. The
   waiting-write evidence matters because a load on some queues fills their lanes while the ring as a whole stays under half (T6 §1.2: two
   lanes of four). The rate is exactly the measured drain rate (no headroom factor); the gate raises it to its minimum rate (4 MiB/s).
5. **GC yield constants** (one step, an eighth, 4 MiB/s floor) are the implementer's choice; the brief says "a small floor".
6. **The first cut and the older tests.** With the manual clock a cut has no duration, so the drain history never yields a rate and every cut of
   the default configuration was the half ring. They are now one ring file, so: `the_cut_follows_...`, `a_cut_is_at_least_...`,
   `cuts_without_journal_bytes_...`, `a_cut_is_never_more_than_half_the_ring` and `a_cut_is_never_less_than_...` start their history with an
   explicit `commit_now` (as other tests already seed); the unbounded model (`model_16k`, `model_64k`) sets `cut_min_bytes = u64::MAX`,
   because it asserts that a commit made everything written before it durable, which a prefix cut does not; the bounded model covers prefix cuts.
7. **`queue.rs`'s background-coordinator test** compared a device with a model that applied four overlapping concurrent writes in submission
   order. It failed on the branch after about 37 000 iterations (a block held the earlier write's data) and not on main. The cause is the
   first-cut rule, not a lost write: the test's manual clock gives a cut no duration, so the drain history never yields a rate and every cut of
   the default configuration was the half ring on main (also with `cut_min_bytes = 1`); now each is one ring file, the 64-file ring fills (an
   instrumented copy counted 831 iterations with a write parked for the ring, none on main) and a write on a full lane is journaled after one on
   a free lane whatever order they arrived in: two requests in flight have no order but their `write_seq`s (the copy counted 832 iterations in
   which the numbering differs from the submission order). The model now applies the four writes in `write_seq` order (`TestHooks::seqs`).
8. **Not tested:** the real file system's refusal of ZERO_RANGE (see above); the ring timer's period against the wall clock beyond the 20 s bound;
   the status through the CLI (`lunix status` text prints no write-path line; `--json` has the fields); `ring_pace_rate` under a real gate in a
   stack test (the gate's side and the engine's side are tested apart, as for ADR-32).
9. **Ownership.** `[jstall]` lists the 32 changed paths. `crates/lunix-engine/tests/{model,queue,jcut,common/mod}.rs`, `lunixd/src/daemon.rs`,
   `lunix-iface` and `lunix-space` files overlap other milestones' entries; the edits are local.
10. **Mutation harness.** Each removal was one edit restored with `git checkout` after the run; the working tree and the commits hold none of them.

## rust-skills conflicts

None.

## Remote disk

Largest sample 2.8 GiB (2.4 GiB of target, no incremental cache; the rest were two throwaway copies for the main-against-branch comparison of Deviations 7), with no incremental cache (the kill loops and the benchmark add under 1 GiB of scratch while they run). The remote
directory was deleted at the end.

## Commits

`bb62440` (code, tests, DESIGN, ownership), `1a04bcd` (tests that cannot hang when their rule is removed) and the commit of this report, all signed, on `p1/jstall` (pushed; not merged).

## Review round (MERGE AFTER FIXES: the Fable review of bb62440/1a04bcd/a9e09c2)

One more signed commit on `p1/jstall`. Same host, CPUs 8-15, `-j 4`, debug off. The review found no ordering violation.

| finding | what changed | test (fails with the fix removed) |
|---|---|---|
| C1 | Ring progress is **per lane**: `FullState` keeps, per lane, the freed-file count last seen and the time it last rose; the engine reads `Journal::freed_files_by_lane` right after every reclaim (C9 and `reclaim_covered`, also after a failed reclaim, which may have freed some lanes). A queue judges its writes against its own lane. The `cut_bytes > 0` stamp is gone | `a_lane_that_gets_no_file_fails_its_parked_writes_whatever_the_other_lanes_do` (lane 1 refused, lane 0 cuts and frees files every 19 s; the lane-1 write fails at its deadline; new `MemJournal::set_nospace_lane`); `a_slow_but_progressing_ring_never_fails_a_write` now frees a file per cut |
| C2 | A **running cut** is alive: `CutRunning` (a guard, set from the start of the cut loop until it ends in any way) and a stamp at every segment append and seal (the destage gets the segment store through a small `SealStamp` wrapper), at C3, C5, C6, C8 and after the reclaim. While a cut runs, the later of its last stamp counts as progress for every lane; once it ends only a freed file counts (so a lane the cut did not serve fails at its deadline right after it, as in C1). A failed commit shortens the limit to `full_timeout` only when it came after the write's last sign of progress (it is counted from there, not from the arrival), so a long live cut that then fails does not fail the writes parked before it at once | `a_cut_longer_than_the_deadline_that_keeps_sealing_does_not_fail_a_parked_write` (a cut of 100 s of manual clock, 25 s per seal through `GStore`; a thread polls the queue on the timer's signal; the write survives, and is admitted when the cut freed files) |
| C3 | New `ReserveClass::Retry`: a write the ring refused (`ring_since` set) retries through the gate without tokens, without the cap's overdraw, and does not make the requests behind it due; it waits only behind a space waiter | `lunix-space/tests/pace.rs::the_retry_of_a_ring_refused_write_takes_no_tokens_and_trips_nothing`; the stack test below |
| C3, stack | `lunixd/tests/stack_throttle.rs::sustained_ingest_on_a_small_ring_with_a_slow_drain_is_paced_and_never_fails`: the real gate, engine and store; four ring files of 4 MiB (`journal_tweak`), a store whose `fdatasync` and directory sync take 150 ms (`SlowFs`), 25 s of 256 KiB writes at depth 16 over a 32 MiB region. Asserts no failed write, a non-zero `ring_pace_rate` seen, `ring_paced_writes > 0`, the space throttle `open`, files freed, and the longest completion at most two cut durations + the 10 s delay cap + 5 s. Measured: 331 writes, none failed, the longest completion 3.03 s (cuts of at most 1.56 s: bound 18 s), ring pace applied at 4 MiB/s (the band's floor; the engine's own figure was 2.8 MB/s), 198 writes delayed by the pace (12.2 s in all, the longest 62 ms, none by the cap), the space throttle `open` with all its figures 0, 19 ring files freed, longest ring wait 1.6 s. | it is in `tools/ci.sh` (`stack_throttle`) |
| C4 | A `ZERO_RANGE` failure other than "unsupported" rewrites that file after a plain `fallocate(KEEP_SIZE)` over the range (a half-done punch cannot leave a hole), is logged once per errno, and after 3 failures in a row `ZERO_RANGE` is off for the journal. `FileJournal::inject_zero_range_errno` (hidden) makes the calls fail for tests and the benchmark | `a_zero_range_that_fails_falls_back_to_the_rewrite_leaves_no_hole_and_is_given_up_after_three` (ENOSPC from every call, five reclaims: 3 calls, 5 rewrites, no ring file with fewer blocks than bytes) |
| S1 | `journal-zero-bench` and the reuse and recovery tests on ext4, xfs and btrfs, nearly full, see the table below | `tools/jstall-zero-fs.sh` (root, through `tools/remote.sh root`) |
| C5 | In `queue.rs`'s background test every pair of writes of an iteration where the earlier was acknowledged before the later was submitted must have `seq(earlier) < seq(later)`; the log of numbers is emptied every iteration | the test |
| C6 | The ring pace's delays, caps, FUA bypasses and longest wait are counted apart (`ring_paced_writes`, `ring_paced_ms`, `ring_pace_max_ms`, `ring_pace_capped`, `ring_pace_bypassed_fua`, in the gate's figures and the status); the space throttle's `delayed_*`, `delay_*` and `bypassed_fua` stay what they were. `ring_pace_rate` of the status is the gate's applied rate (0 with the band off or closing). The ring-wait maxima (since attach, 60 s) now start at the **first ring refusal** (a write queued behind a refused one: from the later of its arrival and the refusal), so the time in the space gate is not in them | `the_ring_pace_is_counted_apart_from_the_space_throttle_and_only_while_applied`, `the_recent_longest_wait_counts_from_the_first_ring_refusal_and_ends_after_60_s` |
| C7 | A queue keeps `ring_refused`, the count of its requests the ring refused; the scans of the FIFOs, the verdicts and the gauge run only when it is not 0, and `ring_wait_parked` is that count. Not measured: the saving is a hash lookup per parked entry per poll while a queue holds paced writes; there is no per-poll benchmark here | `every_parked_write_is_judged_not_only_the_head` fails when the counter is not kept |
| S3 | The pace is the **sustained** drain: journal bytes over the time from cut end to cut end of the last four cuts, an interval capped at twice the cut's own time (an idle spell is not a drain rate) | `writes_are_paced_to_the_drain_rate_...` (half a second between the cuts), `cut::tests::the_sustained_rate_counts_the_gaps...` |
| S4 | The GC yield holds for 2 s after the last waiter | `the_collector_keeps_yielding_for_two_seconds_after_the_last_waiter` |
| C8 | The test count below comes from the `test result` lines of one run | |
| S2 | Not done: the order (zero, `fdatasync`, header, `fdatasync`, rename, directory sync) is unchanged and the first `fdatasync` follows the range; a dm-log-writes crash run on the three file systems was not made | |

### ZERO_RANGE on ext4, xfs and btrfs (S1)

Loop-device images of 3 GiB under the milestone's directory (`mkfs`, mount, run, unmount, detach, remove), one root run through `tools/remote.sh root`
(`tools/jstall-zero-fs.sh`). The benchmark is the one above with 8 rounds; the loop device is backed by the page cache of the host, so the append
latencies (microseconds) and the fill rates (GB/s) say nothing about a device: only the reclaim time, which is the metadata and sync work, and the
comparison between the two modes are meaningful. "Nearly full": a filler file leaves 8 MiB free (ext4, xfs; btrfs allocates its data chunks apart, the filler
filled the allocated ones and left 2.2 GiB unallocated), 6 rounds, then every ring file is checked for holes (blocks against length).

| file system | reclaim p50 / p99, `ZERO_RANGE` | reclaim p50 / p99, rewrite | append after reuse p50 / p99 (us), range / rewrite | nearly full, real file system | nearly full, `ZERO_RANGE` failing with ENOSPC | reuse and recovery tests |
|---|---|---|---|---|---|---|
| ext4 | 19.4 / 19.8 ms | 152.2 / 159.8 ms | 8 / 16 and 5 / 15 | 6 of 6 files by range, no hole | 3 calls, 6 files by rewrite, no hole | 4 ok |
| xfs | 12.4 / 12.7 ms | 151.7 / 167.2 ms | 10 / 20 and 6 / 25 | 6 of 6 by range, no hole | 3 calls, 6 by rewrite, no hole | 4 ok |
| btrfs | 22.2 / 110.5 ms | 135.9 / 222.6 ms | 10 / 21 and 9 / 19 | **`ZERO_RANGE` failed with ENOSPC (errno 28) for real**: 3 calls, then given up, 6 files by rewrite, no hole | 3 calls, 6 by rewrite, no hole | 4 ok |

The ENOSPC fallback ran for real on btrfs and by injection on all three; xfs and ext4 did not refuse with 8 MiB free. The ring files were never sparse. The tests of
the table's last column are the two reuse tests, `freed_files_are_counted_by_lane` and the failing-`ZERO_RANGE` test, run with their scratch directory on each file system.

### Failing without the rule (this round)

Each removal was one edit on the remote copy, restored after the run.

| rule removed | tests that failed |
|---|---|
| progress is one global stamp (any lane's file) | `a_lane_that_gets_no_file_fails_its_parked_writes_whatever_the_other_lanes_do` |
| a segment seal is no sign of life of the cut | `a_cut_longer_than_the_deadline_that_keeps_sealing_does_not_fail_a_parked_write` |
| the deadline counts from arrival | `a_slow_but_progressing_ring_never_fails_a_write` |
| a lane's progress stamped without a freed file | `a_cut_that_frees_no_file_is_no_progress` |
| only the front of a queue is judged | `every_parked_write_is_judged_not_only_the_head` |
| `ring_refused` not kept | the same test |
| the retry is an ordinary reservation | `the_retry_of_a_ring_refused_write_takes_no_tokens_and_trips_nothing` |
| `ZERO_RANGE` never given up | `a_zero_range_that_fails_falls_back_...` |
| the ring pace counted with the space throttle | `the_ring_pace_is_counted_apart_from_the_space_throttle_and_only_while_applied` |
| the pace follows the cuts' own speed | `writes_are_paced_to_the_drain_rate_while_the_ring_is_nearly_full` |
| no hold of the GC yield | `the_collector_keeps_yielding_for_two_seconds_after_the_last_waiter` |
| GC does not yield / yields under space pressure | the two GC tests of the first round (`the_collector_yields_...`, `space_pressure_keeps_...`) |
| `ZERO_RANGE` never used | `a_file_reinitialised_by_zero_range_...` |
| ring wait counted from arrival | `the_recent_longest_wait_counts_from_the_first_ring_refusal_...` |

Not separately detectable: removing the plain `fallocate` before the rewrite changes no test, because the rewrite itself writes the whole range and so
allocates it; the call is the review's belt for a rewrite that fails half way (ENOSPC on both).

### Gate of the review round

`tools/ci.sh` (CPUs 8-15, `-j 4`, debug off, `CARGO_INCREMENTAL=0`) on the final head: green, ownership `jstall ok (36 changed paths)`, `ci: all green`; 2 176 tests passed, 0 failed, 28 ignored (the sum of the `test result` lines of the log, one non-interleaved run). Kill loops by hand on the same code (`LUNIX_KILL_CUT_MS=1`, first hit, `cut_min_bytes = 1`; the report commit changes no code):

| loop | points reached | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|---|
| plain 16 KiB | 52 of 55 | 54 / 54 | 10 | 40 of 42 points | ok (1 744 s) |
| integrity 16 KiB | 52 of 55 | 57 / 57 | 10 | 40 of 42 | ok (1 963 s) |
| encrypted 16 KiB | 52 of 55 | 54 / 54 | 10 | 40 of 41 | ok (1 804 s) |
| plain 64 KiB | 52 of 55 | 54 / 54 | 10 | 40 of 42 | ok (1 777 s) |

Deviations of this round:
1. **The failed-commit path** keeps the ADR-32 rule (a write fails at `full_timeout` after a commit failed for space) but counts it from the write's last progress, a live cut included: a cut that sealed for a minute and then failed fails the writes parked before it `full_timeout` after its last seal, not at once and not never. A failed cut is no progress.
2. **Cut life only counts while the cut runs.** After it ends, a lane that got no file fails its overdue writes at once (C1 wins over C2 there, on purpose).
3. **`ring_wait_*` changed meaning again**, from the first ring refusal instead of the arrival (C6's choice; the brief's earlier text said arrival).
4. **S2** was not run (no dm-log-writes). **C7** has no measurement.
5. **A first CI run of this round failed once in `lunix-cli`'s `fsck_key`** with "another process holds the volume lock": two test processes of my earlier mutation runs, hung by mutated rules, were still alive on the host (they hold test volumes); I killed them (they were mine) and the whole gate then ran green but for the ownership line, which I fixed; the gate above is the run after that.
6. `tools/jstall-zero-fs.sh` and `lunixd/tests/stack_throttle.rs` are in `[jstall]`; the root run of S1 was refused once by the ublk-leftovers check while another workload's device was up, and ran when it was gone (retry loop, no bypass of the wrapper).

Remote disk: largest sample 2.6 GiB (target; the three loop images were 3 GiB sparse files of which under 1 GiB was used each, one at a time, and the kill loops add under 1 GiB). The remote directory, the loop images (removed by the script) and the scratch were deleted at the end.
