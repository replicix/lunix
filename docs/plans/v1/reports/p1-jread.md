# p1-jread: a guest journal read pins its ring file (H1 of the M15 zero-blocks audit)

Branch `p1/jread` from main `67c7769`. Remote: Linux 7.0.0, CPUs 8-15, `-j 8`.

## What and why

`JournalLane::submit_read` looked the ring file up with `find_in_use`, tried the resident window, then queued a plain file
read. Nothing held the file: `RingFile::inflight` counts batch writes only, and `finish_read` reported success on a full byte
count without asking whether the file was still the file. Reclaim (marks the file `Recycling`, drops its window entries,
zeroes `[4096, hi)`, renames, a lane may reuse it) could run between the lookup and the kernel's read (a read punted to
io-wq, a lane thread that polls late). The guest then got zeros, or bytes of the next incarnation, with a success status;
`queue.rs::on_jread` takes the map fallback only on an error status. After an attach that replayed records nothing is
resident, so every overlay read of a replayed record is a file read and the first cuts after the attach recycle exactly
those files: this is the window M15's zeros fit.

## The fix: pin, and reclaim waits (not validate)

* `RingFile::reads` (new): guest reads queued on the file and not yet reaped.
* `Rings::pin_read(lane, ordinal)` raises it **under the ring lock**, only while the slot is `Active` or `Used` and still
  carries the ordinal. Reclaim moves a file to `Recycling` under the same lock, so a pin is either refused (`ENOENT`,
  which the engine already turns into a map read) or visible to reclaim.
* `submit_read` keeps its first `find_in_use` (cheap early `ENOENT`, window hit path) and pins right before the SQE is built;
  the pin is stored in the `ReadReq` and released in `finish_read` after the bytes are copied (or the read failed), on a
  failed submit, and for reads still queued when a lane is dropped.
* `reclaim`: after the file is `Recycling` (no new pin possible) and the window entries are gone, it waits for
  `reads == 0` (`wait_reads_drained`: yield, then 100 us sleeps) before it zeroes. The wait is bounded (2 s per reclaim call since the review round, see there); on a
  timeout the file and the not-yet-processed candidates go back to `Used` through the existing reclaim error path and the
  next cut retries.

### Why pin and not validate

Validating at `finish_read` (record generation at submit, ENOENT if the file is no longer `Used` with it) is racy by
itself: the zeroing may already have begun when the CQE is reaped, a reclaim that failed halfway returns the file to `Used`
with the same generation and a partly zeroed body, and the check would pass; and the check would have to come before any
byte reaches the guest. The pin closes every case by construction: while a read is queued the file cannot be zeroed or
reused, so the bytes are the record's. Cost: one atomic add and sub per queued read, no extra I/O.

### Starvation, the per-file wake (ADR-33)

Reclaim is not deferred and not retried later: it waits, on the coordinator thread, for the reads of that one file, which
are single ring-file reads that finish in microseconds and are reaped on the lane's next wake (the eventfd is registered on
the ring). New reads cannot pin the file once it is `Recycling`, so the wait cannot be extended by traffic; it ends when
the reads that were already queued complete. The per-file wake is unchanged: the lane is woken right after the file's
zeroing and rename, as before; the only change is that this moment is later by the duration of the in-flight reads of that
file (zero in the common case). Parked writes are woken at the same points as before. A lane that never polls cannot hang
the coordinator for good (bounded, then the file goes back to `Used`). No reclaim is skipped, so there is nothing to
retry "promptly".

### The map fallback returns committed data

The fallback is taken only if `write_seq <= durable_cut` (`queue.rs::journal_read_failed`). `durable_cut` is stored at
`coord.rs:970`, right after C6 (the FULL is durable); reclaim is C9 (`coord.rs:~1045`), and `overlay.release` follows it.
A file is a reclaim candidate only if `max_seq <= max_cut_asked` and below the tail of the cut, so every record in a
reclaimed file is covered by a commit that is already durable when the file is zeroed. The map therefore holds that version
or a newer one, and a read refused with ENOENT for such a record returns it. A read of a record above the cut is never refused
by reclaim (its file is not a candidate) and, if it were to fail for another reason, fails the guest read instead of
serving an older version (unchanged).

