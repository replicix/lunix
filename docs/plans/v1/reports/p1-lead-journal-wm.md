# Note: branch `lead/p1-journal-wm` (M12a re-review R1(b): the flushed watermark)

The M12a implementer made this branch for the lead, from `lead/p1-iface2` `3fe9a8a`. Ownership fails for this branch
by design, so CI ran with `LUNIX_SKIP_OWNERSHIP=1`.

**Status: the format change needs the maintainer's FREEZE.md re-sign before merge.** The amendment text is in
`crates/lunix-format/FREEZE.md` ("Amendment 2026-11-14 — pending maintainer re-sign").

## The rule

- Every journal record header carries `flushed` (bytes 56-63, reserved and zero in the signed layout): the highest
  `write_seq` that a completed group flush had made durable on every lane when the record was appended. It is always
  below the record's own `write_seq`; 0 means none.
- A group flush completes only after every lane's records up to `upto` are reaped and fdatasync'd. So every record at
  or below the highest watermark `W` that any surviving record carries was durable before the crash.
- Recovery reports `W` as `Recovered::flushed`. `on_journal_damage = refuse` refuses only a gap at or below `W`, or a
  damaged range whose next valid record is at or below `W`. That is real loss of acknowledged-durable data.
- Everything above `W` is crash residue: a torn tail, a gap across lanes, two batches in flight. It is logged at warn
  (with `W`) and wiped, as under `wipe`.
- This replaces the 2026-11-13 rule (refuse only mid-journal damage) and the interim limit of `2244313` (refuse is
  for single-queue volumes or setups that run fsck after every crash).

## Commits

| commit | content |
|---|---|
| `7d57130` | lunix-format: `RecordHeader::flushed`, encode and parse (reject `flushed >= write_seq`), `BatchBuilder::with_flushed`; f02 tests; FREEZE.md amendment (pending); on-disk-formats §5.2 and §5.3. |
| `4a99dda` | lunix-iface `Recovered::flushed`; lunix-journal writes the watermark (raised by a completed, unpoisoned group flush and after recovery's sync) and reports the highest found; testkit; new `tests/watermark.rs`. |
| `dd2adb1` | lunix-engine `refuse_worthy` against the watermark (unit test), `OnJournalDamage::Refuse` rustdoc; ADR-4, DESIGN §4.1 and §4.4 step 7, plan M12 paragraph. |
| `61ad263` | lunix-testkit `MemJournal` writes and reports the watermark, `JournalImage::tear` and `lose`; `RecoveryReport::flushed`; lunix-engine `tests/journal_damage.rs`. |

## Compatibility

- A record written under the signed layout has zeros in bytes 56-63 and reads as watermark 0. The item 2 goldens are
  unchanged byte for byte.
- A journal with watermark 0 everywhere refuses nothing for gaps or damage, which matches `wipe` for such journals. A
  journal written before this change is replayed and rewritten at the next attach, so the watermark starts working
  after one attach.
- Downgrade: a signed-layout reader rejects a record with non-zero bytes 56-63. Journals written by this branch cannot
  be read by older builds. Phase 1 has no released journals, so this is noted only.

## Tests

- lunix-format f02: the round trip, watermark 0 as the old reserved field, and the `flushed >= write_seq` rejection
  (replacing the "reserved 56 non-zero" case).
- lunix-journal `tests/watermark.rs`:
  - records carry the watermark and recovery reports it;
  - a gap across lanes above the watermark (two batches in flight on different lanes) is reported with a watermark
    below it;
  - damage at or below the watermark is visible as such.
- lunix-journal `tests/watermark.rs` also has two batches in flight on one lane, the first torn, the second landed:
  the damage is above the watermark.
- lunix-engine unit test `only_loss_at_or_below_the_watermark_is_refused`: `refuse_worthy` on the torn tail, two
  batches in flight, a gap across lanes (all residue), and a gap or damage at or below the watermark (refused).
- The testkit `MemJournal` now writes and reports the watermark the same way, so the engine's tests run the rule
  end to end. `JournalImage::tear` and `JournalImage::lose` damage or remove any record, durable or not, as a media
  error would. Its own tests check that a crash image never has a gap or damage at or below the watermark.
- lunix-engine `tests/journal_damage.rs`, through `Engine::open` with `on_journal_damage = refuse`:
  - a gap across lanes above the watermark opens;
  - two batches in flight on one lane, the first torn, opens;
  - a durable record torn or lost (at or below the watermark) is `OpenError::JournalDamage`, and `wipe` opens the
    same image;
  - 24 seeded crash images (keep, drop and tear of unflushed records, two lanes, out-of-order completions) all open,
    and at least one had residue.
- `RecoveryReport::flushed` reports the watermark.
- The daemon's wiring tests on `p1/m12a` (torn last record under refuse opens, damage followed by a valid record is
  refused) still hold on this rule; they run when `p1/m12a` is rebased onto this branch.

## CI on the build host (CPUs 0-3, `LUNIX_JOBS=4`, `RUST_TEST_THREADS=1`)

- **`dd2adb1`:** `ci: all green`, 1 362 tests passed, 0 failed (ownership skipped).
- **`61ad263`, first run:** the crashpoints build of lunix-engine failed to compile `tests/journal_damage.rs`. This was
  a stale build, not the branch: the files had been edited locally while the previous run was building, so their
  mtimes were older than its cached artifacts and cargo kept those. After touching the changed files:
- **`61ad263`, second run:** `ci: all green`, 1 371 tests passed, 0 failed. That is 1 362 plus the 4 new engine
  tests, which run in both the workspace and the crashpoints step, and the new journal test.
- The build host's target directory was removed afterwards (disk budget).
