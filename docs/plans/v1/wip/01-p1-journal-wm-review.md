# Review: journal flushed watermark (`lead/p1-journal-wm`)

- Reviewed: `lead/p1-journal-wm` at `9b60936`, base `3fe9a8a`. Commits `7d57130`, `4a99dda`, `dd2adb1`, `61ad263`,
  `345274f`, `9b60936`, with the note `docs/plans/v1/reports/p1-lead-journal-wm.md`.
- Read in full: `lunix-journal` (`coord.rs`, `lane.rs`, `journal.rs`, `scan.rs`, `ring.rs`, `shared.rs`), the record
  code and `scan_file` in `lunix-format`, `refuse_worthy` and its caller in `lunix-engine`, the `MemJournal` diff and
  its coordinator, recovery and crash model, all new tests, FREEZE.md, on-disk-formats §5.1 to §5.3, ADR-4,
  DESIGN §4.1 and §4.4, the plan paragraph, the `lunix-iface` journal rustdoc, and finding R1 of the M12a re-review.
- Nothing in the worktree was changed.

## Verdict

**Merge after fixes.** No blocker. One major needs a code change (M1), one major needs a wording change and the
maintainer's explicit confirmation (M2).

- The format change itself is sound and needs no further change: bytes 56-63, the CRC coverage, the reader rule
  and the compatibility story are right. The re-signed FREEZE.md text stands.
- The write-side invariant holds. I found no path that puts a watermark into a record that is higher than what is
  durable, as long as callers pass `upto <= last_issued()` (the engine always does; see m1).
- Failure mode (b), ordinary crashes refused: one path is left, a kill during journal reclaim (M1). It is not new
  (the base refuses it too), but it contradicts the rule this branch states, and the four-queue `kill -9` + `refuse`
  cell will hit it now and then.
- Failure mode (a), durable data wiped as residue: not possible without medium damage. With medium damage it is
  possible in the unwitnessed window, which is wider than the docs say, and in that window the new rule is weaker
  than the rule it replaces (M2).

## What I checked and found right

- **`flushed` only advances after a complete group flush.** `coord.rs:161` raises it only when every `fdatasync`
  succeeded and the poison point, re-read after the sync, is above `upto`. A failed or partial sync poisons at the
  lowest unsynced number and does not advance. A group with nothing dirty advances without a sync, which is right:
  every write at or below `upto` was reaped before the wait ended, and its file was taken and synced by an earlier
  group that succeeded (a failed group puts the dirty set back).
- **The wait is sound.** A sequence number is allocated and entered into `pending` under the lane's flight lock
  (`lane.rs:541`), so a flush that read `last_issued()` cannot miss a number at or below it. Abort, failed batch and
  dropped slot all poison **before** the number leaves `pending`.
- **A record never witnesses its own flush.** The header is encoded in `append` (`lane.rs:612`), while the number
  is still pending, so no flush at or above it can have completed. The clamp to `write_seq - 1` never fires with a
  correct caller.
- **Once poisoned, no record is appended any more**, so no record can carry a watermark from a poisoned state.
- **Recovery's own sync.** `prepare` zeroes the stale tail of every lane and syncs every file that holds a kept
  record before it sets `flushed` to the last recovered number (`journal.rs:190`); it runs to the end before any
  lane opens, and an error leaves `prepared` false. The value is at or above every watermark in a surviving record.
  A crash inside `prepare` gives the same recovery result again.
- **A torn or forged record cannot raise the watermark.** `flushed` is inside the CRC (`encode` computes the CRC
  after the field; `parse` checks it over the 64 header bytes). `scan.rs:266` takes the maximum over accepted
  records only. A record of an earlier file generation fails the `gen` check; a record of the same generation
  beyond the stop point is zeroed by `prepare` before its number is reissued, and reinitialisation zeroes up to
  `hi_off` under a new generation.
- **Stale records with a high watermark cannot survive a wipe** (also after real loss under `wipe`): within a lane
  `write_seq` rises along the files, so everything after the lane's last record below `next_seq` is zeroed.
