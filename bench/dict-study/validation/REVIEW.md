# Independent review: dictionary validation study (draft SUMMARY of 2026-11-13)

Reviewer: independent, did not run the study. Scope: `PREREGISTERED.md` (binding), the draft summary, the lead rulings,
`raw/`, `spikes/dict-validation/`. No prices and no host details appear in this file.

## 1. Verdict

**Summary accepted after corrections, with one decision held open.**

- D1, D3, D4, D6: the NO-GO verdicts follow from the protocol and the data and are robust to every reading I tried.
- D5: NO-GO stands, but it must be relabelled (it is a verdict on the shipped selector, not on the dictionary set) and the
  missing saving pass should be run so that the decision table itself, and not only the cap, carries it.
- **D2, database family: the NO-GO is not established.** It depends on (a) a choice between readings that the protocol
  does not settle and that the draft settled itself after the data were known, (b) one timing cell that is not a sample
  of the decision population and is extrapolated to 13 untimed images, and (c) `pv` where §3 of the protocol says D2 must
  be judged on `pv-cover`. Under the literal reading of the D2 row the database vote is reached at both chunk sizes.
  D2 on raw devices is a robust NO-GO.
- D7: fires for zdicts and Constellation. For Lunix, "codec 3 with built-ins stays reserved" follows from D1 alone;
  "`train`/`apply` leave v1" depends on D2 (database family) and is open until the items in §5 are done.

What I verified by execution: the analysis was re-run on the test host from the repository's scripts (B = 10,000);
`families.csv`, `tests.csv`, `desc.csv`, `votes.csv` and `tables.md` came out byte-identical to `raw/` (SHA-256 prefixes
4536a207, 39a2a320, ba03ad0b, f62ac3e9). An independent percentile bootstrap written for this review reproduces the D2
database intervals (for example se-askubuntu x PostgreSQL pv@3 against ft@3: 5.05 [4.95, 5.15], draft BCa LB 4.94). The
review directory on the host was deleted afterwards; nothing under `ana/` or the data was modified.

## 2. Per decision

### D1 (Lunix codec 3 with built-ins): agree, NO-GO at both sizes
- The counterfactual rule is the protocol's (§1.1: best plain level that passes the cap against the default) and is
  applied consistently: zstd-3 behind the probe at 16 KiB (0.832x zstd-1 on the timed cell), zstd-1 at 64 KiB (zstd-3 is
  0.758x). Comparing level-3 dictionary arms against a level-1 counterfactual at 64 KiB is what the D1 row prescribes
  ("at zstd-1 and zstd-3, vs counterfactual"); the draft says openly that most of the 64 KiB gain is the level.
- Robustness. 16 KiB: best N is -0.20 pp; even if zstd-3 had failed the cap on the host tree (counterfactual zstd-1), the
  best realisable arm (ft@3) would be about 2.2 % before 30 cores of selector cost. 64 KiB: the three host corpora have
  LB 2.89-2.90 pp for the best arm (bs-dist@3), below 3 pp with zero CPU charged and with no cap applied, and a GO needs
  all four; a counterfactual of zstd-2 (passes the cap, not measured volume-wide) would only lower N. No reading flips D1.
- "Decisive" at 16 KiB: the oracle LB is 0.84-1.40 pp (0.76-1.20 with 16 MiB blocks). Deviation 15 (oracle without the
  DB set) cannot matter: the clause would need the DB dictionaries to add about 1.6-2.2 pp volume-wide on OS images that
  contain no database pages, and the verdict does not rest on the clause anyway (0 of 4 is already NO-GO by the first
  clause). Correction: say "decisive among the measured candidate sets"; the oracle is the best of the dictionaries that
  were trained, not a bound on every possible set.
- Note for the record: the 4-of-4 vote rests on two independent contents (one host tree in three file systems, one VM
  image). The draft says so. This weakens a GO, not a NO-GO.

