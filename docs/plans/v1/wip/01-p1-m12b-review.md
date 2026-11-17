# M12b review: in-process stack tests and `lunixd::testsupport`

- Branch `p1/m12b` at `930bf26`, base `bf03943`. Reviewed 2026-11-16, read-only, plus non-root runs on the test
  host from a scratch copy (deleted afterwards).
- Scope: `crates/lunixd/src/testsupport.rs`, `crates/lunixd/tests/stack_{model,kill,support}.rs`, the report, and the
  code the findings point at (`lunix-engine/src/recover.rs`, `lunix-journal/src/{scan,journal}.rs`,
  `lunix-fsck/src/journal.rs`, ADR-4, on-disk-formats §5).

## Verdict

**Merge after fixes.** No blocker in M12b's own files: the builder, driver, oracle and both test programs are sound,
and the oracle is strict (whole-device equality with a prefix at least as long as the durable one). The work found a
real engine defect (E1) and a real fsck defect (F1).

What must happen before the gate can be called green for M15:

1. E1 fixed in `lunix-engine` (recommendation below: **take the proposed fix**), the tolerance arm removed from
   `stack_model.rs`, the ignored test un-ignored, the `refuse` half rerun.
2. The lead accepts or closes the three gate deviations in M2, M3 and M5 (each has a small follow-up).
3. F1 and the new F4 fixed in `lunix-fsck` (not M12b's files) before any gate reads "fsck clean" as exit 0.

## What I ran

All with the E1 fix applied to the scratch copy unless noted, release profile, 4 CPUs.

| run | result |
|---|---|
| `stack_support -- --include-ignored`, unmodified | 3 pass; `refuse_ignores_torn_records_below_the_floor` fails with `JournalDamage` as reported |
| my probe `rev_e1_after_one_crash_then_clean_close`, unmodified | fails: **one** crash, then a **clean close**, then `refuse` refuses the next open |
| both, with the proposed fix | pass |
| `stack_model`, 6 configurations x 4 sequences x 2 000 ops, fix applied, **any** refusal made a failure | pass, 0 refusals in 488 damaging crashes (about half under `refuse`) |
| same run, `lunix-fsck` on the crashed volume before each reopen | **86 of 430** crashed volumes get `error journal_damage` (exit 2), all of them volumes that then open under `refuse` (F4) |
| same run, `journal/` listed at the end of each sequence | 24 of 24: `q00-1, q00-2, q01-1, q01-2, m00-1..4`, the ordinals of creation: no ring file was ever reinitialised (M3) |
| `stack_model`, 4 configurations x 8 sequences x 2 000 ops, fix applied, any refusal a failure, and a plain drop required to give back the **whole** oracle | pass: 101 plain drops all exact, 0 refusals in 692 more damaging crashes |

I did not rerun the 100 x 2 000 gate or the kill loops (hours); the per-sequence figures of my sample (about 20
damaging crashes, 45 lost requests, 150 replayed records per sequence) match the report's.

## E1: analysis and recommendation

**The defect is real, and worse than reported.** The report's scenario needs a second crash after a later FLUSH.
Neither is needed. `FileJournal::prepare` (`lunix-journal/src/journal.rs:242-245`) raises the watermark to the last
recovered number at the first lane open, and schedules a mark for it (`:249-257`). So after one crash that tears a
record at or below the cut and leaves a later valid record of that lane, the next record or mark written carries
`flushed >= next_seq` of that range, and **every later open under `refuse` fails, including the one after a clean
detach**, until the ring file is reclaimed. My probe shows it with one crash and a clean close. In the report's gate
run it is about half of all `refuse` opens (849 of roughly 1 600 per configuration).

The crash that produces it is ordinary: the commit does not sync the journal (no `submit_group_flush` in the commit
path; only FLUSH and FUA do), so records at or below a durable cut can be torn by power loss.

**Why the range rule refuses nothing but false positives.** Take a damaged range `R` in lane `L` with neighbours
`prev_seq = P`, `next_seq = N`, `N <= w` (`w` = `Recovered::flushed`), and assume no duplicate and no gap at or
below `w`.

1. `write_seq` rises strictly within a lane across its files (§5.2, enforced by `valid()`), so a record that
   legitimately lay in `R` has a number in `(P, N)`.
2. No gap at or below `w` means the merge accepted a valid record for every number in `(floor, w]`
   (`scan.rs:341-380`). So every `s` in `(P, N)` above the floor is held by a valid record somewhere, and that record
   is not in `R` (a damaged range holds no valid record).
3. One number has one record unless it was issued twice, and that is `duplicate`, refused separately. A stale copy
   in `R` of a number later reissued is impossible too: the wipe zeroes everything a lane holds after its last
   record below `next_seq` (`plan_lane`, `scan.rs:483, 491`), and a stale record above the stop point lies after that
   record.
4. So `R` held only numbers at or below the floor (covered by the commit, §5.3 "costs nothing"), or no record at all
   (a flipped bit in batch padding also produces a damaged range). Nothing durable is missing.

**Why the gap rule alone loses nothing.** Let `s` be a lost record that a completed FLUSH or FUA covered, above the
floor.
- If `s <= w`: the record that carries `w` was appended after that flush, so its number is above `w >= s` and above
  the floor; it is a merge candidate. No valid record holds `s`, so the merge cannot pass `s`: `gap_at <= s <= w`,
  refused. This holds wherever the loss is: a torn record, a zeroed block that ends the file scan (§5.3 reader, zero
  magic at a 4 KiB boundary), a file with a destroyed header (its `DamagedRange` has unreliable neighbours, which the
  gap rule does not need), a damaged mark (its number is missing: refused, as ADR-4 already accepts).
- If `s > w`: no surviving record witnessed the flush. This is the documented window (crash inside the mark idle
  delay, full mark ring, `mark_idle = None`); the range rule does not catch it either (`next_seq > w` or `None`).

So the range rule is implied by the gap rule whenever something is lost, and the two differ only where nothing is
lost. **Recommendation: take the proposed fix** (`refuse_worthy` = `duplicate.is_some() || gap_at <= flushed`). It
cannot let the loss of a FLUSH/FUA-acknowledged record through that today's rule refuses.

Do **not** fix it in the journal by zeroing the residue: a zeroed block in front of a kept record ends the reader's
scan of that file (§5.3) and would lose the kept record.

To change with it:
- `lunix-engine/src/recover.rs:168-203` doc comment and function, the unit case "damage inside durable history"
  (`:470-473`), and the second warn line (`:338-342`), which would now call damage below the floor "above the flushed watermark".
- `lunix-engine/tests/journal_damage.rs` range cases; `lunixd/tests/wiring.rs` stays (its case has a gap).
- ADR-4 ("or a damaged range whose next valid record in its lane is at or below it"), on-disk-formats §5.3 "The
  flushed watermark at recovery", the plan's M12 paragraph. These are signed text: maintainer sign-off.
- Add `gap_at` and `duplicate` to `RecoveryReport` (tests now infer the gap from `ignored_after_gap` and
  `next_seq`, and cannot see a duplicate at all, m1).
- `stack_model.rs:295-326`: delete the tolerance arm, so any refusal fails; un-ignore
  `refuse_ignores_torn_records_below_the_floor` and correct its doc comment, which still names the narrow fix
  (`next_seq <= cut_seq + 1`) that the report itself found insufficient (`stack_support.rs:206-207`).

## Majors

### M1. E1 (lunix-engine)
Above. Until it is fixed the `refuse` half of the model test proves little: roughly every second `refuse` open is
refused and falls back to `wipe`.

### M2. The model test cannot see a missing or misplaced sync; the store half of the crash is empty (gate deviation)
The plan's crash is "drop + byte-level corruption of each lane past its last flush point + deletion of files not
synced (the store's test hook)". What is implemented:

- **Store half: nothing.** `damage_unsynced` (`testsupport.rs:1173`) looks for `*.tmp` and `*.open`; the report's F3
  says it found none in 12 000 crashes, and my runs show `files_removed: 0` everywhere. A drop between calls leaves
  the page cache whole, and so does `SIGKILL` in the kill loops. Scenario that passes today: the store renames a
  commit object and returns without the directory `fsync` (or seals a segment without `fdatasync`); the engine
  reports `Committed`, the model counts the cut durable, the "crash" keeps the file, the oracle matches.
- **Journal half: trusts the journal.** The flush point is `max(driver bound, watermark on disk)`
  (`testsupport.rs:1119`). The driver bound is the acknowledgement of FLUSH/FUA; the watermark is the journal's own
  claim. A group flush that acknowledges without syncing one lane is invisible.

That is inherent to an in-process test, but the plan asked for the store hook to cover the first part. R2 is the
right request; with it `testsupport` can undo renames no directory `fsync` covered and truncate files no `fdatasync`
covered, and the oracle then has teeth against the store's ordering. Limits to know: `FsOps` has no create call,
`fdatasync` takes a `&File` (map by inode), and `lunix-index` syncs with its own calls (`run.rs:45, 447`), outside
the hook. The lead should either land R2 and a follow-up before M15, or record this as an accepted deviation; the
report states it openly, the gate table row ("pass") does not.

### M3. The model test never leaves the first ring files; R3 is coverage, not only speed
Per sequence the model writes about 70 MiB (64 KiB chunks) or 18 MiB (16 KiB) over two lanes with 64 MiB files. In my
24 sequences every `journal/` ended with the ordinals of creation: no reclaim ever reinitialised a lane file or a
mark file. So 1.2 M ops of the gate never exercise, together with crash damage: a lane moving to its next file,
`plan_lane`'s wipe of files after the resume file, recovery across a reinitialised file, the checkpoint tail hint
skipping a file (`HintUnusable`), mark ring reclaim. The kill loops do reclaim (160 MiB), but with no damage. Land R3
(ring file length through `OpenParams`) and run the model with files of a few hundred KiB; it also removes the
0.4 s scan per open that dominates the run time.

### M4. `lunix-fsck` reports ordinary crash residue as an error (new, F4; lunix-fsck, found with M12b's tools)
`lunix-fsck/src/journal.rs:227-236`: a damaged range whose next record is above the cut is `journal_damage`,
severity error (`code.rs:159`), with no look at the flushed watermark. ADR-4 calls exactly this shape residue ("a
torn batch followed by a later batch of the same lane"). Running fsck on the crashed volume before each reopen gave
exit 2 on 86 of 430 volumes, every one of which then opened under `refuse` and matched the oracle. The M12b tests do
not see it: the model runs fsck only after the final clean close, and the kill loops only after `SIGKILL`, which
does not tear. With F1 (marks unknown to fsck) this means fsck currently says nothing reliable about the journal.
For the fsck owner: apply the engine's rule (error only for a gap at or below the watermark or a duplicate), count
marks. For M12b once fixed: run fsck after every crash in the model and require no error.

### M5. Kill loops: unreached points are printed, never asserted
`stack_kill.rs:691-703` asserts only points the counting run reached. The eight unreached points and the absent
`gc.*` are a line in the log. If a later change makes the workload miss `store.commit.*`, the test still passes.
Add an explicit expected-unreached list (`index.after_condemn_flag`, `store.evidence.*`, `store.set_aside.*`) and
fail on any other unreached registered point; `gc.*` then fails the loop the day M12's step 12 registers them,
which is what M17.B needs. I checked the eight: GC state and `fsck --truncate-tip` only, reachable by no phase 1
workload without them; the store's own crash tests cover the seven store points.

## Answers to the review questions

**1. Does the model test prove the gate's property?** For the logic it does: after 2 100 damaging crashes per
configuration the whole device equals the oracle after a prefix of the requests, never shorter than what FLUSH, FUA,
a commit's cut or a grow covered; reads match while running; the damage really is limited to records above the
flush point (`stack_support` checks that byte for byte). The oracle is not lenient: `first_match` compares the whole
device, and the durable bound errs low by at most one request (a mark taking a number between a write and the
`issued()` read, `stack_model.rs:208`).

What can still hide:
- any sync ordering bug (M2);
- anything about ring file turnover under damage (M3);
- anything that needs two requests in flight: the driver submits one request at a time, so a lane batch holds one
  record, no two batches are in flight, no cut meets an in-flight write. The damage imitates the on-disk result, the
  code paths that produce it do not run. The plan says "the M11 model test again"; the report defers concurrency
  (decision 1). An accepted deviation should be written as one;
- a crash inside a commit combined with a lost journal tail (the model crashes only between calls, the kill loops
  lose nothing);
- over-discarding by recovery after a crash with no damage (m2);
- damage shapes not generated: a write whose first sectors are missing and later ones present, a damaged file
  header, an interrupted reinitialisation. The first reads like "zeroed record followed by a kept one", which is
  generated; the other two are in the journal's own tests.

The E1 tolerance does not hide data loss: after a tolerated refusal the volume is opened under `wipe` and goes
through the same oracle check. It hides only `refuse` verdicts (M1, m1).

**2. Kill loops.** Coverage is good: 55 points, first/middle/last, recovery points hit while recovering an already
killed volume (the prep kill), a mid-run reopen, a grow, reclaim. Every commit in the plan is preceded by a FLUSH, so
after a kill inside a commit the oracle demands every request submitted so far: strict. The retry
(`stack_kill.rs:654-666`) is sound: a run that ends without a kill is repeated at its own last count, at most three
times, and the final check fails a point with fewer distinct kills than hits allow. Its weak side is the other
direction: when a run has more hits than the counting run, "last" is not the last (only the mark points vary).
Gaps: M5; no assertion on the fsck warning classes (m4); nothing is written after recovery before the clean close
(m5).

**3. `testsupport` for M17.B and M15.** Usable as it is. Notes: m6 to m9.

**4. E1.** Above.

**5. F1, F2, R1 to R3.**
- **F1: confirmed** (`lunix-fsck/src/journal.rs:40-49`: only `q<lane>-<ordinal>.j`). Every healthy volume exits 1,
  and marks missing from the merge give false `ignored_after_gap`. Must be fixed before M15's "fsck clean"; then
  tighten the stack tests to exit 0 at rest.
- **F2: agreed**, harmless, but it also keeps a recovered and cleanly closed volume at exit 1. Cheapest fix: persist
  the index when the catch-up inserted something. Related observation: `Engine::quiesce`
  (`engine.rs:451-454`) is one non-forced commit, and `bound_cut` may cut below the last issued number on a large
  volume, so "final cut and commit" can leave a journal tail; not visible on the 16-chunk test device.
- **R1: agreed**, with two additions: build `lunix-fsck` in the step and set `LUNIX_STACK_REQUIRE_FSCK=1` (otherwise
  the fsck half is skipped silently), and run it in `--release` (the report's own timing). Today nothing of M12b
  runs in CI and clippy never sees `testsupport.rs`.
- **R2: agreed**, needed for M2.
- **R3: agreed**, needed for M3.

## Minors

- **m1.** The tolerance arm (`stack_model.rs:295-326`) also swallows a refusal for a duplicate `write_seq` when a
  below-floor range is present (the report has no `duplicate` field). Gone with the arm.
- **m2.** A plain drop (`crash(false)`) loses nothing: every acknowledged record is in the files. The test accepts
  any prefix at or above the durable one (`stack_model.rs:353`), so recovery discarding intact records would pass.
  Require the whole oracle there. With that change the test passes today (101 plain drops in my run), so it
  costs nothing and closes the hole.
- **m3.** `Driver::run` takes `before + 1` as the durable bound of a FUA write (`testsupport.rs:766`). If a mark takes
  `before + 1`, the write is `before + 2`, above the bound, and the crash may damage a record the model counts
  durable: a false failure, rare (a mark between two lines of the driver). Read `issued()` after the completion and
  use it for FUA.
- **m4.** fsck findings after recovery are tallied and printed (`stack_kill.rs:529-554`), not checked. Assert an
  allowed set per phase (after the kill: `leftover`, `crash_leftover`, `orphan`, `index_missing`, journal notes; at
  rest: none once F1 and F2 are fixed).
- **m5.** `verify` (`stack_kill.rs:557-591`) reads, closes and checks. Add a few writes and a read-back after
  recovery, and a second open after the close. Optional and worth it: record `Driver::durable()` in the progress file
  and apply `damage_journal` above it after the kill; that is the one place a crash inside a commit can meet a lost
  tail.
- **m6.** `TestVolume::crash` and `close` take the stack only; a live `Driver` keeps the handlers and the journal
  open while `damage_journal` rewrites the files. The docs say "drop it first"; nothing enforces it. `crash` also
  sleeps a fixed 150 ms, right only for the default `mark_idle`.
- **m7.** `DeviceModel::first_match` and `truncate` replay the log from the start (`testsupport.rs:983, 1020`), with
  payload regeneration: fine at 160 MiB, quadratic for a long M17.B run. Keep a snapshot at the durable prefix.
- **m8.** `damage_unsynced` has no test of its own and never finds a file in the model (F3); until R2 it is dead
  code (the kill loops plant a `*.tmp` but never call it). Add a support test that plants files.
- **m9.** `copy_as` drops the `json` tweak silently for anything read later from the builder (documented), and
  `fsck()` returns `None` for a passphrase volume, so a quarter of the encrypted sequences skip fsck (reported).
- **m10.** Report: "box2" in the CI row is a machine name (rule 3); the disk budget is 10 GiB, the report says 15 and
  used 20.
- **m11.** `stack_model.rs` module docs say "One refusal is tolerated": it is every such refusal.

## rust-skills

Nothing to raise. Test code panics with context, `testsupport` documents its panics, setters are `#[must_use]`,
no `unsafe`. `as` casts in the tests are on small test values.
