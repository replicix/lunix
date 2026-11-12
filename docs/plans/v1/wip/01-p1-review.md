# Review of plan 01 (phase 1, local-only volume)

Reviewer: independent, 2026-11-12. Inputs: the plan as of today, DESIGN.md,
DECISIONS.md (ADRs 1-30 with gate outcomes), GLOSSARY.md, ROADMAP.md,
CONVENTIONS.md, `bench/BASELINE.md` and the linked RESULTS files,
`bench/crypto-kat/raw/kat-vectors.json`, replicix-core `6fb59d2`
(`chunkfmt` `format.rs`, `codec.rs`, `e2e.rs`).

## Verdict: **not ready** (ready with fixes once the three blockers and the
"before M1" spec gaps are resolved)

The plan is thorough, honours the phase 0 ublk lessons well (W1-W7, the raw
control ring, no `STOP_DEV`, `abort()` on fatal queue errors, identity
checks before recovery) and its crash-point and model-test gates are the
right shape. It is not executable as written for three reasons: (1) the
exit tests need a host the plan itself says does not exist (150 GiB free
against ~50 GB; a 1 GiB-mlocked daemon against a 600 MiB gate); (2) the
M1.B interface list has gaps that would force `lunix-iface` changes in the
middle of wave 1 on the two long poles (journal group flush, journal buffer
ownership, metadata sealing trait, space-reservation transfer, recovery
floor); (3) several frozen-format decisions are left to "M1 decides" and
would be made by a cheaper model after code is written, including two that
have a 2× performance or a security consequence (journal record framing,
integrity tag on unencrypted extents).

Everything below is fixable in the plan text; none needs a new ADR except
where the table says "maintainer".

## Findings

Severity: **B** blocker (wave 1 must not start), **M** major (fix before the
affected milestone starts), **m** minor.

### A. Correctness against the spec

1. **B — M15 "store on a real disk with at least 150 GiB free"; M18 "about
   40 GB per run"; rule 3 "root runs under 100 GiB".** The host has ~50 GB
   free and phase 1 has no GC (spec gap 7), so every churn workload grows
   the store monotonically. xfstests with two 64 GiB volumes, fio verify
   over 64 GiB at two chunk sizes × two encryption settings, pgbench at
   scale 1 000, three VM images and five M18 file systems cannot coexist.
   Fix: (a) size every exit volume at 16-32 GiB and TEST/SCRATCH at 8 GiB
   each, unless the lead adds a disk; (b) make "delete the volume and
   recreate between cells" the default in M15/M16 scripts and have each
   script `df`-check against a stated budget before `mkfs`; (c) approve M17
   (whole-file sweep) *or* accept growth with the smaller sizes; (d) M18
   runs one file system at a time with `journal_max` set to 256 MiB per
   volume (the 2 GiB default × five volumes alone is 10 GiB of
   preallocated zero-fill).

2. **B — M12 gate "resident memory with one idle 1 TiB volume ≤ 600 MiB"
   contradicts spec gap 15 (8 queues × 128 × 1 MiB = 1 GiB mlocked).** As
   written the gate cannot pass at 8 queues. Fix: state the queue count the
   gate is measured at (the host default `min(8, cpus/4)` = 4 → 512 MiB of
   buffers) and set the gate to "≤ queue buffers + 150 MiB"; report the
   buffer figure separately (the maintainer may later lower
   `max_io_buf_bytes` per volume).

3. **B — M13 is listed in wave 1 ("Depends on: M1") but its `fsck` and
   `key ls|add-slot|rm-slot` subcommands link `lunix-fsck` (M14) and
   `lunix-crypto` (M2), and its gate item 5 needs volumes "built by the
   M7/M2 libraries".** Fix: split M13 into M13a (wave 1: socket commands,
   size parser, secrets handling, `--help` snapshots, fake-server round
   trips) and M13b (wave 2, after M2/M7/M14: `fsck` and `key`). Update the
   dependency graph and `ownership.toml` accordingly.

