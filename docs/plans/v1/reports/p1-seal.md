# Report p1-seal: segment seals off the packer thread (ADR-33 T8)

Branch `p1/seal` from `origin/main` 80e1cd1 (it did not move). Builds, tests, the benchmark and the loops ran only on the remote test host
(kernel 7.0.0) through `tools/remote.sh`, CPUs 8-15, `-j 8`. No root run, no dependency change, **no on-disk format change**: the segment files, their
footers, names and the order of their durable steps are byte for byte the ones of main.

## Design

Before: `Packer::append` called `SegmentWriter::seal` on the packer thread whenever a segment was full, and `destage_cut` sealed the last one
before the map transaction. Each seal is footer, `fdatasync`, rename, directory `fsync`; on a network volume 118-207 ms each, serially, while the
workers idled behind the in-order packer.

After (`lunix-destage/src/seal.rs`, `pack.rs`, `pipeline.rs`):

- `Packer::seal_current` hands the full `Box<dyn SegmentWriter>` to a **short-lived sealer thread** (`Sealer::submit`, std thread named
  `lunix-seal`) and returns at once; the packer opens the next segment and goes on appending. The thread runs `writer.seal(Some(range))` (the same
  call, the same store code) and records the outcome in a slot of a mutex-protected table under a condvar.
- **At most `DestageConfig::max_seals_in_flight` (default 4) seals run at once.** `submit` blocks while the bound is reached (counted as
  `seal_stall_ns`). `0` seals inline on the packer thread (the old behaviour; used as the A/B baseline and by the rule-removal tests).
- **Why a thread per seal, bounded, and not a pool or one dedicated thread.** A seal is 100+ ms of blocking I/O every 16 MiB (about 10 per second at
  most); a thread start is microseconds. A dedicated single sealer would serialise the `fdatasync`s again (a network volume can overlap them; the
  measurement below shows the one-in-flight configuration gains nothing at all). A pool adds a queue, a shutdown protocol and idle threads for
  nothing. The bound gives the memory and fd limit: each in-flight seal holds one open file and the dirty pages of one 16 MiB segment (the
  extent buffer is flushed to the file before the seal; the writer keeps its footer entries only), so 4 means about 64 MiB of dirty pages and
  4 fds per cut. A thread that cannot be started fails the cut with `Io` (transient), as a failed seal does.
- **Waiting.** `Packer::wait_sealed` (called in `run`, after the last `seal_current` and before the map transaction) waits until no seal runs,
  moves the successful ones, in segment order, into the list `DestageOutcome::segments` is built from, and returns the first error. `Packer::finish`
  calls it again, so the error path also waits for seals still running and the failed cut deletes every file that got sealed (a detached seal could
  otherwise finish after the cleanup and leave an orphan).
- **Failure semantics, unchanged from the engine's side:** a seal error fails the cut with that error (`Io`, `NoSpace`); nothing was applied to the
  map; the sealed segments of the cut are deleted by `destage_cut_with_cancel` as today; a seal that failed leaves its writer dropped (the `.open`
  file removed, or the sealed name left for the open-time sweep if the failure was in the directory sync, as before). A seal error already seen is
  reported at the next `submit` (fail fast, one segment granularity) and otherwise at the wait. A seal that panics is reported as
  `Invalid("segment seal panicked")` (class Never: the engine stops committing) instead of unwinding through the packer.
- **Early writeback (`sync_file_range`) was not added.** It is only worth it if it measurably shortens the `fdatasync` on the real device; this host
  has no network volume to measure it on and the brief says the device measurement happens later. The seam is the sealer thread (a
  `sync_file_range` call at each `WRITE_BUFFER` flush would go in the store's `ContainerWriter::append`, not in this milestone's paths).
- **Callers of `SegmentWriter::seal`:** (1) the destage packer (changed). (2) GC compaction, `lunix-gc/src/compact.rs` (`writer.seal(None)`): untouched; it
  seals synchronously on the collector thread and the output is named by a later cut (M17 rule 4), so its durability rule is as before. (3) The
  map pack seal (`PackWrite::seal`, C5) is a different object and stays inline in the coordinator. The store API did not change.
