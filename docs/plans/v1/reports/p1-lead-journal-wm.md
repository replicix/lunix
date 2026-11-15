# Note: branch `lead/p1-journal-wm` (M12a re-review R1(b): the flushed watermark)

The M12a implementer made this branch for the lead, from `lead/p1-iface2` `3fe9a8a`, then rebased it onto `origin/main`
`63172f3` (where `3fe9a8a` is merged) for the review fixes. Ownership fails for this branch by design, so CI ran with
`LUNIX_SKIP_OWNERSHIP=1`.

**Status:** the maintainer re-signed the watermark amendment (`9b60936`, `2d877db` after the rebase). The independent
review (`01-p1-journal-wm-review.md`) is "merge after fixes"; the fixes are below. The maintainer decided M2: close the
unwitnessed window with a witness, a mark record (kind 3) in a coordinator-owned mark ring. That is implemented here;
its FREEZE.md amendment (b) is **pending the maintainer's re-sign**. The four-queue `kill -9` + `refuse` cell is the
remaining gate.

## The rule

- Every journal record header carries `flushed` (bytes 56-63, reserved and zero in the signed layout): the highest
  `write_seq` that a completed group flush had made durable on every lane when the record was appended. It is always
  below the record's own `write_seq`; 0 means none.
- A group flush completes only after every lane's records up to `upto` are reaped and fdatasync'd. So every record at
  or below the highest watermark `W` that any surviving record carries was durable before the crash.
- Recovery reports `W` as `Recovered::flushed`. `on_journal_damage = refuse` refuses only a gap at or below `W`, or a
  damaged range whose next valid record is at or below `W`. That is real loss of acknowledged-durable data.
- Everything above `W` is treated as crash residue: a torn tail, a gap across lanes, two batches in flight. It is logged
  at warn (with `W`) and wiped, as under `wipe`, with every record after the first gap.
- A `write_seq` found twice (`Recovered::duplicate`) is refused at any watermark: a wipe did not happen.
- A ring file left by a reinitialisation interrupted between its header write and its rename (a valid header of the
  same lane and volume with a higher ordinal, zero body) is not damage.
- Marks (review M2, maintainer decision): a flush is witnessed only by a record appended after it completed. After a
  group flush that raised `W`, unless a later group whose `upto` is above the counter at the raise completes within
  the idle delay (`JournalConfig::mark_idle`, default 50 ms), the flush coordinator appends a mark (kind 3, 64 bytes
  in its own 4 KiB block, a `write_seq` of its own) carrying `W` to the mark ring `m00-<ordinal>.j` (4 files of 1 MiB)
  and `fdatasync`s it. At most one mark per such flush, only while no flush is queued; also once after recovery's
  `prepare`. Recovery counts marks for contiguity and the watermark and never lists them as records, so they never
  reach the overlay or the map. A failed mark poisons the journal at its number. Metrics:
  `FileJournal::marks_written()` and `marks_skipped()` (ring full until the next cut).
- The remaining window: a crash within the idle delay. The last flush is then unwitnessed and medium damage to its
  records reads as residue (wiped with every record after it); a crash alone cannot damage them. A lost or torn mark
  is residue.
- This replaces the 2026-11-13 rule (refuse only mid-journal damage) and the interim limit of `2244313` (refuse is
  for single-queue volumes or setups that run fsck after every crash).

## Commits