### D2 (per-volume `train`/`apply`): raw devices agree (NO-GO); database family NOT accepted as written
Raw devices: pv@3 is worse than ft@3 and worse than plain zstd-3 on all four eligible corpora; 0 of 4 under every reading.

Database images. Facts from `raw/` (16 KiB, seed 1 hold-out):
- Saving is not in doubt: pv@3 against zstd-3 is 4.9-12.7 % on the 12 SE and TLC images (LB 4.6-12.4 pp).
- N at or above 2 pp without the cap: 8 of 15 with the draft's comparator (best of counterfactual and shipped sets),
  12 of 15 against the counterfactual alone.
- The cap value that removes them is one cell: pv@3 3.809 against zstd-3 2.983 CPU-s/GB = 0.783x, min-max spread under
  0.3 %. The other two measured contents give 0.801x (VM image) and 0.892x (TLC pages). The between-content spread
  (0.78-0.89) is six times the margin to the cap (0.017).

**Which reading does the text support?** Neither cleanly; the draft's sentence "the protocol's text puts the cap against
the CF, so the strict reading stands" overstates it.
- Literal reading of the D2 row: the comparator is "the best shipped set on that volume"; §1.1 defines the cap against
  "the comparator's" throughput and delta-CPU as CPU(A) - CPU(B) on the same comparator. That is the draft's *lenient*
  reading: 9 of 15 at 16 KiB and 8 of 15 at 64 KiB, i.e. the database vote is **reached at both sizes**.
- The general sentence in §1.1 ("the comparator for every dictionary decision is the best plain level ...") cannot be
  literally universal, because D2, D2c and D3 name other comparators in their rows. It supports a counterfactual-based
  cap only by purpose: the cap protects throughput against what the product would otherwise run.
- Purpose favours a third, coherent reading the draft does not show as a vote: since D1 is NO-GO and ft@3 itself fails
  the cap at 0.27x, no shipped set ships; "what ships already" (§1.3) is the counterfactual. Then saving, cores and cap
  are all against zstd-3: 12 of 15 pass N, and the vote is 3 of 15 or up to 12 of 15 depending only on timing that was
  not measured. Under the lenient reading the cap against ft@3 is vacuous (ratio 2.9), which defeats its purpose.
- The draft's strict reading is a hybrid (saving against an arm that will not ship, cap against the counterfactual, the
  worse of both). It is the most conservative of the three, it is deviation 22 and not protocol text, and the tie-break
  "the stricter reading decides" is itself not in the protocol and was adopted after timing and aggregate point estimates
  were known. A tie-break that always picks NO-GO is not neutral. This needs a lead ruling, like the 4-of-6 ruling.

**Is the measurement adequate to decide a near miss?** No.
1. The timed cell is not the decision population. D2 is volume-wide; the cell is 16 KiB cuts of heap and index files
   only (`tmsample.py:102-124`, `timing_run.py:27`), file-offset aligned. My recount on the hold-outs: DB pages are
   56-82 % of the bytes; WAL is 15-36 % on PostgreSQL and MySQL images; 26-44 % of the SQLite images carry other labels.
   The protocol says "throughput on its population". A WAL ratio a few points above 0.80 would lift the blended ratio of
   a PostgreSQL image over the cap.
2. One PostgreSQL image stands in for nine SE images and (as the worse of two cells) for three geonames images; MySQL
   (16 KiB pages) and SQLite (4 KiB pages) were never timed (T5). The protocol required a sample per corpus and view;
   only the host-tree stand-in is sanctioned by it.
3. The dictionary arms are timed through one libzstd path: `ZSTD_createCDict(dict, len, level)` plus `refCDict`
   (`ztime2.c:43`, `:58-60`), so libzstd picks the dictionary's parameters and the attach or copy mode. The extra cost per
   chunk is 5-14 microseconds at 16 KiB, and on the VM content 9 at 16 KiB against 73 at 64 KiB, which at 64 KiB looks
   like a per-call table cost and not match-finding work (hypothesis, not verified). A 2 % miss should not be decided by an untuned default.