- **Engine side.** `SealStamp`/`StampedWriter` (coord.rs) stamps the cut's sign of life at the end of each seal, now from the sealer thread; it
  only touches an `Arc<AtomicU64>`.

### Instrumentation

`DestageStats` (`seals`, `seal_in_flight_max`, `seal_busy_ns`, `seal_wait_ns`, `seal_stall_ns`) -> engine `DestageOutcome` -> `CutCounters` ->
`CutStats` -> the status `write_path`: `seals`, `seal_in_flight_max` (the most in one cut since the attach), `seal_busy_ms` (summed seal time,
wherever it ran), `seal_wait_ms` (what cuts waited at the end of packing for seals still running: `busy - wait` is what the overlap saved,
`wait` is the part that was not hidden) and `seal_stall_ms` (what the packer waited for a free slot). This adds fields to
`WritePathStatus` in `lunix-iface` (ctl message): **an interface change**, the one the brief asks for (the status), additive, no trait changed.
`OpenParams::destage_seals` (lunixd stack, tests only) sets the bound; there is no `lunixd.toml` key.

### New crash points

`destage.seal_submitted` (the packer carried on while a seal is in flight on the sealer), `destage.before_seals_wait`, `destage.after_seals_wait`
(every seal done, map transaction not started). `destage.after_segment_seal` moved to the sealer thread (same place in the seal's life; a kill
takes the whole process). The store's own points (`store.segment.*`) now fire on the sealer thread, concurrently with the packer's
`destage.after_segment_append`; the kill loops enumerate every registered `destage.*` and `store.*` point, so the new interleavings are killed.

## Durability argument against C1-C9

C1 (wait for in-flight writes) and C2 (MVCC cut, ledger bounds) are before any segment; unchanged.

- **C3/C4 (destage; segments sealed; `new_segments` fixed).** Rule kept: *every segment the commit names is durable before the map pack and the
  FULL are written.* A segment is durable when its seal returned: footer and trailer written, `fdatasync` of the file, rename to the sealed name,
  `fsync` of `segments/`. That code is unchanged; only the thread differs. `destage_cut` returns only after (1) `seal_current` of the last segment,
  (2) `wait_sealed` returned `Ok`: **no seal is running and none failed**, and (3) the map transaction committed; `Packer::finish` (success path)
  waits once more. So when the coordinator reads `outcome.segments` (C4) every one is durable, and `new_segments` lists exactly them (the list is
  built only from collected `Done` slots, in segment order).
- **The map transaction (still C3, in memory).** It commits after the wait, so a guest read through the map never meets an `.open` file (the
  property DESIGN §5 step 7 gives). The map pack (C5, `seal_map`) and the FULL (C6) are written after `destage_cut` returned, hence after C4.
- **C5/C6.** Unchanged: the map pack is sealed, then the commit object is published with `new_segments`; a durable commit names only durable segments.
- **C7-C9 (checkpoint, floor, index inserts, reclaim).** Unchanged; index inserts still wait for the durable commit.
- **A kill at any point.** (a) During packing with seals in flight: some segments sealed, some `.open`, some half synced; no map pack or commit
  names any of them (they are written after the wait), so they are garbage: `.open` files and unnamed sealed files above the mark are removed by
  the open-time sweep or GC, as for a kill after `destage.after_segment_seal` today. (b) After the wait, before C5/C6: sealed unnamed segments,
  same. (c) After C6: named segments are durable by the argument above. The journal still holds every write of an uncommitted cut, so no
  acknowledged write is lost. (d) A failed seal: the cut fails, nothing is applied or named, the segments that did seal are deleted (best effort,
  otherwise the sweep).
- **What changed in ordering:** only that several segments' durable steps now interleave with each other and with packing. Segments are independent
  files whose only consumer is the commit, which waits for all of them.

## Tests (each rule has a test that fails without it)

`lunix-destage/tests/seal.rs` (a segment store whose seals take a configurable time and can fail, a map that notes what was sealed at its commit):