4. **M — ADR-9 "every object a commit names is durable before the commit is
   created" against `index_packs[]` and the C1-C9 order.** C8 persists the
   index run *after* C6 writes the FULL commit, so the commit cannot name
   the run that covers its own segments. The plan never says which runs a
   FULL names. Fix (M1 format doc + M11 step 5): "`index_packs[]` of FULL
   *n* lists the runs that were durable before C6 of *n*; they cover
   segments named by commits ≤ *n−1*. C8's output is named by the next
   FULL." Add to the M11 order test: a commit never names a run that is
   not yet durable. (Phase 3's bucket-resident index lookup depends on this
   being exact.)

5. **M — Orphan segments after a crash between C4 and C6 can be adopted by
   dedup and then never named by any commit.** M7 step 6 lists orphans "for
   the engine, which decides"; M11 step 6 just says "orphan sweep via the
   store"; M6 `catch_up` "scans footers of segments newer than the
   manifest's high-water mark and inserts their entries", which would
   ingest the orphans. A later dedup hit then points the map at a segment
   that `new_segments[]` never lists; fsck calls it an orphan and M17 (or
   phase 2 GC) deletes live data. Fix: define the rule once, in M11 step 6
   and M7 step 6: *at open, every sealed segment and map pack whose id is
   above the tip commit's high-water mark and that no commit names is
   deleted before `index.catch_up` runs* (safe: ids are never reused and
   the journal still holds the data because reclaim is after C6). M6's
   `catch_up` takes the set of segment ids named by the chain, not "newer
   than the manifest". M14 reports such files as `orphan` only when the
   volume was closed cleanly.

6. **M — Unencrypted extents have no integrity check on local reads.**
   DESIGN §6 writes `… + payload + tag(16)` with only the crypto fields
   bracketed; ADR-7 lists "AEAD tag" in the framed header. The plan's
   `PlainExtents` is "framing only, no AEAD", and M11's local read path
   checks "header, id prefix, AEAD tag" and does *not* recompute the chunk
   id (spec gap 10). On an unencrypted volume a flipped payload byte in a
   raw-codec extent is therefore returned to the guest as data. Fix
   (maintainer, before M1, format): the 16-byte tag field is present in
   every extent; on unencrypted volumes it holds BLAKE3-128 of the payload
   bytes, checked on every read and by fsck. This keeps one framing, costs
   nothing measurable, and makes "no recompute on local reads" defensible
   on both volume kinds.

7. **M — Journal record framing is a frozen format with a 2× cost
   consequence, left to M1.** M1 item 2 says "record `{…}` 4 KiB-aligned
   with data following" and M3 says "a 64 KiB write is one record". If the
   header occupies its own 4 KiB block, a 4 KiB guest write costs 8 KiB of
   journal I/O and halves the 4 KiB write rate against the spike's figure
   that the M3 perf gate (≥ 0.8×) is measured against. Fix (lead, before
   M1): header inline at the start of the first block (64 bytes), data at
   offset 64, record padded to 4 KiB; the lane copies the data into its own
   aligned fixed buffer (a 4 KiB `memcpy` is ~100 ns). State the layout in
   `on-disk-formats.md` and let M1.B's `try_append` take `&[u8]` (copied),
   see finding 13.

8. **M — `Err(MidRingCorruption)` in M3 step 4 is unimplementable as
   specified.** Recovery is told to flag a valid record after the stop
   point whose `seq ≤ the last flushed seq`, but nothing on disk records
   the last flushed seq (the checkpoint is a hint; no flush marker exists).
   Fix: drop the hard error from `recover`; recovery stops at the first gap
   as DESIGN §4.1 says and ignores everything after it; M14 reports
   "records beyond the first gap" as a warning with the seq range. Do not
   add a flush-marker record (it is a format change and an extra write per
   group).

9. **M — Journal recovery needs the commit's `cut_seq` as its floor, not the
   checkpoint.** With per-lane rings and wrap, "scan from the first file"
   meets stale records of the previous pass; the plan's stale rule (seq
   lower than expected) has no "expected" at the start of a scan. Fix
   (M1.B signature + M3): `recover(hint: Option<&Checkpoint>, floor: Seq)`
   where `floor` is the tip FULL's `cut_seq` (trusted, authenticated).
   Scan every file of every lane, keep records with `seq > floor`, merge by
   seq, stop at the first gap. The checkpoint becomes a pure optimisation
   (which files to skip) and a lost or stale one is harmless. The M3 model
   test then needs a ring-wrap case with a *lost* checkpoint.