4. §3 of the protocol: if `pv-cover` beats `pv` by more than 1 pp "D2 is judged on pv-cover". It does on the database
   sweep volume (1.28 pp). The draft reports this in prose and still judges on `pv`; `pv-cover` exists for two volumes
   and was not timed. This is an unlisted deviation. With +1.3 pp the three MySQL SE images and tlc-my move to or over
   the 2 pp floor in the draft's own strict reading.
5. 64 KiB "confirmatory": database images have no 64 KiB timing cell; the counterfactual (zstd-1) and every cap and core
   figure come from the VM image (`analyze.py:48`, `:75`). "0 of 15 at 64 KiB" is an artefact of that stand-in, not a
   database result. `decide.py:62-63` also requires the confirmatory size to pass for D2 to fire; the protocol does not
   say what "confirmatory" does. No effect today, decisive after any re-timing.

**What can flip.** Under the draft's strict reading a GO needs all of the six SE PostgreSQL and SQLite images plus two
TLC images to pass the cap (or MySQL images lifted by `pv-cover`); the one measured cell is among them, so a flip needs
the volume-wide ratio of the PostgreSQL SE images to come out at or above 0.80 (measured 0.783 on pages only), or enough
MySQL and SQLite images to pass. Under the counterfactual-only reading it needs any 5 of the 9 SE images to pass the cap.
Under the literal reading it is already a GO for the database family. A flip is plausible, not likely under the strict
reading; I cannot call the NO-GO established.

**Required before the D2 database verdict stands** (§5: R1-R3).

### D2c: agree (not evaluated). If D2 fires later, pv-class is within 0.6 pp of pv, so the outcome would be "single dictionary only".

### D3 (zdicts block or file-system set): agree, NO-GO
First clause (D1 does not fire). Descriptively bs-dist is worse than the file-type set at 16 KiB and within 0.4 pp at 64 KiB.

### D4 (zdicts database set): agree, NO-GO
Median N per engine is negative for D4-L and D4-C; no consumer fires. Timing stand-ins are irrelevant because the savings
are negative before any CPU charge. `decide.py:74-76` adds conditions the protocol does not have (three significant
datasets, cap passed on all rows); they only make a GO harder and change nothing here.

### D5 (Constellation adopts the file-type set): agree with NO-GO for this release; relabel and complete
- The hard cap is a §1.1 rule that applies "regardless of N", the registered arm is `select(name, head, len)` at the pinned
  release, and the margin is not a near miss (0.39x on all objects, 0.14x under 64 KiB). Extrapolating from one of the
  four file corpora is acceptable at this margin. "Not decided" would be wrong; so is presenting it as a verdict on
  dictionaries.
- The cap failure is entirely the content classifier. From `raw/timing.csv`: without the selector pass the dictionary arm
  runs at **0.89x** zstd:3 on objects under 64 KiB (2.618 against 2.323 CPU-s/GB) and **0.96x** on all objects. Both pass.
- Did the arm allow a cheaper name path? Not as shipped: with default features `select` runs the classifier on every
  non-empty object under 64 KiB and uses the name only to arbitrate (zdicts `select.rs:124-134`; objects of 64 KiB or
  more return before it, `:139`). A name-only path exists in the same crate (`select_by_name`, or `select` built without
  the `embedded-classifier` feature), but it is a different arm that was not registered; its saving on OS files (many
  without extensions) is unknown. So the timing is of the right arm, and a name-first variant should not have been
  substituted. It should be named as the obvious fix: the file-type study that recommended this set states that the
  classifier's run time was never measured.