### Destage's `read_at` and the read-around

Checked, and covered without change. `read_at` finds the file with `find_in_use_gen`, and a read-around window is parsed
against that generation (`scan_records`: the record header carries the generation, the checksum covers the data); a record
that was zeroed or replaced fails the check, and `read_around` then re-reads the slot: file not in use or a different
generation gives `NotFound`, not damage. Entries are made resident only while the file is still in use
(`insert_checked`). Moreover destage reads the records of the cut it is building, which are above `durable_cut`; their files
are not reclaim candidates until the commit that follows the build, so the race does not exist for it. One residual
that is not new and not a data-return bug: a reclaim that fails midway puts the file back to `Used` with the same
generation and a partly zeroed body; a read-around of a record in it would report `Corrupt` (damage) instead of `NotFound`.
Only an injected or real zeroing I/O error produces this, and nothing above the cut lives in such a file.

## Tests (each fails without the rule)

`crates/lunix-journal/tests/jread.rs` (journal level, window emptied by a restart, as after an attach that replayed):

| test | what it does |
|---|---|
| `reclaim_in_the_gap_before_the_read_is_queued_never_yields_zeros` | the new hook `FileJournal::set_read_gap_hook` runs `tail_after` + `reclaim` to completion between `find_in_use` and the SQE; the read must complete `ENOENT` (the bytes it would otherwise return are zeros) |
| `reclaim_waits_for_a_queued_read_and_the_read_sees_the_record` | a read is queued and not polled; reclaim runs on another thread and must not finish for 300 ms; after the poll the read returns the record, then reclaim finishes and the location is refused |
| (superseded by the review round) | the 60-round stress test of this row was dropped: it had no power (Mutation A passed it) |

`crates/lunixd/tests/stack_jread.rs` (real stack, `plain`, `integrity`, `encrypted`, 64 KiB chunks, small ring): 40 full
chunks in the map, then 300 4 KiB and 8 KiB writes into them that only the journal holds (every chunk partly overwritten),
flushed, drop without a commit, attach again (records replayed, nothing resident). The first journal read of the verify
pass runs a real cut (`commit_now`) and its reclaim through the gap hook; every one of the 10 240 blocks is compared with the
model, then once more with `read_all` after the cut. It also asserts the hook fired, the tip moved and ring files were
freed. Added to the `stack_*` list in `tools/ci.sh`.

### Fail-without-the-rule evidence (run on the remote)

Mutation A (the original behaviour: no pin, no recheck, the file of the first lookup used for the SQE; the wait removed
makes no difference without a pin):

* `reclaim_in_the_gap...` FAILED, `reclaim_waits...` FAILED (reclaim finished while the read was queued)
* `stack_jread::{plain,integrity,encrypted}` all FAILED: `block 1 (<mode>)` differs (zeros returned with success)
* the stress test passed too (no power; dropped in the review round).
  the pin together with the others but is not the fail-without evidence.

Mutation B (wait removed from reclaim, and the pin accepting any role): `reclaim_waits...` FAILED, the others passed:
the ordinal lookup of the pin already refuses a file reclaim has renamed, which is why the gap test passes here; the wait is
what protects the reads queued before reclaim started. (The role check and the wait were removed together in this run; they were
not mutated separately.)

The tree was restored after each mutation (checked by diff before the gate).

## Gates

* `tools/ci.sh` on the remote, final tree: green, `check-ownership: jread ok (8 changed paths)`, `ci: all green`;
  2 188 tests passed, 0 failed, 28 ignored (sum of the `test result` lines). A first run failed in the `stack_*` clippy
  step on a lint in my new test (`mistyped_literal_suffixes` on a seed literal); fixed, the run above is the second.
