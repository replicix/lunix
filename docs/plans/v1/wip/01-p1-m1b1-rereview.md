# Re-review of M1.B1 blocker 1 (branch `p1/m1-b1` at 3a06625)

Scope: Fix A, findings 2-6, the D7 re-reading. Read-only. Gate re-run on the host
in a fresh directory (deleted afterwards): `cargo test -p lunix-iface --features
crashpoints` passes (contract 63, crashpoints 6, ctl 14, incl. the regression test).

## Verdict: **fixed** on the happy path; one failure-path sentence is missing

Every acknowledged write holds a `write_seq` issued before its `Append`
completion and the counter is monotone, so `upto = last_issued()` read at group
formation bounds every acknowledged write; the coordinator waits for every
slot or append `<= upto` on every lane (unappended slots included), then
syncs. Walked: `Full`/`NoSpace` consume no seq; an outstanding slot is waited
for (L33-39, L370-377; the fixture's `inflight` set holds reserved slots, the
regression test fails without the wait); reordered completions across lanes
are covered because the wait is by seq; FUA = `submit_group_flush(own seq)`
(ADR-28) waits for every lower seq, `upto < last_issued` is legal (say so for
M11; FUA is never mentioned); a crash between append and ack promises nothing,
a landed record replays as a legal image; damage above the floor is a missing
seq, recovery stops there, `gap_at`/`DamagedRange` report it. Fatal I/O after
a reservation: under-specified, problem 1.

## Remaining problems

**1. The failure path can still complete a FLUSH over a hole (contract gap).**
`journal.rs` L25-29 leaves poisoning to the *engine* (`fatal_io`) after
`append` returns `Io`; L379-380 says only "a later failure is passed to
`done`"; an `Append` completion with an error status is not covered at all
(`append` is called "infallible apart from a fatal `Io`"). Interleaving: A
reserves 9; B reserves 10, appends, completes, acks; FLUSH forms, `upto = 10`;
A's `append(9)` fails. An implementation that drops 9 from its in-flight set
satisfies the wait, syncs, calls `done(Ok)`; fsync returns; a crash leaves a
gap at 9, recovery discards 10. The engine *may* catch it via `is_read_only()`
when `done` fires; nothing requires that.
*Fix:* on `append`, `JournalCompletion::Append` and `submit_group_flush`: "an
`append` returning `Io`, an `Append` completion with an error status, or a
failed `fdatasync` **poisons the journal**: every pending or later group flush
with `upto >=` the failed seq completes with `Err(Io)`, and `try_reserve` /
`append` return `Io` on every lane; no FLUSH completes past a missing seq,
which is what makes the gap 'at the tail'." Add `JournalFixture::
fail_next_append(lane)` (default unsupported, test skipped) and one test:
reserve on A, write+complete on B, submit flush, fail A's append, `done` gets
`Err`.

**2. A dropped `Slot` is silent.** Pub fields, pub `new`, no `Drop`, no
`#[must_use]`; L28-29 only says "dropping one is a bug". The seq stays in
flight forever, so every later FLUSH and FUA (all have `upto >=` it) hangs
with nothing logged: not data loss, but a silent hang with no test.
*Fix:* (a) make the rule checkable: a lane with an outstanding slot returns
`Invalid` from any call other than `append(that slot)` and treats it as fatal
(problem 1's poisoning); contract test: reserve, drop, next `try_reserve` is
`Err`, a flush then fails. (b) `#[must_use]` on `Slot`/`SlotOutcome`, a private
`consumed` flag set by `pub fn take(self) -> SlotParts` (impls call it), `impl
Drop for Slot { debug_assert!(self.consumed) }`. (c) `append` returns `Invalid`
for a slot this lane did not issue (the `token`).

**3. Plan text is stale against the iface (lead).** D7 (plan L526), the M3
brief (L885-895, L1195-1210: `try_append`, "comes from the caller", wait "until
every lane's `completed_watermark() >= upto`", the finding-17 fallacy) and M11's
FLUSH rule describe the old contract; an M3 implementer reading the plan first
builds the wrong wait. Amend D7 to "taken from one global counter by the
writer's lane at reservation"; point M3/M11 at `journal.rs` before wave 1a.

Minor: `SlotOutcome::Full` (L157-159) parks later writes FIFO "to keep the lane's
records in order"; under Fix A order holds by construction. Report gate table is
stale (1f861de). M3: issue and register the seq under one lock.

## Findings 2-6 applied as intended; D7 re-reading consistent
2 any-lane `submit_read`, completed-append precondition in `journal.rs` and
`overlay.rs`, copy/alignment rule, cross-lane read tested. 3 `open(.., out) ->
OpenedMeta`, node cap, `Stored`+`verify_id`, order unobservable (16). 4
`all_runs`, `delete_run` caller rule. 5 `record_bytes` (64 + blocks·4096 / 64)
equals `Record::encoded_len`, used in `space.rs` steps 1, 5 and `ReclaimReport`.
6 `DamagedRange::{prev_seq,next_seq}`, `Recovered::gap_at`. Minors 7-18 present.
D7: DESIGN §4.1 only says `write_seq` is global and merged at recovery; ADR-3
says "assigned at admission", which `try_reserve` after the space `reserve` is;
the frozen `RecordHeader.write_seq` is "strictly rising within a lane", which
reserve-then-append-before-the-next-call guarantees; `scan_file` is untouched.
Only the plan's wording differs (problem 3).