- The decision table's own NO-GO clause (a floor missed on 2 corpora) and R5 were not evaluated (deviation 19). The only
  saving evidence is the timing sample of one corpus without an interval: 2.30 % under 64 KiB (floor 5 pp) and 0.74 %
  corpus-wide (floor 1 pp), both under the floors before any CPU charge. That suggests the table would also say NO-GO,
  but it is one corpus of four.
- Required wording: "NO-GO for this release (hard cap, caused by the selector's classifier at about 90 microseconds per
  call; dictionary compression alone passes at 0.89x and 0.96x); saving vote not measured". Recommended measurement: R4.

### D6 (Constellation database dictionaries): agree, NO-GO
Savings at 1 MiB are -0.27 to -0.76 % on all 15 images. One formal gap must be disclosed (§3, item 6): the resampling
unit (file) gives 1 unit for SQLite, 168-174 for MySQL and 875-912 for PostgreSQL, all under the 1,024-unit floor of
§4.4 that removed vm-debian and vm-fedora from the raw-device vote. My re-bootstrap on the host with the object and with
16 MiB extents as the unit gives negative upper ends on every image (for example tlc x SQLite -0.76 [-0.82, -0.70] with
16 MiB extents), so the conclusion holds; a GO is impossible with negative point estimates.

### D7: zdicts and Constellation fire. Lunix: the built-in half follows from D1; the `train`/`apply` half is open with D2.

## 3. Errors and defects found

1. **Unlisted protocol deviation (§3, pv-cover rule).** D2 judged on `pv` although `pv-cover` beat it by 1.28 pp on the
   database sweep volume. Draft §3 "Votes" paragraph; no numbered deviation.
2. **Strict D2 reading presented as the protocol's.** `analyze.py:183-197` (comparator = best of counterfactual and
   shipped sets; cap and cores = worse of both). It is a construction of the analysis; draft line "The protocol's text
   puts the cap against the CF, so the strict reading stands" must go.
3. **Cap extrapolated beyond its sample.** `analyze.py:39-51`: one PostgreSQL DB-page cell for all SE images, worse of
   two for geonames, VM image for every database image at 64 KiB, `pv` cost standing in for `db` and `pv-class`
   (`analyze.py:57`). Disclosed as deviation 20, but the abstract and the D2 table print 0.78 against 13 untimed images
   as if measured, and the counterfactual of the database images at 64 KiB is chosen from non-database content.
4. **`decide.py:62-63`**: D2 fires only if the primary and the confirmatory size both reach the vote. Not protocol text.
5. **`decide.py:93-97`**: the D5 vote row is hard-coded (count 0, verdict string) from one corpus's cell; it is not
   computed from any test. `decide.py:98` derives D7-Constellation from D6 alone for the same reason.
6. **§4.4 floor applied inconsistently.** The 1,024-unit requirement was enforced for the raw-device vote and silently
   not for the Constellation database views (1-912 units). "No decision cell is underpowered" is true only for the
   half-width criterion. Needs a deviation entry and a lead ruling or the extent-level sensitivity as the stated basis.
7. **Block dependence is understated.** With 16 MiB blocks the BCa half-widths are 3.1x wider (median over the 130
   re-run tests) with unchanged point estimates. Independent 1 MiB blocks would give the same width; a factor 3 means
   strong dependence between neighbouring blocks, so every interval printed in the draft is too narrow by about that
   factor and the Wilcoxon p-values (down to 1e-300) are not meaningful as stated. No pass flag changes (largest 16 MiB
   half-width 0.83 pp, all decisive margins are larger), but "16 MiB blocks change no conclusion" hides this, and §6 of
   the protocol says the decision follows the wider interval. At 64 KiB the oracle LB on the host corpora drops to
   3.01-3.03 pp with 16 MiB blocks, so the "not decisive at 64 KiB" statement is itself marginal.
8. **`analyze.py:140`**: with one resampling unit the jackknife divides 0/0 (RuntimeWarning in the log); the code then
   sets the acceleration to 0 and reports LB = S. Harmless for the verdicts, but those rows are not intervals and should
   be blank in `analysis-tests.csv`, not numbers.
