# Review 2 of plan 01 (phase 1, local-only volume): readiness for M1.A0

Reviewer: independent, 2026-11-12. Inputs: `wip/01-p1-review.md` (review
1), the revised plan `wip/01-p1-local-volume.md`, DESIGN.md, DECISIONS.md
(ADRs 1-30 with the "Amended 2026-11-12 (phase 1 review)" blocks),
GLOSSARY.md, ROADMAP.md, `bench/BASELINE.md` with the ublk-recovery,
journal-bench, radixmap and crypto-kat RESULTS, `kat-vectors.json`,
replicix-core `chunkfmt` (`format.rs`, `codec.rs`).

## Verdict: **ready with fixes**

M1.A0 may start once finding 1 (a one-line edit of D11, which A0 copies
verbatim) is made. Findings 2-9 are text fixes the lead lands before the
milestone they name; none needs a new ADR, and only one needs the
maintainer (N12, a confirmation). Review 1's 35 findings all landed; the
residues are findings 7, 10 and 14 below.

## Findings

Severity: **B** blocker (before A0 starts), **M** major (before the named
milestone starts), **m** minor.

### A. Spec ↔ plan consistency (check 1)

1. **B — Plain-tag coverage: plan D11 and M2 step 2 say `CCH1 header ‖
   chunk_id ‖ payload`; DESIGN §6, ADR-6, ADR-11 and GLOSSARY say "every
   byte of the extent before the tag", which includes `clen` (a separate
   field after `chunk_id`, outside the 16-byte `CCH1` header).** Pick the
   spec's: it is one contiguous slice (`&extent[..len-16]`), it also
   covers `clen`, and the encrypted layout's AEAD likewise covers its
   framing. Fix D11, M2 step 2 and freeze item 3 ("plain tag input = every
   byte before the tag") before A0 copies the table. N12 is then closed by
   the maintainer's sign-off.
2. **M — Three spec places still say the phase 1 floor is genesis and
   nothing is deleted:** ADR-9 amendment ("`chain_floor` is the genesis
   `commit_seq` until GC or retention moves it, and no commit object is
   deleted"), GLOSSARY "Chain floor" ("In phase 1 it is the genesis
   commit"), DESIGN §4.4 step 2 ("`fsck` verifies the whole chain from
   genesis"). They contradict D5/D15, DESIGN §8.2, §8.7, §11 and ROADMAP.
   Fix (spec edit, lead): ADR-9 "floor = the older of the last `N` FULLs
   (default 2); commits below it are deleted after the GC grace";
   GLOSSARY likewise; §4.4 step 2 "`fsck` verifies the chain from the tip
   down to `chain_floor`". Also DESIGN §6.1: "`gc/` contents are versioned
   by phase 4 and 7" → "the evidence body (ver 1) is defined in phase 1 by
   `docs/reference/gc.md`; `gc/space/` by phase 4".
3. **m — Field naming.** The plan's commit body says `seq = parent_seq + 1`
   and types `Seq`/`CommitSeq`; the spec's terminology block fixes
   `commit_seq` and `write_seq`. A0 names the fields `commit_seq`,
   `parent_seq`, `cut_seq`; `lunix-format` types `WriteSeq` and
   `CommitSeq` (not `Seq`). The journal record field is `write_seq`.
4. **m — ADR-4 amendment has a leftover sentence** ("in one record padded
   to a 4 KiB multiple") that contradicts the batch-packing rule two
   sentences later. Delete it.

Everything else in the freeze list agrees: batch packing (D12 = §4.1 =
ADR-4), genesis (D5 = §8.2 = GLOSSARY), `last:N` default 2 (D5/D15 = §8.2,
§8.7, §11, ROADMAP), index-run lag (D3 = §7 = §8.2 = ADR-9 = ADR-26),
envelope-only `heat/`/`gc/`/`dicts/` (D6 = §6.1 = ADR-11 = ADR-30), codec 3
(`CODEC_ZSTD_DICT = 3` exists in chunkfmt), ADR-28 `refs[].kind`, ADR-25
`lineage[]`/`lineage_idx`/`holds/`/`readers/`, ADR-11 fields. The AAD and
`info` builders match the KAT vectors by width (61 and 100 bytes) and the
`CCH1` header is 16 bytes with a 2-byte codec and 8-byte `ulen`, as D2
says.

### B. GC design for phase 1 (check 2)

5. **M — In-flight segments can be condemned.** Compaction seals `S_new`,
   calls `relocate` at once, and returns `moved` records that the engine
   applies "at the next cut"; `S_new` is named by that cut's FULL. Until
   then `S_new` has zero live bytes by every rule in M17 (no map entry, no
   retained commit names it), so a mark that runs before the next cut
   condemns it; an **idle volume cuts nothing** (M11 step 5), so the grace
   elapses, the recheck still finds nothing, and `S_new` is deleted while
   `relocate` already points the index at it: a later cut applies `rehint`
   to a gone segment, `drop_segment(S_new)` removes the only index entry,
   and the read of `X` fails although `S_old` still holds it. Fix (M17
   rules 4, 5, 7 and M11 step 5): (a) GC never condemns a segment or pack
   whose id is above the tip's high-water mark (the highest id any
   retained commit names): inside a running process such files are the
   open segment, sealed-but-uncommitted output or compaction output, never
   garbage; after a crash the open-time sweep deletes them; (b) pending
   `moved` records are a cut trigger (the idle rule does not apply);
   (c) `relocate` entries follow M6's persist rule (never persisted until
   the segment is named by a durable commit), stated explicitly.
6. **M — Mark staleness across cuts is not argued.** Mark, condemn and
   recheck are separate budgeted steps with cuts between them, so the mark
   a condemn uses may predate a FULL that gained a reference (a dedup hit
   on a not-yet-condemned segment). The design is safe, but the plan must
   record why in `docs/reference/gc.md` and test it: (i) a pass pins its
   root set `R` (commit seqs) when mark starts and walks exactly `R`;
   (ii) condemn excludes any object named by a **currently** retained
   commit's `new_segments[]`, `map_packs[]` or `index_packs[]` (rule 6,
   made operational); (iii) after condemn no new reference to the object
   can arise (dedup excludes condemned, compaction output is new, `rehint`
   only targets compaction output), so a recheck that walks a root set
   pinned at **its** start finds every reference: any root created later
   derives from a pinned root and can add no reference to a condemned
   object; (iv) if the root set changes during a recheck walk, the walk is
   still sufficient for the same reason, and the plan says so. M17.A's
   "dedup race" gate gains the case "cut with a dedup hit between mark and
   condemn; the recheck un-condemns".
