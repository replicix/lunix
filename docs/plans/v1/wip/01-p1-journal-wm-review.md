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
