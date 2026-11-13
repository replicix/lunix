# Review of M9 — `lunix-destage` (branch `p1/m9-destage` at 2f37325)

Independent review of the destage pipeline against the plan's M9 section and
gate, D16-D20, cross-cutting rule 13 (R1), the commit protocol of M11 step 5
(C1-C10), DESIGN §5, §6, §7, ADR-3/5/6/11/12, the `lunix_iface` rustdoc for
`overlay`, `index`, `crypto`, `store`, `map` and `space`, and the testkit
fakes it is tested with. The gate was re-run on the remote host in a fresh
directory; mutations and one extra test ran in a scratch copy, both deleted
afterwards. `rust-skills` ids are cited where they apply; plan and spec win.

## Verdict: **merge after fixes** (one blocker in the M9/M11 contract)

The per-chunk pipeline is right: every chunk of the cut gets exactly one map
op, holes and all-zero chunks never store an extent, partial chunks are
patched onto a base read under R1 (leaf prefix, chunk size, no scan), the
D17 confirmation reads the hinted extent with the full 32-byte id and the
tag, and a `Verified` hit is only taken from the in-process table. I could
not construct a forged run entry, stale hint, colliding prefix or tampered
extent that makes the map point at wrong data without an `Auth`/miss, in
`plain` or in the keyed modes (details under "Dedup and R1 walk"). The map
is written in one transaction, so a failed cut applies nothing, and the
layout is independent of the worker count except in one documented corner.
The code is clean, bounded and free of `unsafe`, `unwrap` and disk-data
indexing.

The blocker is not in the pipeline but in what the function hands to M11:
it **commits the map while the last segment is still `.open`**, and the
engine's read path (plan M11 step 4) reads blocks the overlay does not hold
through the map. In that window a guest read of an untouched block of a
partially overwritten chunk goes to a file the store refuses to read. The
fix is small and inside M9.

## Gate re-run (host, 4 pinned CPUs, `-j 4`, load average 200+)