10. **M — Commit `seq` is overloaded with write `seq`.** GLOSSARY and §8.4
    ("EPOCH commit at tip + 1", "probe forward until a 404") imply commit
    numbers are contiguous integers; FULL carries both `seq` and `cut_seq`.
    The plan never says `commit.seq = parent_seq + 1`. Fix (M1 format doc,
    M11 C6, M14 step 2): commit seq is a counter starting at the genesis
    commit (seq 0, `parent_seq` = none, `parent_hash` = BLAKE3 of `volume`),
    independent of write seq. Also state that phase 1 sets `chain_floor` to
    the genesis seq and deletes no commit file (M17's "chain_floor = tip" is
    wrong and would break M14's "chain from genesis").

11. **m — Node-header codec byte duplicates the extent codec.** ADR-11's
    table puts map nodes under the extent scheme (`CCH1` header with its
    own codec id). M1 item 5 adds a second codec byte in the node header.
    Fix: node header carries `level` and `fanout` only; interior-node
    compression (spec gap 2) is expressed by the extent codec (0 raw in
    phase 1, 1 zstd later). One fewer frozen field.

12. **m — Metadata AAD object name must be the bucket-relative name.** M1
    item 7's AAD is `"lunix meta v1\0" ‖ volume ‖ kind ‖ object name`; local
    commits live at `state/commits/<seq>`. State that the name in the AAD is
    always `commits/<seq:016x>` (and likewise for every kind), so the local
    bytes upload verbatim in phase 2 (ADR-7, §13.3).

13. **m — Missing fields in frozen bodies.** EPOCH has no listed body
    (§12 writes `EPOCH{authority: B}`): add `authority?`. Lease: ADR-24
    step 6 writes `transferred_to` (best effort): add it as optional.
    `snapshots/`, `readers/`, `holds/` bodies: name the §6.1 fields in the
    freeze list rather than "frozen". `dicts/`: see finding 27.

14. **m — M9 worker default `min(16, cpus)` plus up to 8 queue threads and
    fio on 16 vCPUs oversubscribes the host and will skew M16.** Default
    `max(2, cpus/2)`, configurable.

15. **m — M11 gate "at most one fdatasync in flight"** is wrong with lanes;
    it is one *group* in flight (one fdatasync per lane with unflushed
    appends). Reword.

16. **m — `/metrics` (M12 step 9) is not in ROADMAP phase 1.** Keep it but
    mark it deferrable so M12, the integration bottleneck, can drop it.

### B. Parallelism: is the M1 boundary sufficient?

17. **M — FLUSH group commit across lanes has no interface.** M1.B gives
    `JournalLane::submit_flush()` per lane and the lane "never blocks", but
    a FLUSH arriving on queue 0 must `fdatasync` every lane with unflushed
    appends, *after* those lanes' writes have completed. Submitting on
    another thread's io_uring is not allowed, so either each lane thread
    must be poked and a barrier assembled, or a flusher owns a ring. The
    plan leaves this to M3 and M11 separately, which guarantees a mid-wave
    iface change on both long poles. Fix (M1.B): `Journal::submit_group_flush
    (upto: Seq, token) ` on a ring owned by the engine's flush coordinator
    (a dedicated thread; fds are shared), which waits for per-lane
    completion watermarks `≥ upto` before issuing the fdatasyncs; lanes
    expose `completed_watermark() -> Seq`. Document that a FLUSH covers
    writes completed before the group formed.

18. **M — `try_append(record)` buffer ownership is unspecified.** Zero-copy
    from the ublk request buffer (registered fixed buffers, lifetime tied to
    the tag) and copy-into-lane-buffer are different signatures and
    different lunix-ublk obligations. Fix: copy (finding 7); signature
    `try_append(&RecordHeader, data: &[u8]) -> Appended{loc} | Full |
    NoSpace`, with the lane owning a bounded pool of aligned fixed buffers;
    `Full` also when the pool is exhausted.

19. **M — No metadata-sealing trait.** M1.B item 8 defines `ExtentCodec`
    and `CommitCodec`; M7 says footers are sealed "through `ExtentCodec`'s
    metadata path", but footers, lease, pins and `dicts/` use
    XChaCha20-Poly1305 under per-kind metadata keys, a different scheme.
    Fix: add `MetaCodec { seal(kind, name, body) -> Vec<u8>, open(kind,
    name, bytes) -> Vec<u8> }` to `lunix-iface::crypto`; `CommitCodec` is
    `MetaCodec` plus the HMAC/chain logic. M2 implements both; testkit's
    `PlainCodec` implements all three.

20. **M — `SpaceGate` has no reservation transfer.** `used = journal_reserved
    + segments + …`: a write reserves under `journal_reserved`; at destage
    the bytes become `segments` (fewer, after dedup and compression) and
    the journal reservation is released at C9. With only `reserve(kind,
    bytes)`/`release`, M9/M11 cannot keep the ledger exact and M8's
    "exactly zero after a million pairs" gate is meaningless end to end.
    Fix (M1.B): `transfer(from_kind, to_kind, from_bytes, to_bytes)` and a
    documented accounting walk-through (admission → destage → commit →
    reclaim) that M8, M9 and M11 all cite.

21. **M — M1 is the serial long pole and is too large for one agent.** 14
    frozen types with byte layouts, golden files, proptest, allocation
    tests, 11 traits with contracts, 8 fakes with crash models, oracles,
    strategies, tools and CI. Fix: (a) M1.A0 (lead or design-heavy agent,
    first): `docs/reference/on-disk-formats.md` byte tables and
    `FREEZE.md` *before any code*, reviewed by the maintainer; (b) M1.A
    implements it; (c) M1.B1 traits + crashpoint macro + ctl protocol
    (small, starts at A5); (d) M1.B2 testkit, in parallel with M2, M3, M5,
    M6, M7, M8, M13a (which need no fakes beyond `PlainCodec`); M4, M9,
    M10, M11 start when B2 lands. Wave 1 depends on *types and traits*, not
    on golden bytes, so the maintainer's layout review can overlap wave 1.

22. **M — M12 is the integration bottleneck and the first place real
    crates meet.** It waits for ten milestones and one agent then runs the
    stack model test where the real bugs surface. Fix: start M12 as soon as
    M11 + M3 + M4 + M7 merge, wiring real crates incrementally with fakes
    for the rest (the stack test is parametrised by which crates are real);
    allow two agents (control plane/daemon vs stack tests). Have M14 reuse
    M12's in-process volume builder (export it from `lunixd` as a
    `testsupport` module behind a feature) instead of writing a second
    mini-engine; until M12 lands, M14 can build volumes with its own
    builder but must not be the first integration.