9. **Provenance of the decision script.** On the host, `families.csv` was written before the first analysis run (a
   small smoke run, then the final run; `analyze.py` unchanged between them), which supports the draft's claim.
   `decide.py` was modified after its first run on the final results. The draft should say what changed. The
   strict/lenient construction in `analyze.py` was written after the timing data and the aggregate point estimates were
   known (deviation 18 admits the latter); the summary should say that plainly next to the D2 verdict.
10. **Host detail in the draft.** Deviation 1 names the host's login. The rules list the user among host details; write
    "the distribution's default login". Otherwise clean: no address, host name, instance or kernel string in the draft,
    `raw/` or the scripts (the package version strings in `db_shares.csv` are distribution versions, which the protocol
    itself names).
11. **Prices.** None in the draft, `raw/` or the scripts. The protocol amendment is wording only (λ and thresholds
    unchanged, made before timing and analysis). Two remarks for the lead: the first commit of the protocol still
    carries the removed figures in history, and §1.3 keeps the phrase "visible on the bill".

Checked and found correct: S and bytes-weighting; hold-out masks; the paired block bootstrap (same resamples for all
arms) and the BCa formula; Holm step-down with the full family size, also for the 16 MiB subset (conservative); the
exploratory corpora adjusted but not voting; N = LB - 0.1 x delta-cores with delta-cores at 5 and 1 GB/s; the decompress
cap (worst 0.875 on a decisive row); hold-out assertions in the training and run scripts; byte-for-byte reproducibility.

## 4. Misleading or incomplete statements to correct

- Abstract, point 2: "it runs at 0.78x ... so only 2 of 15 images pass" reads as 15 measurements. Say: one PostgreSQL
  DB-page cell, applied to 12 images; MySQL, SQLite, WAL and 64 KiB untimed.
- Abstract and D2 rows: show three readings with their votes (literal 9 and 8 of 15; counterfactual-only 3 of 15 as
  timed, 12 of 15 on N; hybrid 2 of 15) and state that the choice is pending a ruling, not that the text decides it.
- §5 "D2 ... NO-GO under the strict reading, a near miss decided by the cap": add that the cap value is not measured on
  the decision population.
- D5 row and abstract: name the selector as the cause and give the selector-free ratios (0.89x, 0.96x).
- Proposal 3 ("makes the set fail every cap in this study") is correct and should be the headline finding for zdicts:
  the earlier adoption recommendation never priced the classifier.
- Threats table, "Block independence": replace with the 3x widening.
- "Decisive" (D1, 16 KiB): qualify as above.
- Deviations: add the pv-cover rule, the §4.4 floor on Constellation database views, the confirmatory-size rule in
  `decide.py`, and the post-run edit of `decide.py`.

## 5. Required corrections and measurements

| id | what | settles |
|---|---|---|
| R1 | **Lead ruling on D2, written before R2/R3 data are seen:** comparator for saving, cores and cap when D1 is NO-GO; what "confirmatory" means; whether either level may pass; whether the "stricter reading decides" tie-break is accepted | which of 2, 3, 9 or 12 of 15 is the vote that counts |
| R2 | **Timing on the decision population:** quiet host, same method, for all 15 database images at 16 KiB and 64 KiB: a block-stratified sample of the whole volume (LBA-aligned chunks, all classes including WAL), arms zstd-1, zstd-3, pv@1, pv@3 with the volume's own dictionary; a second sample draw on three images for between-sample spread; state the libzstd dictionary path and, as a sensitivity, one tuned path | whether the cap fails per image; the counterfactual of database volumes at 64 KiB; removes the single-cell extrapolation |
| R3 | **`pv-cover` on all 15 database images** (16 and 64 KiB, sizes and timing), as §3 of the protocol requires once the 1 pp trigger fired | the arm D2 must be judged on; whether MySQL SE and tlc-my clear 2 pp |
| R4 | (recommended, cheap) per-object `ft@3`, zstd:3 and oracle pass over the four file corpora, bootstrap by file | makes D5 a table verdict (floors, R5) instead of a cap-only verdict; expected to confirm NO-GO |
| R5 | Text corrections of §3 and §4 above; report 16 MiB intervals next to the 1 MiB ones; extent-level sensitivity for D6 in `raw/` | accuracy of the record; no verdict change expected |