| step | result |
|---|---|
| `cargo test -p lunix-destage` (debug) | **39 passed**: unit 1, behaviour 19, compression 7, confirm 8, crypto 2, model 2 (2 731 / 2 635 random cuts at 16 / 64 KiB), perf 3 ignored. Matches the report. |
| `cargo clippy -p lunix-destage --all-targets -- -D warnings`, same with `--features crashpoints`, `cargo fmt --all --check` | clean, no diagnostics |
| `cargo test -p lunix-destage --features crashpoints` | **40 passed** (the 39 plus `the_crash_points_are_registered`) |
| perf, `--release`, ignored tests, `--test-threads=1`, load average 230-270 | zero scan **139.6 GB/s** per core; BLAKE3 **10.59 GB/s** per core (10.60 through `chunk_id`); destage 8 workers, 128 MiB text, 64 KiB: runs **1.02, 1.25, 1.23, 1.23, 1.23 GB/s**, median 1.23, floor 1.0 held; stored/in 0.315; per core-second of input: assemble 2.27, hash 10.0, compress **0.38**, seal 13.2, append 17.9 GB/s; §20 target not met (see "Perf") |
| mutation A: `Hinted` taken as `Verified` (`worker.rs:246`, no confirmation) | **4 of 8 `confirm` tests fail** (`a_hinted_hit_naming_another_valid_extent_is_a_miss`, `..._with_a_damaged_tag_...`, `a_correct_hinted_hit_is_adopted_without_a_store` on the `dedup_confirmed` count, `an_unreadable_or_absurd_hint_...`: the ghost hint was adopted blindly); everything else passes |
| mutation B: base read without the leaf prefix (`read.rs:97` `id_prefix: None`) | **2 fail**: `r1_a_leaf_naming_a_with_a_hint_at_b_fails_the_rmw` (the forged RMW *succeeds*, B's bytes patched) and `r1_a_stale_hint_falls_back_...` (no fallback happened) |
| mutation C: in-cut dedup through `appended` off (`pack.rs:66`) | **4 behaviour, 1 crypto and both model tests fail**, loudly: the second chunk with an id finds its slot `Taken` and the cut fails `Invalid` ("sealed extent taken twice"), never a wrong map |
| extra test: read through the map after `destage` and before `seal_open` of a chunk with one of four blocks overwritten | `read_extent` of the new entry returns `NotFound { what: "segment" }` while the overlay holds 1 of the chunk's 4 blocks (finding 1) |

The branch touches `crates/lunix-destage/**`, `Cargo.lock` with its own
`deps:` commit (the plan allows the `lunix-crypto` dev-dependency once M2 is
merged; it is) and the report. Ownership as the report says.

## Dedup and R1 walk (what I tried to break)

- **Forged run entry `X -> hint(E_B)`** (E_B valid, id B): `confirm_hint`
  (`read.rs:48-74`) opens with `chunk_id = Some(X)`; the codec compares all
  32 header bytes before the tag (`lunix-crypto/src/extent.rs:85-98`,
  testkit `plain.rs:215`), `Auth`, counted, miss, stored. Tested over five
  codec flavours (`confirm.rs:26`).
- **Forged entry with a forged extent carrying id X** (attacker writes the
  bucket): in `integrity`/`encrypted` the tag is keyed and the id itself is
  keyed (D18), so neither can be produced without the key. In `plain` the
  attacker can produce a tag-valid extent with header id X and any payload;
  the confirmation accepts it and the map points at it. That is exactly what
  D16 promises for `plain` ("no protection against anyone who can write the
  bucket") and nothing in M9 can close it; `verify_id` on the confirmation
  would (it recomputes the id from the plaintext) at the price of a
  decompression per hit. Not a finding; worth one sentence in `lib.rs`
  (nit 15).
- **Colliding 16-byte prefix** (`id_len = 16`): the map entry stores a
  prefix, but the confirmation and the in-cut claim use the full id
  (`worker.rs:241-264`, `pack.rs:66`), and the base read's fallback filters
  candidates with `prefix.matches` and then confirms the **full** candidate
  id (`read.rs:115-124`). A second chunk sharing the prefix cannot be
  adopted for the first. In `plain` a bucket writer can still search such a
  collision offline (D18 keeps the residual there, by decision).
- **Stale leaf hint at a tag-valid extent of another chunk** (R1): the base
  read passes the leaf prefix and `chunk_size`, `Auth`, counted, fallback by
  prefix, every candidate confirmed; with no candidate the original `Auth`
  fails the cut and the map is untouched (`confirm.rs:171-214`, the
  `r1_*` tests). `cand.hint == hint` is skipped, so the bad copy is not
  retried.
- **Absurd hint** (`clen` 0, above the chunk size, `off` past the file,
  ghost segment): rejected before or by the store, no read of
  `clen = u32::MAX` (`read.rs:33-39`).
- **Condemned segment**: `get_for_dedup` never returns one; the fallback
  (`get_by_prefix`) may return one, which is correct: the base read only
  *copies* bytes out of it, it creates no reference.
- **Superseded versions inside a cut**: the overlay resolves newest-wins per
  block in `cut` (`overlay.rs:67-70`, testkit `overlay.rs:124-146`), so
  destage sees one piece per block; writes above `c` are not in the view and
  are patched onto *this* cut's extent at the next cut. Correct per ADR-3.
- **Holes**: fully hole-covered chunk, no read (`assemble.rs:91-93`);
  partial holes patch zeros onto the base; an all-zero result is a hole
  whatever the pieces were (`worker.rs:224-232`). Chunk boundaries at 16
  and 64 KiB are exercised by the model and by `check_pieces`.
- **Every write `<= c` reaches the map**: one `ChunkResult` per cut chunk,
  pushed in job order = chunk order (`check_cut` enforces strictly
  ascending), one `MapOp` each (`pack.rs:57-80`), one `commit`. The model
  test compares the whole device against the log at every cut.

## Findings

Severity: **blocker**, **major**, **minor**, nit.

### Blockers

1. **blocker — the map transaction is committed while the last segment is
   still open; a guest read in the C3-C4 window hits an unreadable file.**
   `pipeline.rs:300-310` commits the `MapTxn` after packing; the writer of
   the last segment is returned open (`outcome.rs:43-45`, `pack.rs:137`)
   for the coordinator's C4. The rustdoc at `outcome.rs:28-33` argues the
   window is safe "because the overlay still serves those blocks". It does
   not: the overlay serves only the blocks the cut *wrote*. Scenario, 16
   KiB chunks: cut 1 stores chunk 0 (blocks 0-3). The guest writes block 0;
   cut 2 reads the base (`cold_rmw`), patches, appends the new extent to
   the open segment and commits the map entry of chunk 0 pointing at
   `segments/<id>.open`. Before `seal_open`, the guest reads block 2: the
   overlay has only block 0, so the read goes to `BlockMap::try_get_cached`
   (M11 step 4, plan 1946-1950) and then to the store, which "never reads"
   `*.open` files (store rustdoc, plan 1601); `NotFound`, the index knows
   nothing yet (inserts are held back by design), EIO to the guest for data
   that is intact on disk. ext4 remounts read-only on such an EIO. The
   extra test in the scratch copy reproduces the store side (see the run
   table). The same holds for every `Store` chunk of the last segment, not
   only RMW ones, as soon as a read of an un-overlaid block of such a chunk
   happens; it does not hold for adopted chunks (their segments are sealed)
   or rolled segments (sealed inside the cut).
   **Fix (recommended):** seal every segment the cut appended to **before**
   `txn.commit()`: call `packer.seal_current()` at the end of packing
   (`pipeline.rs:298`), drop `DestageOutcome::open` and `seal_open`, keep
   `DestagedSegment::sealed` always `Some`. DESIGN §5 step 7 says exactly
   this ("seal the segment at the target size **or at a commit cut**"); the
   plan's C4 then becomes the coordinator recording `new_segments`, the
   `transfer(Segments, ...)` true-up and the `engine.c4` crash point, with
   `destage.after_segment_seal` as M9's own point. Every map entry then
   names a sealed, durable file at all times, which also removes the
   ordering trap around `insert_into_index`. **Alternative** if the lead
   wants C4 to stay the coordinator's durable step: return the `MapTxn`
   uncommitted in the outcome and have M11 commit it after C4; that keeps
   the plan's step split but changes "map updated in memory at C3" to
   "after C4", and the outcome gains a lifetime. Either way the rustdoc
   claim must go, and a gate test must read every map entry of the cut
   through the store right after `destage_cut` returns.

### Majors

2. **major — a cut that dirties more leaves than the map's cap can never
   commit, and each attempt stores the whole cut as orphans.**
   `pipeline.rs:300-310` runs the map transaction last, after every extent
   is on disk. `lunix-map/src/txn.rs:44-48` and `:138-140` count the
   transaction's *own* new leaves against `dirty_leaf_cap` even when the
   overlay is empty, so after the coordinator's seal-and-retry (report
   decision 4) the same cut fails the same way; the plan's `MapFull`
   trigger (M11 step 5) assumes a seal makes room, which is false for an
   oversized cut. Scenario: 16 KiB chunks, fanout 512 (one leaf per 8 MiB
   of LBA), default dirty-bytes trigger `B` = 1 GiB = 65 536 chunks; random
   4 KiB writes over a volume above ~160 GiB touch more than the default
   20 000 leaves. Every attempt appends up to 1 GiB of orphan segments
   (finding 3), so the loop also eats the disk. **Fix:** (a) probe the cap
   before storing anything: `begin_update`, `set_hole(ci)` for every cut
   chunk, drop the transaction (discarded on drop, per the trait); it runs
   the same `touch` path and returns `MapFull` with zero bytes written
   (`FlatMap::with_dirty_leaf_cap` makes it testable: `MapFull` with
   `Partial::segments` empty); (b) state in `DestageError`'s rustdoc that
   `MapFull` returned while `map.dirty_leaf_count() == 0` means the cut is
   too large for the map and must not be retried as is; (c) the plan's M11
   step 5 needs a cut bound in distinct leaves (`chunk / fanout` over the
   cut's chunk list is one pass) or a split, which the report defers.
   (`err-doc-errors`)

3. **major — sealed segments of a failed cut stay on disk until the next
   process open.** `outcome.rs:121-128` documents them as "orphans for the
   open-time sweep"; nothing deletes them while the daemon runs. A cut that
   fails at its 60th segment with `NoSpace` leaves 59 × 16 MiB; the ladder
   frees space, the retry writes another 59 and fails again: a death
   spiral the ledger cannot see (the C2 headroom covers one cut). Likewise
   for every `Io`, `Auth` (finding 4) and `MapFull` (finding 2) retry.
   **Fix:** in the error path of `destage_cut_with_cancel`
   (`pipeline.rs:201-215`) delete the segments this cut sealed
   (`SegmentStore::delete`, best effort, `NotFound` ignored) and report the
   bytes freed in `Partial`; nothing durable names them (no commit, no
   index entry: the same argument the sweep uses for ids above the mark),
   so it is safe. The trait says `delete` is "GC only"; the lead should
   either widen that sentence or make the deletion the coordinator's duty
   and write it into the C-protocol. The crash case stays with the sweep.

4. **major — the error contract with the coordinator is not written
   down.** `DestageError` (`outcome.rs:130-147`) gives a `kind()` and
   nothing else. M11 has to decide, per kind, between retry after the
   ladder (`NoSpace`), retry later (`Io` other than `EIO`), seal then
   retry or shrink (`MapFull`, finding 2), stop (`ReadOnly`, `Closed`),
   and **never retry** (`Auth`, `Corrupt` from a base read: the base
   extent is damaged or forged, the cut cannot be destaged and the journal
   cannot be reclaimed, so the volume stalls until an operator acts;
   `Invalid`: an overlay bug). Today a coordinator that loops on `Auth`
   stalls silently and, with finding 3, fills the disk. **Fix:** a table
   in the `DestageError` rustdoc (retryable / after action / permanent),
   and carry the failing chunk number (`DestageError.chunk: Option<u64>`)
   so the log can name the LBA range; the worker has it at
   `worker.rs:201`. (`err-custom-type`, `err-doc-errors`)

### Minors

5. **minor — determinism of a duplicate's codec under backpressure.**
   `worker.rs:264-273`: the first worker to *reach* an id compresses it
   with the pressure of **its** job, but the extent is stored at the
   position of the first chunk in LBA order with that id. When two equal
   chunks sit in jobs on either side of `lz4_depth` or `raw_depth`, the
   stored codec and `clen` (and so the map entry) depend on which worker
   claimed first. The test `the_layout_does_not_depend_on_the_worker_count`
   runs without pressure (3 jobs), so it cannot see it. Fix: either make
   the claim carry the pressure of the lowest job index seen so far and let
   a lower-pressure latecomer recompress (complex), or narrow the
   determinism claim in `lib.rs` to "equal pressure" and add a test with
   `batch_chunks = 1`, `lz4_depth` inside the duplicate span, asserting
   equal maps across worker counts (which then documents the choice).

6. **minor — the backpressure signal is the cut's size, in jobs.**
   `pipeline.rs:61-70` defines depth as "jobs after this one", so the first
   jobs of any cut longer than `lz4_depth` jobs always degrade whatever the
   pool is doing, and the thresholds (`config.rs:93-94`, 4 096 / 16 384
   jobs) mean 1 GiB / 4 GiB at 16 KiB chunks but 4 GiB / 16 GiB at 64 KiB.
   The plan's "worker queue deeper than a threshold" is a lag signal; the
   only lag the coordinator can see is journal fill and dirty bytes, which
   is also the ladder's "throttle destage" input. Recommend: thresholds in
   bytes (chunk-size independent), and a `pressure: Pressure` field in
   `DestageConfig` (or an argument) the coordinator sets per cut from
   journal fill, which is deterministic for the cut and reflects real
   pressure; keep the index-based rule as the fallback. The report should
   not call the current rule "queue depth".

7. **minor — lean volumes must read as scans (R1).** Rule 13 R1: "Lean
   volumes (`id_len = 0`) read only as scans", that is with `verify_id`.
   `read.rs:93-99` passes an empty prefix (matches every id) with
   `verify_id: false`. Set `verify_id: deps.params.id_len == IdLen::Zero`
   on the base read; the confirmation read already has the full id. One
   line, exact conformance, and the test
   `a_base_read_with_a_zero_length_id_prefix_has_no_fallback` can assert
   the recomputed id is checked (flip a payload byte instead of the tag
   byte in `plain`, where the unkeyed tag is recomputable).

8. **minor — `check_cut` trusts the overlay's `write_seq`s.**
   `pipeline.rs:218-233` checks ascending chunks and (in `check_pieces`)
   piece tiling, but not `piece.write_seq <= cut.cut_seq`, nor
   `write_seq != 0` for data pieces. A buggy overlay would put records
   above `c` into a commit whose `cut_seq` is `c`, and the segment footer's
   `write_seq_hi` would exceed the cut. Both are one comparison in
   `check_pieces` (`assemble.rs:33-69`), returning `Invalid` like the other
   overlay-bug checks.

9. **minor — the `Verified` adopt path skips the hint sanity check.**
   `worker.rs:246` writes `hint.clen` into the map entry as is; the
   `Hinted` path refuses `clen` outside `1..=chunk_size` (`read.rs:33-39`).
   A verified footer should never carry such a hint, but the check is two
   comparisons and keeps the map's invariant local. (`api-parse-dont-validate`)

10. **minor — a worker panic becomes a coordinator panic.**
    `pipeline.rs:73-83`: the `Guard` fails the pool so the packer wakes,
    but `thread::scope` re-raises the panic in the caller. That is the
    right default for a bug, and the plan's M10 rule for queue threads is
    `abort()`; say so in `destage_cut`'s rustdoc (`# Panics`) so M11 decides
    deliberately whether its coordinator thread catches or aborts.
    (`doc-panics-section`)

11. **minor — test gaps.** (a) No test reads the cut's map entries through
    the store between `destage_cut` and `seal_open` (would have caught
    finding 1). (b) No test that `MapFull` leaves `Partial::segments` empty
    (finding 2). (c) `the_layout_does_not_depend_on_the_worker_count` uses
    3 jobs for 40 chunks; with `batch_chunks = 1` it would exercise the
    cross-job claim race it is meant to cover, and a variant under pressure
    pins finding 5. (d) The model test's cut sizes (6 chunks) never roll
    more than two segments or run the window stall (`result_window` is 12
    with 3 workers, jobs at most 6); one case family with `chunks = 64`,
    `batch = 1`, `workers = 4` would exercise `wait_job`/window interplay.
    (e) `an_unreadable_or_absurd_hint_is_a_miss_and_absurd_ones_are_not_read`
    runs on one codec flavour only; the `clen` guard is codec-independent,
    so that is fine, but say so in a comment.

### Nits

12. `pipeline.rs:203-205`: `EIO` only; `EROFS` (30) from a file system
    that went read-only is the same event for the volume. Consider both.
13. `compress.rs:65` and `:151` zero-fill the output buffer to the bound
    on every call (`resize(_, 0)`: a 64-128 KiB memset per chunk). Small
    next to zstd, but avoidable with a buffer kept at the bound once
    (`resize` only when shorter). (`mem-reuse-collections`)
14. `worker.rs:290`: one `Vec` per sealed extent plus an `Arc<SealSlot>`
    (`Mutex` + `Condvar`) per unique id. Fine for now; a slab of slots
    indexed by claim order would cut the allocations if profiling ever
    shows them.
15. `lib.rs` "Rule R1" section: add the one-line consequence for `plain`
    volumes (a bucket writer can satisfy the D17 confirmation with a forged
    tag-valid extent; D16 accepts this) so nobody reads the confirmation
    as a security control in that mode.
16. `stats.rs:13`: `codec_slot` folds any unknown codec into slot 3; the
    `Codec` enum makes that unreachable, a `debug_assert!` would say so.
17. `outcome.rs:63-64` (`seal_open`): with finding 1 fixed this method goes
    away; if it stays, note that it cannot be called twice *successfully*
    ("idempotent" today means the second call is a no-op `Ok(None)`).
18. `pack.rs:106-107`: `writes` covers only stored chunks' records, which
    matches on-disk-formats item 27 ("records that contributed data"); the
    report's spec-gap note should cite that item rather than call it open.
19. Report line "a cut that needs more dirty leaves than the cap is not
    split" is the whole of finding 2 and deserves its own sentence in the
    plan's M11 step 5, not only the report.

## Recommendations on the implementer's decisions (report numbering)

1. **Borrow the cut: confirm.** `release(&cut)` needs it; the plan's
   by-value signature was shorthand.
2. **Last segment left open for C4: reject** (finding 1). Seal inside
   `destage_cut` before the map commit, or return the transaction
   uncommitted. Full segments sealed as they roll: confirm.
3. **Job list of the whole cut, memory bounded by `result_window`:
   confirm.** The `results` vector is `O(jobs)` of `Option`, the sealed
   extents in flight are at most `result_window` jobs of at most 1 MiB
   input each, and the window cannot deadlock while
   `result_window >= workers` (a blocked worker holds no claim; a claimer
   fills its slot before it can block). Rename "depth" per finding 6 and
   validate `result_window >= workers` in `DestageConfig::validate`.
4. **`MapFull` fails the whole cut: confirm the all-or-nothing**, with the
   pre-store probe and the "oversized cut is not retryable" sentence
   (finding 2) and orphan deletion (finding 3). Splitting stays deferred.
5. **`Closed` for cancellation, the cause otherwise: confirm.** The
   `cause`/`Closed` disambiguation in `pipeline.rs:280-291` is right;
   `a_host_eio_makes_the_volume_read_only_and_names_the_cause` covers it.
6. **Stale-hint fallback in the base read: confirm.** DESIGN §7 and the
   plan's M11 step 4 require it on every read; GC's compaction relies on
   it. Apply finding 7 for lean volumes.
7. **`SpaceGate` used for `is_read_only` and `fatal_io` only: confirm.**
   The M1.B1 walk-through puts C2 headroom and the C4 true-up on the
   coordinator; `sealed_bytes()` gives it the actual.
8. **`cold_rmw` counts base reads from the store only: confirm.** A
   partial write over a hole reads nothing and should not be called cold.
9. **Requests:** add `cargo clippy/test -p lunix-destage --features
   crashpoints` to `ci.sh` (same precedent as store and journal); accept
   the `deps:` commit.

## Perf (focus 7): environment, not code

The stage breakdown (per core-second of input: assemble 2.3-3.2, hash 10,
compress 0.38, seal 10.5-13, append 13-18 GB/s) says compress is 80 % of the
CPU. zstd-1 on the synthetic word-salad corpus at 0.38 GB/s/core sits at the
bottom of DESIGN §5's own table ("zstd-1 0.4-2 GB/s, data-dependent: the
bottleneck") and the research note's numbers for the same kind of corpus.
The §20 target (3 GB/s with `auto` on 8 workers) therefore needs eight
*physical* cores of compress at 0.4 GB/s; four pinned CPUs on a host at load
200+ bound the run at about 1.5 GB/s, and 1.0-1.36 was measured. Nothing in
the code is in the way: the zstd context is per worker and reused
(`compress.rs:146`), the probe is 4 KiB of LZ4, raw chunks skip both, the
packer is off the critical path at 13+ GB/s, and the only per-chunk waste
(nits 13, 14) is a memset and two allocations, well under 5 %. The gate
item should be re-run on a quiet host with at least 8 cores before "not met"
goes into the plan's record; the floor of 1.0 GB/s per run holds either way.

## rust-skills notes

`#![forbid(unsafe_code)]`; no `unwrap`/`expect`/indexing on disk or cut
data in library code (`err-no-unwrap-prod`, rule 7); `conc-scoped-threads`
for the pool; `mem-reuse-collections` per worker; `num-cast-try-from` on
every narrowing (`u32::try_from(compressed.len())`, `usize::try_from`
offsets); `err-doc-errors` on the public functions; `test-descriptive-names`
throughout. `conc-atomic-ordering`: `Release`/`Acquire` on `failed` is right,
`Relaxed` on `next` is right. `api-non-exhaustive` is not used on the public
structs, consistent with the rest of the workspace. `perf-profile-first`:
the per-stage counters are the profile; keep them.

## What I verified by running

All on the remote host, CPUs 0-3, `-j 4`, no root, no ublk, in a fresh
directory for the branch and a separate scratch copy for the changed trees;
both deleted afterwards.

- The branch head: `cargo test -p lunix-destage` (39), the same with
  `--features crashpoints` (40), `cargo clippy --all-targets -D warnings`
  with and without the feature, `cargo fmt --check`, and the three ignored
  perf tests in release. Counts and numbers are in the gate table and agree
  with the report; the throughput numbers reproduce the report's within
  noise on a host at load 230-270.
- The report's uncommitted mutation checks, re-done: skipping the
  confirmation, dropping the base read's prefix expectation, and switching
  off the in-cut dedup each fail the tests the report names (4, 2 and 7
  tests). The third fails loudly (`Invalid`) rather than with wrong data,
  which is the right failure mode for a packer invariant.
- Finding 1, store side: a test added to the scratch copy destages a
  partial overwrite without `seal_open` and reads the new map entry back
  through `SegmentStore::read_extent`: `NotFound`. The engine side (the
  read path consulting the map for un-overlaid blocks) is the plan's M11
  step 4 text, not code I could run.
- Finding 2: by reading `lunix-map/src/txn.rs` (`touch`, `commit`): the
  cap counts the transaction's own new leaves, so an oversized transaction
  fails `MapFull` on an empty overlay. Not run.
- Not run: the model with more cases than the default 500 per size, the
  `tools/ci.sh` whole-workspace gate (the report's run is recent and the
  branch touches one crate), anything with root.

---

## Re-review (branch `p1/m9-destage` at 2f81877, rebased onto main 2f211d1 with M11 merged)

Targeted at the fix commit 2d8b5cf (source diff against the reviewed code
commit, the new tests, the report's "Review fixes" section) and the seam to
`crates/lunix-engine/src/destage.rs`. Run on the remote host (4 pinned CPUs,
no root) in a fresh directory, mutations in a scratch copy; both deleted.

### Verdict: **merge**

The blocker and the four majors are fixed as recommended, and each fix has
a test that a mutation of the fix fails. Nothing new blocks; the new
findings are minors and nits for M12's adapter or a follow-up.

### The original findings

| finding | status | how checked |
|---|---|---|
| 1 blocker, map committed over an `.open` segment | **fixed.** `run` has one path to `txn.commit()` and `packer.seal_current()` precedes it (`pipeline.rs`, "The last segment is sealed before the map commit"); full segments are sealed in `append`. `DestageOutcome::open`/`seal_open` are gone and `DestagedSegment::sealed` is not an `Option`, so the type cannot express an unsealed segment. A seal failure returns before the commit; an error path drops the packer's open writer (abandoned) and never reaches the map. No read can reach an unsealed segment through the map. | read; mutation A (final seal removed): 33 tests fail, among them `every_map_entry_is_readable_the_moment_the_cut_returns` |
| 2 major, oversized cut | **fixed.** `probe_map` runs after `check_cut` and before the pool starts: `begin_update`, `set_hole` per cut chunk, drop. Both maps clear `txn_open` on drop (`lunix-map/src/txn.rs:157`, testkit `map.rs:414`), and `lunix-map`'s `touch` is the same cap arithmetic `commit` uses, so a probe that passes cannot be followed by a commit-time `MapFull` while the coordinator is the only writer. `CutTooLarge` = `MapFull` with `dirty_leaf_count() == 0`, `AfterMapSeal` otherwise; the rustdoc says not to retry the former. | read; mutation B (probe removed): `map_full_is_found_before_anything_is_stored` fails |
| 3 major, orphans of a failed cut | **fixed.** The error arm deletes exactly `packer.finish()`: segments this call opened and sealed. Nothing durable can name them: the index is never written by this crate, the map transaction either was not started or returned `Err` (atomic by the trait), and no step after a successful `commit` can fail, so the delete loop is unreachable once the map holds the entries. Adopted chunks point at older segments, which are not in the list. A crash at any point of the loop leaves sealed files no commit names, which the sweep or GC removes; the three crash points (`after_assemble`, `after_segment_append`, `after_segment_seal`) cover the windows that leave files. | read; mutation C (delete skipped): two tests fail |
| 4 major, error contract | **fixed.** `ErrorClass` with the table in rustdoc, `chunk` carried from worker and packer, EIO and EROFS both `Stop` and both `fatal_io`. | read; tests assert class and chunk |
| 5, 6 determinism and pressure | **fixed.** One `Pressure` per cut (`config.pressure` max the byte thresholds on `chunks x chunk_size`); the claimer's job no longer matters. | read; new test with `batch_chunks = 1`, 1-8 workers |
| 7 lean scan, 8 `write_seq`, 9 verified `clen`, 10 `# Panics`, 11 tests, nits 12, 15, 16, 18 | fixed. Nits 13, 14 and (e) deferred, acceptable. | read |

### Seam to M11 (`DestageFailure { error, retry, chunk }`, `Retry`)

Lossless. `Retry::classify` in the engine and `outcome::classify` here are
the same function line for line (EIO/EROFS first, then `NoSpace`, the two
`MapFull`s by dirty leaves, `Io`/`Busy`, `ReadOnly`/`Closed`, the rest), and
the six variants pair one to one (`Transient`=`Later`, `Permanent`=`Never`,
the others by name). `error` and `chunk` copy across. The outcome maps
without loss too: `segments` -> `sealed`, `entries` flattened ->
`index_inserts`, the counters through `DestageStats::add_to_engine`'s order
(hash, dedup, compress, seal, append), `extent_auth_failures` added (M9
does its own reads, so nothing is double counted). `DestageCtx::reader` and
`::writes` go unused by the adapter (M9 reads through `segments` + `codec`
and computes per-segment ranges), which is fine. `cargo test -p
lunix-engine` passes on the rebased tree (46 tests).

### New findings

R1. **minor (M12 adapter) — classify once, not twice.** The adapter must
    copy `DestageError::class` into `Retry` with a `match`, never call
    `Retry::classify` on `error` again: M9 samples `dirty_leaf_count()`
    *after* the failure and `SimpleDestager` *before* the call. They agree
    today (a failed cut dirties nothing), but two copies of the table will
    drift. Better: an `impl From<ErrorClass> for Retry` in the adapter with
    an exhaustive match, and a test over every `ErrorKind`.

R2. **minor — a failed delete is silent.** `pipeline.rs` error arm:
    `if let Ok(n) = deps.segments.delete(..)` drops the error, and
    `Partial::segments` lists every segment "deleted or tried to" without
    saying which. The engine's stand-in logs a warning there. The file
    then sits on disk outside the ledger (the coordinator's `abandon`
    releases the whole `seg_bound`) until GC. Return the undeleted ids
    (`Partial::undeleted: Vec<SegId>`) or count them, so the adapter can
    log and keep the charge. (`anti-empty-catch`)

R3. **minor — `freed_bytes` has no consumer and its rustdoc misleads.**
    "give them back to the ledger": nothing was charged for these bytes
    beyond the C2 bound, which the coordinator releases whole on failure
    (`coord.rs` `abandon`), and `DestageFailure` has no field for it.
    Reword to "informational" or drop it; do not have the adapter release
    it a second time.

R4. **minor — `AfterMapSeal` means different things on the two sides.**
    M9's table says "`BlockMap::seal`, then retry the same cut"; the
    engine maps it to `CutFail::Smaller` (cut at a lower `write_seq`). If
    the map is already at its cap, `begin_update` fails whatever the cut
    size, so "smaller" cannot converge. Harmless today because every
    commit's C5 seals the map and a commit that fails after C3 stops the
    engine, so a cut never starts with dirty leaves; say that in the
    adapter, or make the coordinator seal on this class. One sentence in
    either rustdoc.

R5. **nit — the lean-volume test does not pin `verify_id`.**
    `a_lean_volume_reads_its_base_as_a_scan` asserts only `cold_rmw == 1`,
    which also holds with `verify_id: false`. A counting codec wrapper (or
    a `plain` extent with a payload byte flipped and its unkeyed tag
    recomputed) would fail the mutation.

R6. **nit — `dedup_rejected` now also counts a `Verified` hit with an
    absurd `clen`**; its doc in `stats.rs` still says "persisted-run hits
    whose confirmation failed".

R7. **nit — `pressure` has no carrier in `DestageCtx`.** The adapter has
    `ctx.journal` and can derive it from `Journal::fill()` per call, as the
    engine's comment says; note in M12's plan that the thresholds
    (fill -> `Lz4`/`Raw`) are still undecided, and that with
    `Pressure::None` only cuts above 4 GiB degrade.

R8. **nit — the store trait still says `delete` is "GC only"** (the report
    flags it); the lead should widen the sentence in `lunix-iface` since
    both M9 and the engine's stand-in now call it for their own unnamed
    segments.

### What I ran

- `cargo test -p lunix-destage --features crashpoints`: **46 passed** (unit
  1, behaviour 26, compression 7, confirm 8, crypto 2, model 2; perf 3
  ignored, not re-run). `cargo clippy -p lunix-destage --all-targets -D
  warnings` with and without the feature, `cargo fmt --all --check`: clean.
- Mutation A (no final seal before the commit): 15 behaviour, 7
  compression, 8 confirm, 1 crypto, 2 model tests fail. Mutation B (no
  probe): `map_full_is_found_before_anything_is_stored` fails. Mutation C
  (no deletion): `a_commit_time_map_full_deletes_what_the_cut_stored` and
  `a_failed_chunk_fails_the_cut_applies_nothing_and_deletes_its_segments`
  fail.
- `cargo test -p lunix-engine` on the rebased tree: 46 passed.
- Not run: `tools/ci.sh` for the whole workspace (the report's run after
  the rebase is green), the perf tests, anything with root.