23. **m — `Completer::complete` threading rule is implicit.** libublk 0.4.8
    commits only from the queue thread; say in the trait doc that
    `complete` is called only from `on_request` or `poll` on that queue's
    thread, and that `poll` is the only path for deferred completions.

24. **m — `check-ownership.sh` and M15's `fix(<crate>)` commits conflict.**
    Either the script allows commits titled `fix(` on `tests/`-owning
    branches, or M15 only reports failing tests. Pick the latter.

25. **m — M9/M11 against fakes is realistic** provided the fakes carry the
    same contracts as the traits (non-blocking vs blocking) and the
    `MemJournal` implements the floor-based recovery of finding 9 exactly.
    Add to the M1 gate: every fake has a contract test shared with the real
    crate (a `lunix-iface::contract` test macro both run), so M3/M4/M5
    prove they match what M9/M11 were built against.

### C. Executability by cheaper models

26. **M — "Where the spec is silent M1 decides" covers ~10 frozen byte
    layouts and three semantic choices (genesis `parent_hash`, first commit
    seq, record framing).** See finding 21: decide them in M1.A0 with the
    maintainer, not after code. Give M1.A a table of every decision already
    taken (this review's spec-gap table) so it does not re-decide.

27. **m — Gates that are not mechanically checkable:** M9 "§20 target 3
    GB/s reported as met or not" (fine, informational); M4 "perf
    informational"; M2 "attach matrix equals a hand-derived table" (write
    the table into the plan or the on-disk-formats doc so the implementer
    does not derive it); M18 "note's claims stay inside the data"
    (reviewer judgement; acceptable). M5 gate at "fanouts 512 and 256" and
    M14's corruption matrix are fine.

