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
  `reads == 0` (`wait_reads_drained`: yield, then 100 us sleeps) before it zeroes. The wait is bounded (10 s); on a
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
the coordinator for good (10 s bound, then the file goes back to `Used`). No reclaim is skipped, so there is nothing to
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
| `reads_racing_reclaim_and_reuse_return_the_record_or_an_error` | 60 rounds: 10 records appended, 8 reads of older records queued, a thread reclaims the files below the cut while the lane keeps refilling the ring (reuse with other bytes); every completion is the exact record or ENOENT. Probabilistic (see below) |

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
* `reads_racing...` passed: it is a statistical test and the race is too narrow to hit in 480 reads on this host; it guards
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
   one and the pin removes both outcomes by the same mechanism. Reuse is exercised by `reads_racing_...` (statistically).
4. **No on-disk format, interface or protocol change.** `lunix-iface` untouched.
5. **rust-skills conflicts:** none. (The wait uses `thread::sleep` on the coordinator thread, which is not async code;
   the 100 us poll avoids adding a condvar to every ring file for a rare wait.)

## Remote

Peak use of the milestone directory: 22 GiB (debug and release targets, the two kill loops' scratch), above the 10 GiB budget: the
release and debug targets plus two loops at once. Source, target and scratch were removed at the end.