| rule | test | mutation (run on the host) | result |
|---|---|---|---|
| the map transaction commits only after every seal is done | `the_map_commits_only_after_every_seal_of_the_cut_is_done` (first seal 400 ms, three fast ones) | `wait_sealed` removed from `run` | fails: `(opened, sealed)` at the commit `[(4, 2)]`, expected `[(4, 4)]` |
| the same, at the file system: no map pack or commit object is published while a segment `fdatasync` runs or an `.open` segment exists | `lunixd/tests/stack_seal.rs::the_map_pack_and_the_commit_wait_for_every_segment_of_the_cut` (real store, a recording `FsOps` with a 250 ms segment `fdatasync`, 48 MiB, 3+ segments) | `wait_sealed` removed from `run` **and** from `finish` | fails (the violation list is not empty). With only `run`'s wait removed it still passes, because `finish` waits before `destage_cut` returns: the two waits back each other up, which is why the destage test spies the map's commit |
| a seal error fails the cut, leaves nothing, loses no write | `a_failed_seal_fails_the_cut_and_leaves_nothing_a_retry_cannot_redo` (the 2nd seal fails while the 1st runs; the 4th fails after packing ended) and `stack_seal::a_failed_segment_seal_fails_the_cut_and_a_retry_loses_no_write` (real store, injected `fdatasync` failure: the tip is unchanged, `segments/` empty, no map pack or commit published, a retry commits and every MiB reads back) | seal errors ignored (in `wait_sealed` and `submit`) | fails |
| seals overlap packing and each other (6 segments of 150 ms: about one seal, not the sum) | `seals_overlap_packing_and_each_other` | the sealer forced inline (bound 0) | fails (`inline took 900+ ms`, overlapped test asserts `< 450 ms`, `in_flight_max >= 2`) |
| the seals in flight are bounded | `the_seals_in_flight_are_bounded` (bound 2, observed concurrency exactly 2) | bound removed | fails (observed 6) |
| crash points registered | `the_seal_crash_points_are_registered` (feature `crashpoints`) | - | - |

The mutation of the bound and of the inline mode also failed `the_seals_in_flight_are_bounded`/`the_map_commits...` as listed by the runs (mutation B
failed three of the four tests, A three). Existing suites: all of `tools/ci.sh`, including the crash-point suites of destage, engine and store, the
stack kill tests in their default form, and `stack_ring_recover`.

## Benchmark: cut time with a slow `fdatasync`

`stack_seal::seals_beside_the_packer_shorten_a_cut_with_a_slow_fdatasync`: the real store and the real pipeline, a volume file system whose segment
`fdatasync` sleeps 200 ms first (a network volume), 56 MiB of incompressible data written and flushed, then one forced cut (`commit_now`), 4 segments,
64 KiB chunks, integrity mode, local disk of the host otherwise (`LUNIX_SEAL_DELAY_MS`, `LUNIX_SEAL_MIB` change the run):

| `max_seals_in_flight` | cut (C1-C9) | seals busy (sum) | cut waited at the end | packer stalled for a slot | in flight max |
|---|---|---|---|---|---|
| 0 (inline: before T8) | 917 ms | 852 ms | 0 | 0 | 1 |
| 1 | 903 ms | 854 ms | 210 ms | 628 ms | 1 |
| 4 (default) | **278 ms** | 863 ms | 219 ms | 0 | 4 |

The cut is 3.3 times shorter: the seals' 863 ms of work hide behind one `fdatasync` (the longest one) plus the packing. With one in flight there
is no gain (packing a segment is faster than its seal, so the packer just waits), which is why the bound is not 1. The test asserts that 4 in flight
takes under 70 % of the inline time. One run each; the figures are those of the first run of the test on the host. On the real device the gain is
bounded by how much its `fdatasync`s can overlap (to be measured there: `seal_busy_ms`, `seal_wait_ms` and `seal_stall_ms` in `write_path`).

## Gates

- `tools/ci.sh` on the host (CPUs 8-15, `-j 8`) on the code commit: **green**, `check-ownership: seal ok (19 changed paths)`, `ci: all green`;
  2 202 tests passed, 0 failed, 28 ignored (sum of the `test result` lines).