Until R1-R3 are done the summary should print D2 as "raw devices NO-GO; database family: not yet decided (timing
incomplete, reading pending)", and D7-Lunix as "codec 3 with built-ins stays reserved; `train`/`apply` pending D2". This
is not the protocol's "undetermined" (that word is reserved for the sample-size rule); it is an execution gap: the
protocol's own timing and pv-cover requirements were not completed for the cells that decide.

## 6. What could flip a verdict

| decision | can it flip? | by what |
|---|---|---|
| D1 16 KiB | no | oracle under half the floor |
| D1 64 KiB | no | LB under 3 pp on three corpora with no CPU charged |
| D2 raw | no | negative saving |
| D2 database | **yes** | the reading (literal reading: already reached), or R2/R3 moving the cap over 0.80 on enough images |
| D3, D4, D6 | no | derived from D1, or negative savings |
| D5 | not for this release | only a faster selector (name-first path or a faster classifier) re-tested with the saving pass; then the open question is the saving, which one corpus puts under both floors |
| D7-Lunix | partly | follows D2 database |

---

# Final check (2026-11-14): draft 2 against Amendment A1, Ruling R-D2 and the new raw files

## Verdict: accept after corrections (text only; no further measurement required)

The verdicts in draft 2 follow from A1, R-D2 and the data. D2 for database images is presented as what it is: a
mechanical "GO at 16 KiB only" under the pre-data amendment, recorded as "marginal, not established" by a ruling that is
labelled post-data everywhere it appears. D5 is now carried by the decision table. D1 is "decisive among the measured
candidate sets" only. The corrections below are wording, two numbers and one script line.

## What I checked

- **Order of events.** A1 was committed (30e4329) before `d5_objects.csv`, `db_pvcover_arms.csv` and `timing-db.csv`
  existed; R-D2 (af312fd) is after the A1 analysis and says so. On the test host `families.csv` for A1 predates the
  A1 `analyze.py`, its smoke run and the final run; `decide.py` was last written before its logged run.
- **Reproduction.** `decide.py` re-run on the test host from the stored A1 analysis files (one short process, niced,
  directory deleted afterwards): `votes.csv` and `tables.md` byte-identical (5af43b32, 0fea0870). Script and file hashes
  in the reproducibility appendix match the repository.
- **Recount from `raw/analysis-a1-tests.csv` and `raw/timing-db.csv`** (my own script): pv-cover 9 of 15 either level,
  7 of 15 at level 3, pv 5 of 15, 0 of 15 at 64 KiB, 13 and 12 of 15 with the cap not applied; COVER medians 1.0081
  (level 3) and 1.0944 (level 1), and `decide.py:75` requires both to exceed 1 pp; passing caps 0.8020, 0.8044, 0.8088;
  decompress 0.7962 and 0.7997 on the two SQLite images; timing max/min at most 1.019, no flagged cell; 16 MiB
  half-widths 3.22x (median), no pass flag changes, 8 cells over 1.5 pp at 16 MiB, none deciding. All as printed.
- **D5** from `raw/d5_objects.csv` and the 8 D5 tests: S and N as printed; both floors missed on 4 of 4 corpora before any
  CPU charge; selector loss 0.6-1.2 pp under 64 KiB (R5 passes); the per-object oracle reaches only 2.5-3.3 %.