- **Boundaries.** `gap_at == flushed` is refused, `gap_at == flushed + 1` is residue. The record whose number
  equals the watermark is itself durable; damage with `next_seq == flushed` is refused. A gap at or below the
  watermark always has a candidate beyond it (the witness has a higher number), so `gap_at` is always set for it.
  Empty lanes give 0. A lane holding only residue still contributes its (true) watermarks.
- **Compatibility.** Zeros read as watermark 0; the goldens pass unchanged; `flushed >= write_seq` is rejected in
  `encode`, `parse` and therefore in `valid_at`. With watermark 0 nothing is refused, as the note says.
- **`refuse` leaves the journal untouched**: the engine returns before any `open_lane` or `reclaim`.

## Majors

### M1. A kill during journal reclaim is refused, although no record is lost

`crates/lunix-engine/src/recover.rs:190-194` (the damage clause), `crates/lunix-journal/src/scan.rs:144-148` and
`216-226` (a bad file is reported as a damaged range, before the hint can skip it),
`crates/lunix-journal/src/ring.rs:328-340` (`reinit_slot`), `docs/reference/on-disk-formats.md:339-342`.

- `reinit_slot` zeroes the file, syncs, writes the new header (new ordinal), syncs, renames. Between the header
  write and the rename the file has a valid header that does not match its name. On-disk-formats §5.1 calls this
  state "a crash during reinitialisation" and says the file is reinitialised again at open.
- The scan reports that file as `DamagedRange { start: 0, end: file length, next_seq: the lane's next record }`.
  The lane's next record is old and almost always at or below the watermark, so `refuse_worthy` is true.
- Reclaim runs at every cut (C9). The window is one `fdatasync` per reclaimed file after `kill -9`, and extends to
  the directory `fsync` after a power loss.
- Probe (scratch copy, real `FileJournal`): 42 records on one lane, a flush after 40, the first file put into
  exactly that state, `recover(None, cut = 30)`. Result: 12 records, `gap_at = None`, `flushed = 40`,
  `damaged = [{ ordinal 1, 0..file length, prev_seq 0, next_seq 31 }]`, `refuse_worthy = true`. The volume would
  stay offline with nothing lost.
- A second, rarer shape of the same kind: a power loss that tears the first zeroing chunk of `reinit_slot` leaves
  records at or below the floor with a damaged range between them; `next_seq` is at or below the watermark too.

Fix (no format change), either or both:

1. Drop the damage clause: `refuse_worthy = rec.gap_at.is_some_and(|g| g <= rec.flushed)`. The gap clause alone is
   exact for loss above the floor. A destroyed record in `(floor, flushed]` is a missing number, and the witness
   of the watermark is a candidate above it, so `gap_at` is set and is at or below the watermark. Damage that
   destroyed only records at or below the floor costs nothing (DESIGN §4.1 step 3 says so already).
2. In `scan.rs`, do not report an interrupted reinitialisation as damage: header decodes, lane and volume match,
   ordinal differs from the name, bytes from 4096 on are zero.

Add a test for the state (journal level and through `Engine::open` if the testkit gets a "bad file" control), and
one sentence in on-disk-formats §5.3 and ADR-4.

### M2. The unwitnessed window is wider than "the newest flushed records", and inside it the rule is weaker than the one it replaces

`crates/lunix-engine/src/recover.rs:186`, `docs/explanation/DECISIONS.md:226-230`,
`docs/reference/on-disk-formats.md:464-465`.

- A completed flush is witnessed only by a record **appended after it completed**. Everything between the highest
  witnessed watermark and the last acknowledged `upto` is unwitnessed. For a burst followed by one FLUSH and then
  idle time, that is the whole burst.
- Probe: 10 records, one group flush (acknowledged), no later append, the data of record 3 damaged. Result:
  `flushed = 0`, `gap_at = 3`, `ignored_after_gap = 4..10`, `refuse_worthy = false`. Under `refuse` the volume
  opens and records 3 to 10, all acknowledged as durable, are wiped. The base rule refused this image (records
  beyond a gap).
