# Note: branch `lead/p1-journal-wm` (M12a re-review R1(b): the flushed watermark)

The M12a implementer made this branch for the lead, from `lead/p1-iface2` `3fe9a8a`, then rebased it onto `origin/main`
`63172f3` (where `3fe9a8a` is merged) for the review fixes. Ownership fails for this branch by design, so CI ran with
`LUNIX_SKIP_OWNERSHIP=1`.

**Status:** the maintainer re-signed the FREEZE.md amendment (`9b60936`, `2d877db` after the rebase, text unchanged).
The independent review (`01-p1-journal-wm-review.md`) is "merge after fixes". The fixes are below. M2 needs the
maintainer's explicit confirmation of the trade (docs only, no code change). The four-queue `kill -9` + `refuse` cell is
the remaining gate.

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
- The unwitnessed window (review M2): a flush is witnessed only by a record appended after it completed. Every record
  above `W` is in the window, which after a burst, one FLUSH and no later write is the whole burst. Medium damage there
  reads as residue: the volume opens and the damaged record and every record after it are wiped, although their flush
  was acknowledged. Here the rule is weaker than the 2026-11-13 rule; outside the window it is stronger. The docs now
  say so (ADR-4, on-disk-formats §5.3, DESIGN §4.1); the maintainer confirms the trade.
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