7. **M — `gc/` evidence is "appended per line" but every `gc/` object is a
   sealed create-only object (§6.1, ADR-11).** A sealed object cannot be
   appended to, and the object name under `gc/` is not fixed anywhere, yet
   it is freeze item 15 and in A0's paths module. Fix (A0 + M17 rule 11 +
   M7 step 7): one sealed object per GC pass, `gc/<pass:016x>`, body
   `{ver: 1, bytes}` with the pass's JSON lines, written once at pass end
   (and once by the urgent pass); the only mutable GC state is the local
   `state/gc/condemned` (atomic replace). `gc/space/<commit_seq>` stays
   reserved. N9's "uploads verbatim in phase 2" then holds by construction.
8. **m — "A damaged tip has a fallback" (rule 1, §11) is undefined.**
   Falling back to `tip − 1` at mount would reuse the create-only name
   `commits/<tip>` and lose journal records already reclaimed. Fix (M11
   step 6, M14): mount never falls back; a tip that does not authenticate
   is a typed error; `lunix fsck --repair --truncate-tip` (M14) moves the
   damaged object aside and makes `tip − 1` the tip, offline. `last:2`
   buys exactly that repair, nothing automatic.
9. **m — Delete-time index invariant (N7).** A segment with stale hints
   pointing into it may be deleted only if the index durably names the
   current copy of every live id those hints resolve to, or that copy's
   segment is named by the chain (so `catch_up` restores the entry after a
   crash). This holds by construction (finding 5c, D3), but M17 rule 5
   should state it and M14 check it (every retained-root entry resolves by
   hint or index: already in M14 step 3; add "after `index/` is deleted and
   rebuilt").

Crash mid-compaction (`S_new` sealed, `relocate` in memory, crash): the
open-time sweep deletes `S_new`, the memtable is lost, `S_old` is still
named and indexed: safe. Crash mid-delete: unlink is idempotent, the list
is a hint, recheck skips a missing file: safe. Journal floor: unaffected
(GC never touches the journal; reclaim stays behind C6). fsck: M14 +
M17.B cover condemned-but-present, condemned-and-gone, needed-and-gone.
Order with commits (rule 6) is right.

### C. Parallel executability (check 3)

10. **M — Ownership overlaps that break `check-ownership.sh` in wave 2:**
    M17.B owns `crates/lunix-fsck/src/gc.rs` but must add `mod gc;` and a
    call site in M14's `lib.rs`; M12b owns `src/testsupport.rs` and needs a
    `mod` line plus a `testsupport` feature in M12a's `Cargo.toml`; M13b
    owns `src/{fsck,key}.rs` but M13a's stubs are wired in `main.rs`;
    M17.A owns `crates/lunix-gc` (which contains `tests/`) and M17.B owns
    `crates/lunix-gc/tests/`. Fix: M14, M12a and M13a create the files
    with the final signatures (`pub fn check_gc(...)`, `pub mod
    testsupport` behind the feature, `pub fn run_fsck(...)`/`run_key(...)`
    returning "built in M13b") and the later item edits only those files;
    `ownership.toml` lists `crates/lunix-gc/tests/**` under `m17b` only.
11. **M — M17.A cannot test pack liveness against `FlatMap`.** `FlatMap`
    (B2a) has no packs, but M17.A's `Collector` needs `walk_nodes` with
    real pack ids and `rehint`. Fix: M17.A depends on `lunix-map` (M5, wave
    1b, routine) as a dev-dependency and uses `FlatMap` only as the content
    oracle; or B2a's `FlatMap` models packs (one pack id per `seal`). The
    first is cheaper and the graph already has M5 before M17.A.
12. **m — M7 is in wave 1a but its gate needs a codec fake** ("plain and
    encrypted through a fake `ExtentCodec`"); `PlainCodec` is B2a. Either
    move M7 to wave 1b or let it carry a 30-line in-crate test codec.

Critical path: A0 → A5 → B1 → B2a → B2b → M11 → M12a/b → M17.B → M15.
After B1/B2a/B2b, no wave 1 milestone needs another's crate; the frozen
crates change only through the lead, and `GcView`/`Collector` (B1 item
10) is the one interface designed before its first user (M17.A); its
reviewer should read M17's rules 1-9 as a checklist.

### D. Safety (check 4)

13. **M — The root wrapper's outer `timeout -k` contradicts W4.** A
    `SIGKILL` from `timeout -k` reaching a daemon blocked in `DEL_DEV`
    with a mounted file system is exactly what W4 forbids ("no `SIGKILL`
    to a daemon that is stopping; wait 30 s; only `umount` afterwards").
    Fix (M1.A `tools/remote.sh root`, W8): the outer timeout signals the
    **script** (`TERM`), whose `trap` runs stop-fio → `umount` → detach
    with the W4 watchdogs; the `-k` grace is ≥ `DEL_DEV` watchdog (60 s)
    + 60 s; the wrapper never signals the daemon process itself; a script
    that is still alive at `-k` is reported as a wedge (W9), and the
    wrapper writes the leftovers file so the next run refuses to start.
14. **m — Residues a cheaper model can get wrong:** M10's grow test (16 →
    64 GiB on a ≤ 40 GiB budget) is fine only because the backing file is
    sparse and fio is bounded: say "`--io_size` ≤ 8 GiB" in the gate.
    `LUNIX_MIN_FREE_GIB` 120 against 336 GB free leaves ≈ 200 GB, equal to
    the cap: the lead's brief should state the cap once so the two
    numbers move together. M12's device smoke runs under the `root`
    wrapper too (the plan says "root, serialised" but not "through
    `tools/remote.sh root`"); add it.

W1-W9, the identity check before recovery, `abort()` on queue-thread
errors, the no-`STOP_DEV` unit test, the one-root-run lock, the leftovers
check and the per-cell `df-guard` are present and match the P0.3/P0.12
lessons. Budgets add up (60 + 32 + 100 GiB ≤ 200 GB).

### E. M18a / M18b (check 5)

15. **m — `chunkfmt::Codec` is not `#[non_exhaustive]`.** Adding a variant
    is source-breaking for exhaustive matches; `Lz4` is `#[cfg(feature =
    "lz4")]`, so M18a must add `ZstdDict` under `#[cfg(feature =
    "zstd-dict")]` the same way, and the Constellation `cargo check` must
    run with the feature **on** as well as off. The announce-before-merge
    rule (ADR-19, plan rule 8) is honoured.
16. **m — M18b stage 3 depends on M14** (the validation uses `lunix-fsck`'s
    walk), not only M12a/M13a. Installing Docker or containerd on the
    shared host is a system change: prefer `skopeo`/`umoci` or rootless
    `podman --root` under the scratch directory; the lead approves either
    way.

No plan or spec text assumes format-aware recompression of media. The
only mention of "already-compressed media" (M18b workloads) is an
incompressible control for the LZ4 probe's raw path, which is correct.
ADR-12's "recompress on seal" and ROADMAP's "cold-segment recompression"
mean re-running a generic codec, not transcoding. M18a is consistent with
ADR-30 and ADR-19; the pruned matrix and CPU budget are credible.

## Resolutions for open gaps N6-N13

| # | resolution | who |
|---|---|---|
| N6 | Adopt the defaults, with one fix: the grace floor "10 s + 2 × read helper timeout" exceeds the 60 s default if the read timeout is 30 s. Define `read_timeout` (default 10 s) in `volume.json`; floor = 30 s; `gc_grace` default 60 s; `gc_rate` 64 MiB/s; live threshold 0.5; step budget 50 ms; `retain_last` 2. Revisit after M17.B. | lead |
| N7 | Accept chunk-id liveness with index fallback and `id_len = 0` ⇒ hint-only liveness, no compaction. Add the invariant of finding 9 to `gc.md` and M14. | lead |
| N8 | Accept whole-pack deletion. Fixed trigger: if M17.B measures the dead share of surviving packs above 50 % of pack bytes, open a follow-up (re-emit live nodes of packs under 25 % live at the next seal); otherwise defer to phase 4. | lead |
| N9 | Yes, if `gc/` evidence is one sealed object per pass under its bucket-relative name (finding 7); the condemned list is local-only and never uploaded. Fix the §6.1 wording (finding 2). | lead |
| N10 | Informational; M17.B reports write amplification. No action. | — |
| N11 | Resolved; delete the stale ADR-4 sentence (finding 4). | lead |
| N12 | Tag covers every byte before the tag (finding 1). Confirm at A0 sign-off. | maintainer |
| N13 | Open until the file-type study finishes; M18b records the commit of `spikes/dict-filetypes/` it used and lists any convention drift in the note. | lead |

## Maintainer decisions before M1.A0

One: **N12** — confirm that the plain-extent tag covers every byte of the
extent before the tag (header, `chunk_id`, `clen`, payload). Recommended:
yes. Everything else here is lead or A0 scope and is settled by the A0
sign-off itself (finding 7's `gc/<pass:016x>` name is part of the paths
module the maintainer signs).

## Order of fixes

Before A0 starts: 1, 3 (table wording). Before A0 sign-off: 2, 4, 7.
Before M1.A: 13. Before B1: 10 (stub ownership in `ownership.toml`), 11
(M17.A's dependency), 12. Before M11/M17.A: 5, 6, 8, 9. Before M18a/M18b:
15, 16. Finding 14 at M10/M12.