- The same image after one restart and one new append is refused (probe 4: `flushed = 10`), so the mechanism
  works as designed. The exposure ends when a later record lands or a cut moves the floor past the records.
- This needs medium damage; a crash alone cannot produce it. Nothing on disk can tell it from residue after a
  power loss with a volatile cache, so I do not ask for a code change in this branch.

What I ask for:

1. Correct the wording. ADR-4 says the rule "closes the other direction too". It does so only for witnessed
   flushes; for damage followed by valid records in an unwitnessed range it opens what the old rule refused. State
   the window as "every record above the highest watermark a surviving record carries, which after a burst and one
   FLUSH is the whole burst", and say that the records after the damaged one are wiped with it.
2. The maintainer confirms this trade explicitly (the re-sign text mentions only "the newest flushed records").
3. Optional follow-up, outside this change: shrink the window. Candidates are a witness written when a group flush
   completes on an idle journal, or the watermark in the checkpoint slot. Both are format work.

## Minors

- **m1. A flush with `upto > last_issued()` plants a false watermark.** `coord.rs:161`, `journal.rs:251`,
  `lane.rs:624`. The trait does not forbid it. Probe: record 1, flush with `upto = 1000`, lane 0 reserves 2 and
  holds the slot, lane 1 writes 3: record 3 is on disk with `flushed = 2` while 2 was never written. A kill there
  gives a gap at the watermark and a refusal. The engine always passes `last_issued()`, so this is hardening:
  clamp `upto` to the counter read when the flush is submitted (or reject it with `Invalid`), and say so in the
  `submit_group_flush` rustdoc. The clamp in `lane.rs` hides the misuse instead of stopping it.
- **m2. DELTA regions do not write 0.** `lunix-format/src/journal.rs:525-531`: `encode_delta_region` copies
  `rec.header.flushed`; `decode_delta_region` accepts any value below `write_seq`. FREEZE.md and §5.2 say "DELTA
  and memfd: 0". Set it to 0 in the encoder and reject non-zero in the strict decoder, with one f02 case. No
  caller exists yet, so nothing on disk is affected.
- **m3. A duplicate `write_seq` above the watermark is now wiped silently.** `scan.rs:300-326` stops at a duplicate
  like at a gap and sets `gap_at` to the next expected number, which is above the watermark in the usual case. The
  base rule refused it. A duplicate is never crash residue (it means a wipe did not happen). Report it apart
  from a gap, or make it refuse-worthy regardless of the watermark.
- **m4. Test gaps.**
  - No test for M1's state, for m1, or for the watermark across ring wrap and reclaim with a checkpoint hint.
  - `random_crash_residue_always_opens_under_refuse`: writes are synchronous and the last flush is at block 20 of
    24, so each seed has at most three unflushed records and never a reserved or uncompleted append at the crash.
    More writes after the last flush and `Manual` completions held at the crash would cover R1's shapes better.
  - `MemJournal` is faithful on the points that matter (watermark taken at append, raised by a completed flush
    below the poison point and at the first lane open, torn records carry none). It differs in three places:
    it has no bad-header or reinitialisation state, it deduplicates sequence numbers (`journal.rs:826`), and it
    sees records behind a dropped record of the same lane where the real scan stops at a zero block. The last
    one only makes the model produce more shapes.
  - The real journal has only hand-made images. The real orderings (open batch at a kill, two batches in flight)
    are left to the four-queue `kill -9` cell, which is the right gate; run it after M1 is fixed.
- **m5. Docs.**
  - "pending re-sign" is still in `on-disk-formats.md:358` and the §5.3 paragraph, `DECISIONS.md:210`, the plan
    (`01-p1-local-volume.md:2259`), `lunix-journal/README.md:18`, `lunix-format/src/journal.rs:137` and the note's
    status line.
  - On-disk-formats §5.1 (interrupted reinitialisation is repaired at open) and the §5.3 rule (damage with a next
    record at or below the watermark is loss) contradict each other until M1 is fixed.
  - The `lunix-iface` module docs describe group flush and poisoning but not the watermark; one paragraph there
    would help, since `Recovered::flushed` is part of the contract now.