* Kill loops by hand on the same code (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`, release,
  `stack_kill`, the two loops at once, `-j 4` each):

| loop | points | reached | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|---|---|
| plain 16 KiB | 55 | 52 | 54 / 54 | 10 | 40 of 41 points | ok (1 706 s) |
| integrity 16 KiB | 55 | 52 | 57 / 57 | 10 | 40 of 41 | ok (1 963 s) |

## Deviations and choices

1. **`tools/ownership.toml` and `tools/ci.sh` changed**: `tools/remote.sh` refuses a milestone without a `[jread]` table, so I
   added one (journal sources, the two test files, this report, ci.sh, ownership.toml itself); the lead may fold it elsewhere.
   `ci.sh` runs `stack_jread` with the other stack tests.
2. **Test-only hook** `FileJournal::set_read_gap_hook` (`#[doc(hidden)]`, like the existing hooks) in `journal.rs`/`shared.rs`.
3. **The lane-level reuse-with-other-bytes case is not deterministic.** The gap hook runs inside `submit_read` on the lane's
   own thread, which therefore cannot append into the freshly reclaimed file; the zero-bytes variant is the deterministic
   one and the pin removes both outcomes by the same mechanism. (Superseded: the review round makes it deterministic with two lanes.)
4. **No on-disk format, interface or protocol change.** `lunix-iface` untouched.
5. **rust-skills conflicts:** none. (The wait uses `thread::sleep` on the coordinator thread, which is not async code;
   the 100 us poll avoids adding a condvar to every ring file for a rare wait.)

## Remote

Peak use of the milestone directory: 22 GiB (debug and release targets, the two kill loops' scratch), above the 10 GiB budget: the
release and debug targets plus two loops at once. Source, target and scratch were removed at the end.

## Review round

Review: `/data/cst/lunix-lead/01-p1-jread-review.md` (MERGE AFTER FIXES). All items done.

### Changes

* **M2 (timeout blast radius).** One deadline per `reclaim` call (`Instant` at entry, default 2 s, was 10 s per file). A file
  whose reads are not drained by then goes back to `Used` untouched, together with the rest of its ring; the loop goes on with the
  other lanes and the mark ring; the call returns `Err(TimedOut)` at the end ("retried at the next cut"). Files freed earlier in the
  call stay freed, counted and woken.
* **M3 (failed submit after a successful push).** `submit_read` now tells the two failures apart. A failed push queued nothing:
  complete with the error, free the id, drop the pin (the buffer is no longer leaked). A failed `submit` after a successful
  push keeps the request, buffer, id and pin and sets `submit_owed`; `poll` retries the submit (any later submit also flushes
  the entry), and the read completes normally. The id is never freed while its entry may complete.
* **L1 (reinit failure).** `SlotInfo::dirty`: set when a reinit fails (the zeroing may have begun), cleared when the slot is
  freed. A dirty file stays `Used` and a reclaim candidate (a later call redoes it) but `pin_read` refuses it, so guest reads take
  the map (the file is below a cut, so `write_seq <= durable_cut`).
* **L2 (tests).** The powerless stress test is dropped. The reuse case is deterministic with two lanes (below). The 300 ms sleep is
  replaced by a drain hook that fires when reclaim starts waiting.
* **L3.** The `read_gap_hook` mutex is behind an `AtomicBool` (one relaxed-cost load per window miss). `RingFile::unpin_read` is a
  real method used at all four release sites (debug-asserts against underflow) and the doc link is right.
  **Not done as asked: stamping `cut_alive` during the wait.** The journal crate cannot reach the engine's progress state without
  an `lunix-iface` change. Instead the limit is lowered to 2 s per call and the timeout path costs the coordinator at most that
  once per call; a drain timeout frees no file, which the engine already counts as "no ring progress", and a read stuck for
  2 s is a stuck queue thread. A `full_timeout` of 1 ms is allowed by the policy code, so no limit can be "below the smallest
  `full_timeout`"; if the lead wants the stamp, it needs a provided method on `Journal` (not done: interfaces are frozen).

### Tests (`crates/lunix-journal/tests/jread.rs`, 7 tests) and fail-without-the-rule evidence

Each row: the rule removed alone, on the remote.

| rule removed | tests that fail |
|---|---|
| original behaviour (no pin, file of the first lookup used) | gap test, cross-lane reuse test, reinit-failure test |
| `Recycling` accepted by `pin_read` (M1) | `a_file_that_turns_recycling_in_the_gap_is_refused_at_the_pin` only |
| `dirty` ignored by `pin_read` (L1) | `a_file_whose_reinit_failed_is_refused_to_reads_until_it_is_redone` only |
| the drain wait removed | `reclaim_waits_for_a_queued_read...` (the drain hook never fires) and the timeout test |
| a timeout aborts the whole call (old behaviour) | `a_drain_timeout_leaves_the_other_lanes_reclaimed...` only |
| a failed submit completes with an error and frees the id (old behaviour) | `a_failed_submit_of_a_queued_read_does_not_recycle_its_id` only |

* M1: `a_file_that_turns_recycling_in_the_gap_is_refused_at_the_pin`: the read gap hook starts a reclaim of two files on a
  thread whose free hook blocks after the first, so the second is `Recycling` when the read pins; the first lookup (which also
  refuses `Recycling`) had already passed. A plain read of a `Recycling` file is refused by `find_in_use` and cannot reach the
  pin, which is why the earlier version of this test (read while blocked in the free hook) passed with the pin arm removed.
* M2: two lanes, a read queued on lane 0's first file and never polled, a 300 ms limit: the call errors within the limit,
  lane 1's files are freed in the same call, lane 0's file stays and reads right (`read_at`), lane 1 can append, the read is
  then polled (right bytes) and a second call frees lane 0's file. The wake of a lane's parked writes is `wake_lane` right
  after each freed file, unchanged and not asserted here (the wake fd belongs to the lane object); the lane-1 append and freed
  counters show the files are usable.
* M3: the submit of the first read is injected to fail (`fail_next_read_submit`), a second read follows at once; both complete
  OK with the right bytes.
* L1: `fail_nth_reclaim_file(1)` makes the first reinit fail; the read of that file is refused, the retry frees it.
* L2: `a_cross_lane_read_of_a_file_reclaimed_and_reused_in_the_gap_is_refused`: lane 1 reads a record of lane 0's file; the gap
  hook reclaims it and lane 0 (another object) appends until it rolls into a freed file; the read must be refused. With the
  original behaviour it returns the success of the reused file.
* The drain-wait test now waits on the hook instead of sleeping.

`stack_jread` is unchanged and still fails without the fix.

### Gates (review round)

* `tools/ci.sh` on the remote, final tree: green (`jread ok`, `ci: all green`), 2 192 tests passed, 0 failed, 28 ignored. (A first
  run of this round failed only `cargo fmt --check` on my new test file; formatted with `tools/remote.sh fmt`, the run above is the next.)
* Kill loops one after the other (`LUNIX_KILL_CUT_MS=1 LUNIX_KILL_HITS=first LUNIX_STACK_REQUIRE_FSCK=1`, release, `-j 8`):

| loop | points | reached | runs / kills | partial cuts | kills inside a partial cut | result |
|---|---|---|---|---|---|---|
| plain 16 KiB | 55 | 52 | 54 / 54 | 10 | 40 of 41 points | ok (1 694 s) |
| integrity 16 KiB | 55 | 52 | 57 / 57 | 10 | 40 of 41 | ok (1 989 s) |

  (The ssh session timed out near the end of the second loop; the test process finished on the host and its log shows `ok`.)
* Remote disk: the debug target was removed before the loops; the milestone directory peaked at 2.3 GiB after that
  (about 10 GiB during the ci.sh run, debug plus release targets).