- Kill loops by hand on the same code (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`, `cut_min_bytes = 1`, the child's commits
  `commit_bounded`; `cargo test --release -p lunixd --features testsupport,crashpoints --test stack_kill kill_loop_<name>`), one at a time, CPUs 8-15.
  58 registered points now (the three new `destage.*` points included), 55 reached in each loop, each killed at least once, fsck required:

| loop | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|
| plain 16 KiB | 57 / 57 | 10 | 43 of 44 points | ok (1 814 s) |
| integrity 16 KiB | 60 / 60 | 10 | 43 of 44 | ok (2 068 s) |
| encrypted 16 KiB | 57 / 57 | 10 | 43 of 45 | ok (1 879 s) |
| plain 64 KiB | 57 / 57 | 10 | 43 of 45 | ok (1 836 s) |

  (A first attempt of the four lost its ssh connection after three minutes and left two loops running at once; I stopped them and killed their
  processes (my own, by binary path), and ran the loops again one at a time detached on the host.)

## Deviations and choices

1. **Ownership table:** `tools/ownership.toml` had no `[seal]` entry (`remote.sh` refuses an unknown milestone); I added one (see the review round for the trimmed list).
2. **Interface:** `WritePathStatus` in `lunix-iface` gained five fields (above). Nothing else in `lunix-iface` changed.
3. A seal that panics becomes an error (class Never) instead of unwinding on the packer thread.
4. No `sync_file_range`, no `lunixd.toml` key for the bound (default 4), see Design.
5. Error-path counters: a failed cut's seal figures are not added to the status (the stats of a failed cut are not, today either).
6. rust-skills: no conflict with the project's conventions found; `Sealer` uses `std::thread::Builder` and a poisoned-lock-tolerant `lock`, as the
   crate does; the one `unwrap_or_else(into_inner)` on the condvar wait mirrors `worker.rs`.
7. (First round only; reverted in the review round: the `DECISIONS.md` status edits.)

Remote disk: the build tree and scratch of this milestone, about 6 GiB (removed at the end).

## Review round

Findings F1-F7 of `01-p1-seal-review.md`, fixed on `p1/seal`.

| # | change |
|---|---|
| F1 | `Sealer` keeps the `JoinHandle`s and joins them in `wait_all` and in `Drop` (after `in_flight == 0`): no seal thread outlives its cut on any path. `Packer` has a `Drop`: a packer dropped without `finish` (a worker panic unwinding out of `destage_cut`) waits for its seals and deletes the segments they sealed, which nothing names. `finish` empties the packer first, so its drop does nothing. |
| F4 | `wait_sealed` deletes the segment of every `Failed` or `Taken` slot, best effort (idempotent; its thread has ended): a seal that failed after the rename (the directory `fsync`) or panicked after it no longer leaves a sealed file. What can still remain: a segment whose delete itself fails stays for the open-time sweep or GC, as for any segment of a failed cut. |
| F5 | `Slot::Pending`, an unreported `Taken` slot and a slot/segment count mismatch are hard errors in the collection loop: `panic!` in a debug build, `Invalid` and a failed cut in a release build (`invariant()` in `pack.rs`). Not reachable, so not testable. |
| F6 | `Packer::finish` returns the segments, the counters and the seal errors after the first; the error path now keeps the seal figures (`Partial::stats`) and `Partial::more_seal_errors` lists the further errors (the lunixd adapter logs them at `warn`). `DestageConfig::validate` refuses `max_seals_in_flight > MAX_SEALS_IN_FLIGHT` (64). `in_flight_max` no longer uses `as`. |
| F7 | Both `DECISIONS.md` hunks reverted (the file is identical to main). `[seal]` in `tools/ownership.toml` lists exactly the 19 files the branch changes, no `DECISIONS.md`. The five `WritePathStatus` fields stay (signed off). |
| F2 | The destage tests no longer assert a duration: a seal that must be slow blocks on a latch (`Gate`) that a helper thread releases when the state the test waits for is reached (`wait_for`, a bounded poll of the store's counters). The `one <= inline` assertion and every other timing assertion are gone. The benchmark is `#[ignore]` (run with `--ignored`); the other stack tests keep only their disk-order and error assertions. |
| F3 | New tests below; the doc comment of the stack ordering test now says it does not tell the wait in `run` from the one in `finish`. |

Tests of the round and their fail-without-rule evidence (mutations run on the host with `cargo test -p lunix-destage --test seal`; "hang" = the test binary did not
finish in 60 s and was killed: the broken rule loses a wake-up or a count):

| rule | test | mutation | result |
|---|---|---|---|
| no seal thread outlives a cut that unwinds; what it sealed is deleted (F1) | `a_worker_panic_unwinding_out_of_the_cut_leaves_no_seal_thread_and_no_sealed_file` (a worker panics at its 60th chunk seal with seals in flight, two held, one sleeping) | `Packer::drop` and `Sealer::drop` emptied | fails (a seal still runs, or a segment is left, on return) |
| a cancel with seals in flight waits for them and deletes what they sealed | `a_cancel_with_seals_in_flight_ends_the_cut_after_they_ended` (128 segments, window 3, seal 1 held, seal 2 sleeping 400 ms, flag set while the packer waits for a slot) | the wait in `finish` and the drops emptied | fails (a seal still runs on return); it passes with the drops alone emptied, because `finish` waits |
| a seal that panics becomes `Invalid`, the next cut works (F3) | `a_panicking_seal_fails_the_cut_with_invalid_and_the_next_cut_works` | `Completion::drop` returns early on a missing outcome | hang |
| a seal failure while the packer waits for a slot fails the cut (F3) | `a_seal_that_fails_while_the_packer_waits_for_a_slot_fails_the_cut` (bound 1, seal 1 held, then fails) | the wake-up of waiters after a seal removed | hang |
| a failure after the rename leaves no sealed file (F4) | `a_seal_that_fails_after_the_rename_leaves_no_sealed_file` (the seal renames, then reports an error); on the real store `stack_seal::a_failure_of_the_directory_fsync_after_the_rename_leaves_no_segment` (injected `segments/` `fsync` failure) | the deletes of `Failed` slots removed | fails (a segment is left in the store) |
| all failed seals are reported (F6) | `several_failed_seals_are_all_reported` (two fail; one error is returned, one more listed; the figures count 4 seals) | the list of further errors dropped | fails |
| the in-flight bound has an upper limit (F6) | `the_seals_in_flight_have_an_upper_bound` | the check removed | fails |
| first-round rules, now with latches | `the_map_commits_only_after_every_seal_of_the_cut_is_done` | `wait_sealed` removed from `run` | fails (four tests do) |
| | `seals_overlap_packing_and_each_other` (seal 1 held: the other three must pack and seal) | sealer inline | fails (waits 3 s for a state that cannot come) |
| | `the_seals_in_flight_are_bounded` (two held; the third must not start) | bound removed | fails (the cancel test too) |
| | `a_failed_seal_fails_the_cut_and_leaves_nothing_a_retry_cannot_redo` (first and last seal; no write lost; retry reads back) | seal errors ignored | fails (first round) |

The spawn failure of the sealer thread stays untested (it cannot be injected). Gates of the round:

- `tools/ci.sh` (CPUs 8-15, `-j 8`) on the code of the round: **green**, `check-ownership: seal ok (19 changed paths)`, `ci: all green`; 2 221 tests passed,
  0 failed, 30 ignored (sum of the `test result` lines; the ignored ones include the new benchmark).
- Kill loops by hand on the same code, as in the first round (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`), one at a time, each
  started detached on the host and polled (58 points registered, 55 reached in each):

| loop | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|
| plain 16 KiB | 57 / 57 | 10 | 43 of 44 points | ok (1 773 s) |
| integrity 16 KiB | 60 / 60 | 10 | 43 of 44 | ok (2 061 s) |
| encrypted 16 KiB | 57 / 57 | 10 | 43 of 45 | ok (1 913 s) |
| plain 64 KiB | 57 / 57 | 10 | 43 of 45 | ok (1 855 s) |

## Deviations of the round

1. The stall test and the panicking-seal test show a lost wake-up or count as a hang, not as an assertion; the harness cannot do better without a timeout in the code under test.
2. `Partial` (public in `lunix-destage`) gained `more_seal_errors`; the lunixd adapter logs it. No `lunix-iface` change beyond the five signed-off status fields.
3. A segment whose best-effort delete after a failed seal fails is not in `Partial::undeleted` (its id is not in the sealed list); it stays for the open-time sweep or GC.
4. `git checkout <sha> --` with no paths detached my worktree for part of the round; I put it back on `p1/seal`. Nothing was committed while detached and the tree content was the same, but one CI run and the first kill loop synced the tree while the report file was absent (no code effect).