- **m6. Nit.** `Journal::recover` on a live object stores `flushed = floor` (`journal.rs:306`) while a group of the
  previous use may still complete and `fetch_max` an old `upto`. Only reachable when one `FileJournal` is recovered
  twice with a flush in flight; the daemon makes a new object per attach.

## rust-skills

No conflict and no finding in the changed code beyond the above. The atomics use `SeqCst` for the store and
`Acquire` for the lane's load, which is correct here; the new public field is documented; the builder method is
`#[must_use]`.

## What I ran

All on the host the lead named, from a scratch copy, one test thread, no root, one cargo job at a time. The scratch
copy and the remote source and target directories are deleted.

- `cargo test -p lunix-journal -p lunix-engine -p lunix-format -p lunix-testkit` at `9b60936`: exit 0,
  475 tests passed, 0 failed, 0 ignored.
- Four probe tests against the real `FileJournal` (scratch only, not committed), with the engine's `refuse_worthy`
  copied in:
  - probe 1 (M1): kill between header write and rename in reclaim: refused with no record lost;
  - probe 2 (m1): `upto` above `last_issued()`: a record claims an unwritten number durable;
  - probe 3 (M2): flushed, unwitnessed, one record damaged: not refused, 8 acknowledged records wiped;
  - probe 4: the same image after a restart and one append: refused, as designed.

# Mark ring review (2026-11-14)