- **Earlier review items.** R1 (A1, pre-data), R2 (own volume-wide cell for each of the 15 images, both sizes), R3
  (pv-cover on all 15), R4 (D5 pass), R5 (16 MiB intervals, extent re-bootstrap): done. Defects 1-10 of §3: addressed
  (deviations 18, 22, A1-A12; one-unit rows blank; D5 vote computed; login reworded). Not done and only partly disclosed:
  the second sample draw and the tuned libzstd dictionary path asked for in R2 (fix 5 below).

## Required text fixes

1. **Status paragraph (lines 3-8).** "Verdicts follow §1.2 and A1 mechanically" → add "except D2 for database images and
   D7-Lunix, which are recorded under Ruling R-D2 (post-data)". And after "`raw/families-a1.csv` ... before the A1
   analysis ran" add "but after the follow-up measurements existed and after their per-image timing ratios and the D5
   aggregates had been seen (deviation A1)". Today that disclosure is only in the deviation list.
2. **D7-Lunix (§0 table, abstract table, §6 row).** "fires (in effect, by R-D2)" overstates a pre-registered result:
   `raw/analysis-a1-votes.csv` says "D7-Lunix: does not fire". Write: "does not fire mechanically (A1: D2 database
   'GO at 16 KiB only'); its consequences (codec 3 reserved, `estimate`/`train`/`apply` out of v1) are applied by Ruling
   R-D2, made after the data". Remove the bold "fires for Lunix" from the abstract table.
3. **§0 evidence table: the noise cuts both ways and the table shows one direction only.** Add two rows:
   - "images failing the cap by as little: compress 0.794-0.796 on askubuntu-pg, askubuntu-my, serverfault-my and
     geonames-pg (all with N 3.2-7.9 pp); with the cap not applied 13 of 15";
   - "counterfactual choice: zstd-3 passes the cap against zstd-1 at 0.803-0.823 on the nine SE images (0.803 on
     serverfault-pg); a failure would change the comparator of that image".
   And one sentence under the table: "Within the same noise the mechanical vote could lie anywhere between 5 and 13 of 15."
   The same balance belongs in R-D2's summary in §3 ("three of its deciding margins" → add "and four images miss by
   margins of the same size").
4. **Two numbers.** Line 192: "Level 1 runs 1.17-1.21x faster than zstd-3 on the SE images" → "1.15-1.19x" (pv@1
   1.17-1.19, pv-cover@1 1.15-1.18). Timing paragraph: "level 1 0.97-1.21" → "0.97-1.19".
5. **Threats and "Deciding experiment".** Add: "Dictionary arms are timed through libzstd's default prepared-dictionary
   path (dictionary state created by level only, default attach or copy mode); a tuned path was not measured. One sample
   draw per image." and make both part of item (2) of the deciding experiment. Without it the 0.80 edge reads as a
   property of dictionaries when it is a property of one configuration.
6. **Host detail in a script.** `spikes/dict-validation/relay_timing.sh:5` names the quiet host's ssh alias and the ssh
   configuration path. Take both from the environment, as `collect.sh` does for the test host, before anything is
   committed. The old `bench/dict-study/validation/SUMMARY.md` still names the host login in deviation 1; it must be
   replaced by draft 2, not kept beside it.

## Checked and clean

- No prices or money figures in draft 2, the new raw files or the scripts; the protocol's "on the bill" phrase is gone
  (the first protocol commit still carries the removed figures in history; the lead's call).
- No address, host name, instance or kernel string in draft 2 or `raw/`; the only host detail found is fix 6.
- Nothing overstated in D1 (qualified, with the 16 MiB bound 0.76-1.20), D5 (table and cap, cause named, and the honest
  statement that a faster selector would not rescue the saving), D3, D4, D6 (extent-level agreement shown, unit counts
  disclosed) or D2 raw.
- Minor, optional: the summary is 429 lines against the protocol's "about 400".