28. **m — Scratch path inconsistency:** M10's gate says `$LUNIX_SPIKE_DIR/
    m10/`, rule 2 says `$LUNIX_HOST_BASE/<m>/scratch`. Use one.

29. **m — Watchdogs:** W4 covers control commands; add a per-test outer
    `timeout -k` for every root script (M10, M12 smoke, M15, M16, M18) and
    a wall-clock cap per M15 crash-point run (3 runs × ~40 points must fit
    a stated budget), and a `df` guard before every `mkfs` (finding 1).

### D. M18 (dictionary study)

30. **M — M18 does not need Lunix and should not wait for M12.** The
    measured population is "unique non-zero chunks of the device image
    after a FLUSH". A loop-backed image of the same file system, chunked
    offline (zero chunks dropped, dedup by BLAKE3 at 4/16/64 KiB) gives the
    identical set; Lunix adds only the id-prefix and the hint. Fix: capture
    from a loop image with an offline chunker (a small tool under
    `tests/dictstudy/`), and run *one* validation cell through a Lunix
    volume when M12 exists (stored unique set equals the offline set). The
    debug-export request to the lead disappears, M18 can start in wave 1,
    and the root-lock contention with M15/M16 shrinks to mkfs/mount time.

31. **M — Compute budget is unstated and, as written, is days of CPU.**
    Cells: 5 folds × 4 dictionary sizes × 3 sample sizes × 4 class settings
    × 3 chunk sizes × ~30 runs ≈ 20 000 ZDICT trainings (10-60 s each on
    100 MB), plus zstd-19, xz and brotli over ~40 GB per run. Fix in
    PROTOCOL.md: the size and sample sweeps run on two file systems (ext4,
    one database run) only; `d`, `k`, `steps` fixed once on a pilot fold
    and recorded; zstd-19/xz/brotli measured on a 1 GB block-stratified
    random sample per run, not the whole export; state the expected
    CPU-hours and the host's 16 vCPUs shared with four milestones.

32. **m — Statistics are sound** (pre-registration, blocked k-fold,
    paired Wilcoxon with rank-biserial, block bootstrap with percentile and
    BCa, Holm, lower-bound-clears-threshold). Two refinements: (a) 1 MiB
    extents inside a database table or VM image are not independent; add a
    sensitivity run with 16 MiB blocks and report whether intervals widen;
    (b) the primary metric must be weighted by the raw/hole share that
    ADR-30 says caps the gain, so report "volume-wide stored bytes" with
    holes and raw extents included, not only the compressible subset.

33. **If the maintainer moves codec id 3, the dictionary blob format and a
    trainer/evaluator into replicix-core** (so Constellation can use them):
    (a) M1 must *not* freeze the `dicts/` body; freeze envelope + kind id
    only, like `heat/` (the body is then `{ver, bytes}` defined by the core
    crate); (b) M18's tooling splits into a generic corpus container +
    trainer/evaluator library (candidate crate `replicix-zdict`: zstd-with-
    dictionary codec behind `CODEC_ZSTD_DICT`, dictionary blob
    `{magic, ver, dictID, class, trained_on_hash, bytes}`, `train`,
    `evaluate`) living in replicix-core, and a Lunix study harness that
    depends on it; the harness itself stays in `tests/dictstudy/`; (c) add
    a Constellation arm with its own pre-registered hypothesis family:
    whole files < 4 MiB as single objects and 4 MiB chunks of larger ones,
    taken from the *files* of the same corpora (not the block image; file
    system metadata is irrelevant there), classes by file extension (top
    20 by bytes, rest "other"), dictionary sizes 32/64/112 KiB, evaluated
    per extension and corpus-wide; (d) the two arms share the container,
    seeds, the analysis command and the reviewer checklist, but each has
    its own primary metric and Holm family; (e) replicix-core gets the
    codec-3 decoder test vectors from the arm that first produces a
    dictionary, pinned as goldens. This is a scope increase of roughly
    one third; it belongs to a separate plan in replicix-core with M18 as
    its first consumer.

### E. Other

34. **m — Host prerequisites:** toolchain 1.98 needs `rustup` on the host
    (BASELINE's host had 1.96); say who installs it. xfstests for ublk
    devices needs `TEST_DEV`/`SCRATCH_DEV` as block devices and a
    non-root-reserved mount tree; document the config file in M15.

35. **m — `cargo tree -e features` check in `tools/ci.sh`** must name the
    exact edges it rejects (`crashpoints`, `test-rng`, `tripwire` enabled
    on any non-`dev-dependencies` edge); otherwise an implementer will
    write an unreliable grep.

## Spec-gap resolutions

"Before M1" means the resolution changes a frozen type or a trait and must
be fixed in the plan before M1.A0 starts. "Maintainer" means it changes
DESIGN or an ADR and needs a gate-outcome or amendment block.

| # | gap | recommended resolution | who | before M1? |
|---|---|---|---|---|
| 1 | control protocol | Adopt length-prefixed JSON frames over the Unix socket; record it as a DESIGN §16 decision ("Constellation framing") in the same commit that lands M1.B. gRPC adds a build dependency with no phase 1 benefit. | lead | before M1.B |
| 2 | fanout, interior compression | Adopt the plan: `map_fanout` in the descriptor (legal 256, 512; default 512); interior nodes raw in phase 1 via the extent codec (finding 11), no node-header codec byte. Record as an ADR-8 amendment. | maintainer | yes |
| 3 | CCH1 widths vs KAT | Adopt the plan: stored header keeps CCH1's 2-byte codec and 8-byte `ulen`; AAD and `info` use the vectors' 1/1/4/4-byte canonical widths. Both are safe (chunk ≤ 1 MiB, codec ≤ 255). Record under ADR-11 as "AAD is a canonical re-encoding, not the header bytes". | maintainer | yes |
| 4 | index pack path | Add `index/<pack:016x>` to §6.1 and to the ADR-26 text; freeze the run format (finding 4 fixes what a FULL names). | maintainer | yes |
| 5 | byte layouts | Decide them in M1.A0 (byte tables first, maintainer review, then code), finding 21. Genesis `parent_hash` = BLAKE3 of the `volume` bytes; genesis commit seq 0. | lead → maintainer sign-off | yes |
| 6 | `heat/`, `gc/`, `gc/space/` bodies | Adopt the plan (envelope + kind id + `{ver: u16, bytes}`); extend the same rule to `dicts/` if finding 33 is adopted. | maintainer | yes |
| 7 | no GC in phase 1 | Approve M17 (whole-file sweep, offline, tip-only) now, scheduled in wave 2; and shrink exit volumes (finding 1). The 16 B/chunk index budget, 2 GiB `journal_max` and 256 MiB headroom are fine as plan defaults; record them in `volume.json` defaults, not the spec. | lead (M17: maintainer) | no, before M15 |
| 8 | journal lanes | Adopt lanes (one per queue, global seq, merge at recovery) with the group-flush interface of finding 17 and the floor-based recovery of finding 9; record `epoch` = 0 locally. Record as a DESIGN §4.1 clarification. | maintainer | yes (file header, record framing) |
| 9 | local file set | Adopt the plan (`volume` = exact descriptor bytes, `volume.json` local settings, `families/<id>/keyring`, `state/device` with `boot_id`, registry `volumes.json`). Update DESIGN §4's directory listing. | maintainer | yes (item 14 of the freeze list) |
| 10 | local reads skip id recompute | Accept the plan's default *together with* finding 6 (tag on every extent); `verify_reads` flag; M16 measures the cost of `verify_reads = true` so the maintainer can flip the default with data. | maintainer | no (M11) |
| 11 | "journal deduplicates by seq" | Wording: replace with "reissued requests are re-admitted with a new seq; the reissue is idempotent at the block level". | lead | no |
| 12 | CONVENTIONS conflicts | Add a "Phase 1 execution" section to CONVENTIONS that points at this plan's rules (commits in worktrees, per-milestone `CARGO_TARGET_DIR`, remote host). | lead | before wave 1 |
| 13 | queue count vs host | M16 runs Part A at 4 and 8 queues, reports both with the CPU split; the §20 gate is judged at the better of the two with the oversubscription stated. | lead | no |
| 14 | power-loss emulation | Keep as a stretch in M15; it is a phase 8 CI item in DESIGN §21. | lead | no |
| 15 | mlocked buffers | Finding 2; add `max_io_buf_bytes` (default 1 MiB, min 256 KiB) as a per-volume attach option so a host can trade it against `max_sectors`. | lead | no |
| 16 | `on_hang` watchdog | Out of phase 1, as the plan says; note it in ROADMAP phase 8. | lead | no |
| 17 | "xfstests passes" | Adopt the plan's reading (no test fails that passes on a `loop` reference in the same session); also require the `-g quick` exclusion list to be committed and justified per test. | maintainer | no |
| 18 | ROADMAP/ADR-30/ADR-5 text | Move the benchmark to phase 1 in ROADMAP and ADR-30; if finding 33 is adopted, say the trainer lives in replicix-core. | maintainer | no |

New gaps found by this review (not in the plan's list): finding 4
(`index_packs` lag), 5 (orphan adoption), 6 (plain-extent tag), 7 (record
framing), 8-9 (recovery floor, `MidRingCorruption`), 10 (commit seq and
`chain_floor` in phase 1), 12 (AAD object name), 13 (EPOCH/lease fields).
Of these, 4, 6, 7, 10, 12, 13 must be decided before M1.

## Top risks to phase 1 finishing

1. **M1 decides frozen formats late and alone.** A cheaper model freezing
   ~10 byte layouts after writing code, reviewed afterwards, is the single
   largest source of "migration in phase 2". Mitigation: M1.A0 layout
   document with maintainer sign-off before code; wave 1 starts on types
   and traits, not bytes (finding 21, 26).
2. **Integration surfaces only at M12.** Ten crates built against fakes
   meet for the first time in one milestone with one agent; interface
   mismatches (findings 17-20) would each cost a `lunix-iface` change and a
   rebase of every wave-1 branch. Mitigation: close the five interface gaps
   in M1.B, shared contract tests for fakes and real crates, M12 started
   incrementally with two agents.
3. **Host capacity.** ~50 GB free, 16 shared vCPUs, one root lock. M10,
   M12 smoke, M15 (fio × 8 configs, xfstests × 4 + reference, ~120
   crash-point runs, 150 random kills), M16 (pgbench 30 min cells, VM
   churn) and M18 (five file systems) are all root-serialised; at face
   value that is a week of exclusive host time plus disk the host lacks.
   Mitigation: finding 1 sizes, M17, M18 decoupled from root (finding 30),
   a published root calendar, and a second host or a disposable VM for M10
   and M15 if the lead can get one.
4. **A ublk wedge on the shared host.** Phase 0 needed a reboot twice. The
   W rules are right; the residual risk is a cheaper model "retrying" a
   stuck command. Mitigation: outer `timeout -k` on every root script, the
   `root` wrapper refuses to start if the previous run's leftovers file is
   non-empty, and an explicit "stop and report, never retry" line at the
   top of M10, M12, M15, M16, M18.
5. **Crash-consistency subtleties found in M15 instead of M11.** Lane
   merge with torn tails, ring wrap without a checkpoint, orphan adoption,
   and reclaim-before-commit are all cheap to model and expensive to debug
   under xfstests. Mitigation: findings 5, 8, 9 in the plan now; the M11
   model test adds "ring wrap with lost checkpoint" and "crash between C4
   and C6 then re-destage of the same data" as named cases; M12's stack
   test runs every crash point at both chunk sizes and both encryption
   settings before M15 starts.

Perf targets are not a finishing risk (ROADMAP allows an explained
shortfall), but the lead should expect the 4 KiB write row to miss on this
flush-bound disk (the spike's one-thread journal did 36.5 K rec/s) and
budget M16's explanation time accordingly.