- Reviewed: `lead/p1-journal-wm` from `09ae91a` to `12f7e95`: `bf6dcd1` (format), `1e19b30` (journal, iface, engine,
  testkit), `d4ea229` (docs), `12f7e95` (note). Targeted at the mark ring (the maintainer's answer to M2 above).
- Read in full: `coord.rs` (the mark scheduling and `write_mark`), the diffs of `journal.rs`, `scan.rs`, `ring.rs`,
  `shared.rs`, `lane.rs`, `config.rs`, the format and iface diffs, the engine's `wanted` and `commit` (C1) and where
  it calls `tail_after`, the `MemJournal` mark model, `tests/marks.rs`, the engine tests, FREEZE.md amendment (b).
- Under the no-compatibility rule I raise nothing about older layouts.

## Verdict

**Merge after fixes.** No blocker. Two majors need a code change (K1, K2). The record layout (kind 3, the mark
file, the golden) is right and can be re-signed as it is; item 4 of amendment (b) should be reworded first (k3).

## What I checked and found right

- **A mark never witnesses more than is durable.** The watermark a mark carries is `Shared::flushed`, which is
  raised only after a group's `fdatasync`s succeeded with the poison point above `upto`. The coordinator thread
  writes the mark between groups, so no group runs while it does. A poison that comes later does not undo a
  completed sync, so a mark already written stays true; a poisoned journal writes no further mark.
- **A mark is not written before its flush's sync.** The mark is scheduled after the group's sync and written in
  the coordinator loop after the group returned. After `prepare` it is scheduled after the last file sync.
- **The number of a mark leaves no gap and no duplicate.**
  - It is taken only after a place in the ring is found, so a full ring consumes nothing.
  - It is in no lane's pending set, but no group can run while the coordinator writes it, so no flush completes
    over it unwritten.
  - A failed encode or write poisons at the number.
  - A kill between taking the number and the write leaves a gap above the watermark, with only unflushed lane
    records after it: residue.
  - `resume_off` moves only after a successful sync, and a failure stops all later marks, so no block is written
    twice.
  - Stale marks at or above the new `next_seq` are zeroed by `prepare` with the lanes.
- **Mark ring reclaim against the cut is safe.** A mark file goes only when it is `Used` and its highest number is
  at or below `max_cut_asked`. The engine calls `tail_after(c)` only after C6 (`coord.rs:513`), so that value never
  exceeds the durable cut. The active mark file is never reclaimed. Nothing is released from the ledger for marks.
- **"A later flush makes the mark unnecessary" is sound.** `counter_at` is read after the watermark was raised, so
  every number above it was reserved and encoded after the raise and carries at least that watermark. A later
  group that completed unpoisoned with `upto > counter_at` made number `counter_at + 1` durable, whether it is a
  lane record or a mark. A stream of flushes cannot defer the mark for ever: a group that neither settles nor
  raises leaves the timer alone.
- **A lost or torn mark is residue.** Marks are written one at a time, each synced, so a torn mark is the last one
  and has no later mark behind it; a missing mark number with lane records after it is above the watermark.
- **A full ring behaves.** Probe: 1 020 marks, then `marks_skipped` counts, no number is taken, flushes keep
  completing, a cut frees the files, marks resume with the next flush, and recovery sees every number once.
- **The engine copes with numbers it never issued**: the cut takes `last_issued()`, marks are never listed, replay
  skips kind 3.

## Majors

### K1. The witness of the recovered watermark can be wiped by `prepare`, and no mark replaces it

`crates/lunix-journal/src/journal.rs:351` (`witnessed` is set to `rec.flushed`) and `:223` (the mark after
`prepare` is scheduled only when `counter > witnessed`).

- `rec.flushed` is the maximum over **all** scanned records, including those beyond the first gap. `prepare` wipes
  those. When the wiped record was the only witness and the watermark equals the last kept number, `counter ==
  witnessed`, no mark is scheduled, and nothing on disk witnesses the flush any more.
- This is the ordinary multi-queue crash: records 1 and 2 flushed, lane 0's record 3 still in its open batch, lane
  1's record 4 landed carrying watermark 2.
- Probe (real `FileJournal`, 5 ms idle delay): that image; first recovery gives `next_seq = 3`, `flushed = 2`,
  residue; lanes opened; 500 ms idle; `marks_written = 0`. Then the data of record 1 damaged: second recovery gives
  `flushed = 0`, `gap_at = 1`, `ignored = 2..2`, `refuse_worthy = false`. An acknowledged record is wiped as
  residue, a hundred idle delays after the open. The window stays open until the next write lands.
- This is outside the "crash within the idle delay" window the docs and amendment (b) name.

Fix: set `witnessed` to the highest watermark carried by a record that survives the wipe (number below
`next_seq`), computed in the scan, or set it after the wipe in `prepare`. Add the probe as a test (a gap across
lanes whose only witness is beyond the gap, then idle, then damage).

### K2. Every attach of a clean journal takes a number, which makes the engine commit an empty cut

`crates/lunix-journal/src/journal.rs:219-231`, `crates/lunix-engine/src/coord.rs:235` (`wanted`) and `:265-271`.

- After `prepare` on a journal with nothing above the floor, `counter == floor` and `witnessed` is the watermark of
  an old mark below it, so a mark is scheduled. It takes number `floor + 1` and witnesses `floor`, which a durable
  commit already covers.
- Probe: empty journal, `recover(floor = 7)`, lane opened, no write: `last_issued = 8`, `marks_written = 1`. A
  second attach with floor 8: `last_issued = 9`, one more mark.
- The engine half, by reading (the engine tests use `MemJournal`, which writes no mark at open): `wanted` sees
  `last_issued() > tip.cut_seq`, and after `commit_interval` `commit` finds `c > prev` and runs C2 to C10 with an
  empty cut. So each attach of an idle volume costs one mark and one FULL commit that changes nothing. The plan's
  "an idle volume cuts nothing" no longer holds.
- The same happens once when a cut lands inside the idle delay after a flush: the mark follows the cut, takes the
  next number, and the next interval commits it alone.

Fix: write no mark for a watermark the cut already covers. In `prepare`, schedule only when records above the
floor were recovered; in `write_mark`, return when the watermark is at or below `max_cut_asked`. A test at the
engine level needs a `MemJournal` that models the mark at open, or a check through `lunixd`.

## Minors

- **k1. Ring full: the window reopens with a counter only, and stays open after the cut.** `coord.rs:275-279`.
  Is it acceptable: yes for merge, if it is said. Reaching it needs more than 765 to 1 020 idle flushes between
  two cuts, which at the default delay is at least 38 s of flush-then-idle traffic with no successful commit, in
  practice a store outage. Three things to do:
  - the skipped watermark is dropped, not retried: in the probe no mark came in the 100 ms after reclaim freed the
    files, only with the next flush that raised the watermark. Keep the due mark and write it when reclaim frees a
    file;
  - log once at warn when skipping starts, and export `marks_skipped` (the note lists the export as a follow-up);
  - add a test (the probe runs in under 8 s with a 1 ms delay).
- **k2. A damaged durable mark: refusing is right; under `wipe` it costs data although a mark holds none.**
  `scan.rs:335`. Probe: records 1 to 3, flush, mark 4, records 5 to 7, flush, mark 8; mark 4 damaged. Recovery:
  3 records, `gap_at = 4`, `flushed = 7`, refuse-worthy; the damage is reported under lane 65535. Under `refuse`
  that is the right, conservative answer: the journal has medium damage inside witnessed history and recovery
  cannot prove the missing number was a mark. Under `wipe` (the default) records 5 to 7, acknowledged data, are
  wiped because a block without data was lost. Each idle flush adds one such block. A decision for the
  maintainer: either say so in ADR-4, or let recovery bridge missing numbers that lie between `prev_seq` and
  `next_seq` of a damaged range in the mark ring when their count is at most the number of damaged blocks (the
  mark ring holds only marks). I would not put the bridge in this branch.
- **k3. FREEZE.md amendment (b).**
  - Item 4 says "The remaining window is a crash within the idle delay". Also open: the ring-full case (k1),
    `mark_idle = None`, and K1 until it is fixed. Name the ring-full case, or move the "when" rules out of the
    format freeze into ADR-4 and keep only the layout and the reader's rules in FREEZE.md.
  - Item 4 says "after a restart, once the first lane open made the recovered records durable". After K2's fix
    that is "when records above the floor were recovered".
  - Item 3 says marks exist only in the mark ring. The reader does not enforce it: the scan accepts kind 3 in a
    `q` file and kinds 1 and 2 in an `m` file (the latter would reach the engine with lane 65535). Not reachable
    without forging a CRC and the generation, but the normative text and the reader should agree: treat a record
    of the wrong kind for its ring as damage.
  - A mark file's header is a lane-0 header; only the name tells it from `q00`. Worth one sentence.
- **k4. An empty FLUSH after a mark writes another mark** (by reading, `coord.rs:229`): the group's `upto` is the
  mark's number, which raises the watermark and schedules a mark that witnesses only the previous mark. One per
  empty FLUSH, harmless; skip it when every number in `(before, upto]` is a mark.
- **k5. Costs to state in the docs.** A failed mark write poisons the volume (necessary, the number is taken). The
  mark is a synchronous write and `fdatasync` on the coordinator thread, so a FLUSH that arrives during it waits
  for one extra sync.
- **k6. Nits.** A due mark is dropped at shutdown (a clean stop without a final cut leaves the last flush
  unwitnessed). A failed allocation of the mark buffer disables marks with no counter. `recover` on a live object
  now also races a mark in flight (m6 above).
- **k7. Tests.** `MemJournal` writes the mark at once and keeps every mark through a crash, so the engine's seeded
  test never sees a missing mark with lane records behind it, the idle delay, the mark at open (K2) or a full
  ring. The journal's kill tests cover the first; the others are untested.

## What I ran

Same host and rules as above (scratch copy, one test thread, no root, one cargo job at a time; scratch copy and the
remote directories deleted).

- `cargo test -p lunix-journal -p lunix-engine -p lunix-format -p lunix-testkit` at `12f7e95`: exit 0, 491 passed,
  0 failed, 0 ignored. This run does not build the `crashpoints` feature, so the kill tests were not run by me.
- Four probes against the real `FileJournal` (scratch only): K2 (a number per attach), K1 (witness wiped by
  `prepare`), the full ring with reclaim (passes), k2 (damaged durable mark).