| commit | content |
|---|---|
| `7d57130` | lunix-format: `RecordHeader::flushed`, encode and parse (reject `flushed >= write_seq`), `BatchBuilder::with_flushed`; f02 tests; FREEZE.md amendment (pending); on-disk-formats §5.2 and §5.3. |
| `4a99dda` | lunix-iface `Recovered::flushed`; lunix-journal writes the watermark (raised by a completed, unpoisoned group flush and after recovery's sync) and reports the highest found; testkit; new `tests/watermark.rs`. |
| `dd2adb1` | lunix-engine `refuse_worthy` against the watermark (unit test), `OnJournalDamage::Refuse` rustdoc; ADR-4, DESIGN §4.1 and §4.4 step 7, plan M12 paragraph. |
| `61ad263` | lunix-testkit `MemJournal` writes and reports the watermark, `JournalImage::tear` and `lose`; `RecoveryReport::flushed`; lunix-engine `tests/journal_damage.rs`. |

After the rebase onto `63172f3` the commits above are `bfac48f`, `d9ee050`, `b7041fa`, `4c94a6c`, the first version of
this note `2b1ee48`, and the maintainer's re-sign `2d877db`. The review fixes:

| commit | content |
|---|---|
| `24a8603` | M1: `SlotInfo::reinit` and the scan skip an interrupted reinitialisation (zero body checked); crash point `journal.reinit.before_rename`; the kill tests check at every point that `refuse` would open, and the child writes a witness after each flush so the M1 state is refuse-worthy without the fix. m3: `Recovered::duplicate`, refused at any watermark. m1: `submit_group_flush` clamps `upto` to `last_issued()` (FileJournal and MemJournal; iface rustdoc). The iface module docs describe the watermark. m4: the seeded engine test has 8 to 24 unflushed writes and 1 to 4 appends in flight per seed. |
| `08c5e2c` | m2: `encode_delta_region` writes `flushed = 0`, `decode_delta_region` rejects any other value (f02 case). m5: the re-sign in the field rustdoc. |
| `cb7583f` | M2 wording (ADR-4, on-disk-formats §5.3, DESIGN §4.1 and §4.4 step 7, plan); M1 in on-disk-formats §5.1 and ADR-4; duplicates; DELTA `flushed = 0` in §5.4; "pending re-sign" replaced in on-disk-formats §5.2 and §5.3, ADR-4, the plan, the journal README and the field rustdoc. DESIGN §4.1 step 4 no longer says that no marker of the last flushed `write_seq` exists. |

## Mark ring (review M2, maintainer decision)

| commit | content |
|---|---|
| `bf6dcd1` | lunix-format: `RecordKind::Mark` = 3 (`len` 0, `lba` 0, no data), strict encode and parse, `BatchBuilder::push_mark`, `MARK_FILE_LEN`, DELTA rejects marks; golden `02-journal-record-mark.bin`; FREEZE.md amendment 2026-11-14 (b), pending re-sign, and the compatibility wording of the watermark amendment dropped. |
| `1e19b30` | lunix-journal: the mark ring `m00-<ordinal>.j` (4 x 1 MiB), loaded, scanned, planned, prepared and reclaimed with the lanes; the coordinator's `MarkDue` and `write_mark` (crash points `journal.mark.before_write` and `journal.mark.after_sync`); `JournalConfig::mark_idle`, `TraceEvent::Mark`, `marks_written()`/`marks_skipped()`; lanes refuse to reserve kind 3. lunix-iface: `MARK_RING_LANE`, `record_bytes` for marks, contract docs. lunix-engine: skips a mark on replay. lunix-testkit: `MemJournal::set_marks` (a mark at once after each flush that raised the watermark). Tests below. |
| `d4ea229` | ADR-4, on-disk-formats §5 intro and §5.1 to §5.4, DESIGN §4.1 and §4.4 step 7, plan, journal README, this note. |

Tests:
- lunix-format f02: the mark golden and its negative suite; `len`/`lba`/data/watermark rules; marks read back by the
  normative reader; DELTA encode and decode refuse a mark.
- lunix-journal `tests/marks.rs`: an idle flush gets exactly one mark and recovery counts it (no record, next number
  after it); the review's M2 probe (10 records, one flush, record 3 damaged) is refuse-worthy once the mark landed and
  reads as residue without marks; no mark before the idle delay ends; a later flush within the delay makes the mark
  unneeded; a lost or torn mark is residue; a damaged durable mark followed by a later one is reported (lane
  `MARK_RING_LANE`) and refuse-worthy; a lane cannot reserve a mark; recovered records get a mark after open.
- lunix-journal kill tests: the child idles past a 5 ms mark delay after each flush; `kill_before_the_mark_write` and
  `kill_after_the_mark_sync` (the watermark then covers the last flush the child saw). Every kill test checks that each
  number up to `next_seq` is a record or a mark, and that `refuse` would open. The M1 kill test still fails without
  its fix.
- lunix-engine: `the_m2_probe_is_refused_once_the_mark_landed` (`OpenError::JournalDamage` with marks, opens without),
  `marks_take_numbers_and_change_no_block`, the seeded refuse test with marks in half the seeds, and the engine model
  test with marks in half the workloads (numbers the engine never issued between its own).
- Not covered: the mark ring filling up (255 marks per file, 4 files) before a cut frees a file; `marks_skipped`
  counts that case. fsck listing marks is M14's follow-up; exporting the two metrics from `lunixd` is a follow-up
  for M12.

## Mark ring review fixes (2026-11-15)

The review ("Mark ring review (2026-11-14)") is "merge after fixes"; the maintainer re-signed amendment (b) and the
lead records the re-sign on the final text.

- **K1:** `witnessed` after recovery is the highest watermark carried by a record that survives the wipe (numbered
  below `next_seq`, `Plan::kept_flushed`), not the maximum over all scanned records. Test
  `a_witness_beyond_the_gap_is_replaced_by_a_mark_after_open` (the review's probe).
- **K2:** no mark for a watermark at or below the last durable cut (`max_cut_asked`): a clean attach takes no
  number, a cut inside the idle delay cancels the owed mark. `prepare` schedules a mark only when records above the
  floor were recovered. Test `a_clean_attach_or_a_cut_inside_the_delay_writes_no_mark` (two clean attaches leave
  `last_issued` unchanged).
- **k1, ring full:** the owed mark is kept and retried every idle delay, and at once when reclaim frees a mark file;
  `marks_skipped` counts each skipped watermark once; a warning is logged once per episode (lunix-journal now depends
  on `tracing`). Test `a_full_mark_ring_retries_after_reclaim` (1 020 marks, one skipped, written after reclaim
  without a new flush).
- **k3, item 3 enforced:** recovery treats a kind-3 record in a lane file, or a kind 1/2 record in a mark file, as
  damage. Test `a_record_of_the_wrong_kind_for_its_ring_is_damage`.
- **k4:** no mark for a group whose only new number is the last mark. Test
  `an_empty_flush_after_a_mark_writes_no_mark`.
- **k2:** a damaged durable mark costs the later acknowledged records under `wipe`; documented in ADR-4 and
  on-disk-formats §5.3 as a known cost. No bridge in this branch.
- **k5:** the costs (a failed mark poisons; the mark's sync delays a FLUSH arriving meanwhile) are stated in ADR-4
  and §5.3.
- **k6 (part):** a failed allocation of the mark buffer now counts in `marks_skipped`.
- **FREEZE amendment (b):** item 4 reworded (above the last durable cut; no mark for a mark-only group; after a
  restart only when records above the floor were recovered; retried when the ring is full; the remaining window is a
  crash within the idle delay, a full ring until reclaim, and `mark_idle = None`); item 2 says the mark file's
  header is a lane-0 header; item 3 says recovery treats a wrong-kind record as damage. Layout unchanged; "Re-signed
  by" left pending for the lead.
- **Listed, not fixed:**
  - k6: a due mark is dropped at shutdown (a clean stop without a final cut leaves the last flush unwitnessed);
    `recover` on a live object races a mark in flight (as m6).
  - k7: `MemJournal` writes its mark at once and keeps every mark through a crash; it does not model the idle delay,
    the mark at open, a full ring or a lost mark behind lane records. The journal's own tests cover those.
  - k2 bridge (recovery bridging missing numbers inside a damaged mark range): a maintainer decision.
  - Exporting the two metrics from `lunixd` (M12a) and fsck listing marks (M14).

## Format status

- The watermark (bytes 56-63) is re-signed (`2d877db`). 0 means no watermark known; the reader rejects
  `flushed >= write_seq`. Its amendment text in FREEZE.md lost its compatibility reasoning under the maintainer's
  no-compat rule of 2026-11-14 (layout unchanged).
- The mark ring (kind 3, `m00-<ordinal>.j`, golden `02-journal-record-mark.bin`) is **pending the maintainer's
  re-sign**: FREEZE.md "Amendment 2026-11-14 (b)".

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
  - 32 seeded crash images all open: a flushed history, then 8 to 24 unflushed writes on two lanes with out-of-order
    completions, then 1 to 4 appends still in flight at the crash; keep, drop and tear of everything no completed
    flush covered. More than 8 of them have residue and at least one has a gap.
- `RecoveryReport::flushed` reports the watermark.
- Review fixes:
  - M1: `an_interrupted_reinitialisation_is_not_damage` (journal, hand-made state; the dirty-body control is still
    damage). It fails without the fix with the review's probe result (`next_seq` 31, refuse-worthy). The kill test
    `kill_between_the_reinit_header_and_the_rename` (crash point `journal.reinit.before_rename`, hits 1, 2, 5) fails
    without the fix (damage with `next_seq` 61 under watermark 74) and passes with it. Every kill test now asserts
    that `refuse` would open.
  - m1: `a_flush_above_last_issued_does_not_raise_the_watermark_past_it` (the review's probe 2).
  - m3: `a_duplicate_write_seq_is_reported_apart_from_a_gap`, and the engine unit test refuses a duplicate at
    watermark 0.
  - m2: an f02 case for a DELTA record with a watermark.
  - Not added: the watermark across ring wrap and reclaim with a checkpoint hint (m4, first bullet).
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
- **Review fixes, `fe46fe5`** (rebased onto `63172f3`; source and target in the host's `journalwm` directories): the
  first run passed every test (1 429 passed, 0 failed) and stopped at the ownership step, which fails by design on a
  lead branch; the rerun with `LUNIX_SKIP_OWNERSHIP=1`: `ci: all green`, 1 429 tests passed, 0 failed.
- **Mark ring, `d4ea229`** (scratch under a milestone id of its own, after a shared scratch directory lost a test's
  template mid-run): `ci: all green`, 1 446 tests passed, 0 failed (ownership skipped). An earlier run at the same
  head failed `cargo fmt --check` on empty files a local `touch` had created on the build host; it was not the branch.
