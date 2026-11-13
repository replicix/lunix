# Review of M11 — `lunix-engine` (branch `p1/m11-engine` at 987fda3, base c83d632)

Independent review of the write path, flush groups, the commit protocol C1-C10,
startup recovery, the R1 reader and the destage seam, against the plan's M11
section and gate, D1-D20, cross-cutting rule 13 (R1-R4), the journal contract
(`lunix_iface::journal`, `overlay`, `space`, `map`, `index`, `store`, `gc`),
DESIGN §4.1-§4.4, §8.1-§8.2, ADR-3, ADR-4, ADR-9, ADR-18, ADR-28, ADR-31, the
M12 journal-damage note and the M9 review (`01-p1-m9-review.md`) with M9's
revised contract (`p1/m9-destage` at 49d74aa). The engine's test suite was
re-run on the remote host in a fresh directory; eight mutations ran in a
scratch copy; both were deleted afterwards. `rust-skills` ids are cited where
they apply; plan and spec win.

## Verdict: **merge after fixes** (one blocker, small; four majors)

The protocol is right where it matters. Every commit is a cut: `write_seq`
registration and `try_reserve` share one lock, a number leaves the in-flight
set only after its overlay insert, and the coordinator reads `last_issued()`
before scanning the sets, so C1 cannot miss a write (`queue.rs:335-347`,
`:607-613`, `engine.rs:150-171`). A cut lowered to `c' <= c` after the wait
is still a prefix cut (ADR-28), the commit names only objects that are
durable (segments sealed inside C3, the pack at C5, index runs from the
manifest), the floor is raised only after `put` returned, index inserts
happen only after C6, and the journal is reclaimed only after C6 with a tail
computed after it. Recovery verifies to `chain_floor`, runs R4 before any
write, sweeps before `catch_up`, feeds the allocator all three D17 inputs,
logs journal damage before any lane opens and honours `refuse`. The R1 reader
is the only `read_extent` caller and has no expectation-free path; a lean
volume reads as a scan. I could not construct a crash point, reclaim,
reopen or MapFull retry that loses a FLUSH- or FUA-acknowledged write, and
the model test's FUA check does fire when FUA is ignored (mutation table).

What must change: a failed **journal read** falls back to the map even for a
record that was never destaged, so a disk fault on the journal turns into
silently stale data instead of `EIO` (blocker 1, a few lines). The majors are
a recoverable `MapFull`/`Io` from the compaction re-hints that is treated as
fatal because the destage transaction already committed (2), destage errors
that are retried forever without classification (3; M9's `ErrorClass` exists
and the seam drops it), a grow applied to the in-memory map before a commit
that may still fail (4), and a test gap: the model never has a write in
flight at a cut, so C1 and the overlay/map hand-off are covered only by one
nondeterministic test (5).

## What I ran (remote host, 4 pinned CPUs, `-j 4`, no root, no ublk)

| run | result |
|---|---|
| `cargo test -p lunix-engine` at 987fda3 (fresh directory) | **38 passed, 0 failed**: unit 1, `api` 3, `basic` 18, `model` 2 (100 x 2000 per chunk size, 91 s), `queue` 8, `r1` 6. Matches the report. |
| mutation `reclaim-before-c6`: `tail_after(c)` + `reclaim` issued right after C3 | **caught**: 3 `basic` tests fail on `violations()` (`GJournal::reclaim` check) |
| mutation `floor-before-c6`: `floor.raise` before `commits.put` | **caught**: 4 `basic` tests fail (`MemFloor` "above the durable tip"; the crash-between-C6-and-floor test sees the floor already raised) |
| mutation `unsealed-pack`: `seal_map` names the pack without sealing it | **caught**: 9 `basic` tests fail (`GStore::put` check and reopen) |
| mutation `index-before-c6`: index inserts moved to right after C3 | **not caught** (18 pass), and benign by construction in this engine: a crash loses the memtable with the process and a non-crash failure after C3 is fatal (no further cut dedups). Keep the discipline; no test can see the mutation. |
| mutation `flush-immediate`: FLUSH completes in `on_request` | **caught**: 6 `basic` tests (`fua_and_flush_survive...`, reopen tests) |
| mutation `fua-ignored`: a FUA write completes like a plain write | `basic` passes; **the model catches it** in both chunk sizes within one case: "after a crash the device is no prefix >= 38 of 38 logged writes" (the FUA `min_n` check works) |
| mutation `no-c1-wait`: `wait_inflight(c)` removed | **model passes** (100 x 2000, both sizes); only `queue::the_background_coordinator_commits_under_load...` fails ("read at 253952" differs). See major 5. |
| mutation `jread-fallback-stale`: every journal read completes with an error status | 4 `basic` tests fail with **wrong data under a success status** ("overlay read" `assert_eq!` after `st.is_ok()` passed): the engine served the map's older version. This is blocker 1 observed. |

Not run: the gate-size model (500 x 2000; the report's run is recent and the
100-case run reproduces), `tools/ci.sh` for the whole workspace, anything with
root, the encrypted-codec R1 cases (no codec in the testkit) and the lease
case (no lease).

## Findings

Severity: **blocker** (fix before merge), **major**, **minor**, nit. Line
numbers are from 987fda3.

### Blockers

1. **blocker — a failed journal read falls back to the map for a record that
   was never destaged, returning stale data with a success status.**
   `queue.rs:676-678` (`on_jread`): on any error status the part is re-issued
   through `map_parts`, with the comment "the record was reclaimed after the
   lookup: its data is in the map now". That is only true when the piece's
   `write_seq <= tip.cut_seq`. The same fallback runs when `submit_read`
   itself fails (`queue.rs:487`). Scenario: the guest writes block `b` (seq
   `s`, acknowledged, not yet cut) and reads it back; the overlay yields
   `Journal(loc)`; the lane's read of `loc` fails with `EIO` (a bad sector,
   an `O_DIRECT` read error; the M3 journal returns an error status for a
   reclaimed location *and* for a failed read, the engine cannot tell them
   apart); `map_parts` reads the chunk's previous version (or zeros for a
   hole) and completes the request with `ok`. The guest's acknowledged write
   has silently disappeared from a read while its journal record is intact
   and will be destaged at the next cut; a file system checksum, if any,
   catches it, otherwise nothing does. The mutation `jread-fallback-stale`
   shows exactly this: wrong bytes, `is_ok()`. **Fix:** keep the piece's
   `write_seq` in `JRead` (`queue.rs:142`) and the durable tip's cut in an
   `AtomicU64` on `Inner` written at C6; in `on_jread`, fall back to the map
   only when `piece.write_seq <= durable_cut`, else fail the read with the
   journal's errno (`EIO`). Treat `submit_read`'s `Err` the same way. Add a
   test with a `GJournal` fault that fails one `submit_read` for an undestaged
   record and expects `EIO`, and one for a destaged-then-reclaimed record
   that expects the data. (`anti-empty-catch`: the error is swallowed.)

### Majors

2. **major — a recoverable error in `apply_moved` is fatal for the process.**
   `coord.rs:283-289`: the compaction re-hints run in their own transaction
   *after* the destage transaction committed, so the handler for any error
   (`walk_leaves` `Io`, `begin_update` `Busy`, `rehint`/`commit` `MapFull`
   for a leaf the cut did not dirty, which the trait allows: "may be for a
   new one") is `fatal("c3 rehint")`: no more commits, writes `EIO` until a
   reopen. Scenario: a cut of `max_chunks_per_cut` chunks dirties
   `dirty_cap / 2` leaves; a compaction pass moved extents of a segment
   whose 1 024 chunks are spread over more than `dirty_cap / 2` leaves
   (random-write history); `MapFull` from `rehint`; the volume stops
   committing and the journal fills. The MapFull retry at `coord.rs:203`
   does not apply (`is_failed()` is already set). **Fix:** apply the re-hints
   *before* the destager's transaction (a dropped transaction applies
   nothing; `rehint` is conditional on the entry at commit time, so the
   order does not change the result) and let the MapFull retry cover it, or
   on failure keep `co.moved` for the next cut and continue with the commit
   (the destaged map is consistent without the re-hints). With the first
   option, a cut that fails after the re-hint transaction committed in
   memory still leaves the in-memory map pointing at sealed, existing
   segments, so reads stay correct until the reopen reloads the tip. Name
   the moved segments in the commit either way (M17 rule 4).

3. **major — destage errors are not classified; permanent errors are
   retried forever, each retry writing and deleting the whole cut.**
   `coord.rs:274-281` returns the destager's error to `tick`, which logs a
   warning (`engine.rs:299-300`) and retries at the next period (100 ms).
   For `Auth`/`Corrupt` on a base read (a damaged or forged extent, which R1
   makes an error on purpose), `Invalid` (a malformed cut), `Unsupported`,
   `NotFound`: the same cut fails the same way; `SimpleDestager` stores
   every unique chunk to a segment before the map transaction and deletes
   the segments on failure (`destage.rs:112-118`), so each attempt rewrites
   the cut's data, 10 times a second, while the journal fills and writes
   park. M9's review asked for exactly this classification and M9 now has
   `ErrorClass` (`Transient`, `AfterSpace`, `AfterMapSeal`, `CutTooLarge`,
   `Permanent`, `Stop`) with a chunk number; the engine's seam (`Destager`,
   `destage.rs:96-108`) returns `lunix_iface::Result` and drops it, so the
   M12 adapter cannot pass it through and the coordinator distinguishes only
   `MapFull`. **Fix:** give the seam a typed error
   (`DestageFailure { error: Error, class: Retry, chunk: Option<u64> }` with
   `Retry::{Later, AfterSpace, CutTooLarge, Never, Stop}`, 1:1 with M9's),
   have `SimpleDestager` classify with the same table, and in `commit`:
   `Never` and `Stop` set `failed` (writes `EIO`, reads continue, a loud
   `error!` naming the chunk's LBA range once), `CutTooLarge` cuts smaller,
   `Later`/`AfterSpace` fail this attempt as today. (`err-custom-type`,
   `err-doc-errors`, `obs-error-chain`: log once.)

4. **major — `map.grow` runs before the destage and before any failure can
   be refused.** `coord.rs:253-258`: a grow commit grows the in-memory map
   at the start of C3; if the destage then fails (`abandon`, retryable) the
   map is grown while the durable tip, `self.size` and the next commit's
   `size` are not. The next ordinary commit seals a map of `new/cs` chunks
   under a commit whose `size` is the old one; `Engine::open` then opens the
   root with `tip.size / cs` chunks (`recover.rs:206`) against a root of a
   different depth, which the real map may refuse or misread. The MapFull
   retry also re-runs `grow` (harmless). **Fix:** move the `grow` to just
   before `seal_map` (after destage and `apply_moved`; this cut never
   touches chunks beyond the old size because `self.size` is published only
   after C6), so a destage failure leaves the map untouched and a seal
   failure is already fatal. Add a test: grow with a destager fault, then a
   plain commit and a reopen.

5. **major — the model never has a write in flight at a cut, so C1 and the
   overlay/map hand-off are untested by it.** `model.rs:157-199`: every
   batch waits for all its tags before the step ends, and commits
   (`:312-317`, `:259-262`) run on the test thread with nothing outstanding;
   `Mixed` completions therefore never leave a reserved or appended-but-
   uncompleted record at C1, and no read runs inside the C2-C9 window. With
   `wait_inflight` deleted the model passes 100 x 2000 at both chunk sizes
   (mutation `no-c1-wait`); only `queue::the_background_coordinator...`
   catches it, and that test depends on a 1 ms thread race. The report's
   decision 1 is the single most delicate piece of the protocol and has no
   deterministic test. **Fix:** in the model, hold some appends with
   `JournalMode::Manual`/`complete_append_seq` across a `commit_now` (the
   fake supports it): submit writes, complete only a subset, commit (the
   coordinator waits; complete the rest from another thread or interleave
   with `pump`), then check reads and the reopen prefix; and issue reads
   between the commit's steps by arming a crash point that stops the engine
   after C3 and reading blocks of a partially overwritten chunk. Also assert
   that `first_match` is the *full* device at least once per case with
   `min_n == log.len()` after a forced commit (it is today, implicitly).

### Minors

6. **minor — the cut is bounded by chunk count, not by distinct leaves, and
   the bound costs a second full cut.** `coord.rs:218-239`,
   `config.rs:78`: `max_chunks_per_cut = map_dirty_leaves_max / 2` (10 000)
   bounds a 16 KiB volume to about 156 MiB per cut however sequential the
   writes, so a 1 GiB `B` trigger becomes seven back-to-back commits, and
   `bound_cut` builds a probe `overlay.cut(c)` (a second MVCC view of the
   whole overlay) whenever the overlay exceeds the cap. The quantity the cap
   protects is distinct leaves (`chunk / fanout`). **Fix:** count distinct
   `chunk / fanout` in the probe (the descriptor's `map_fanout`, D1; add it
   to `EngineConfig`) and stop at the first chunk that would exceed
   `dirty_cap / 2` leaves; consider having the overlay expose a cheaper
   "chunks with a version <= c" count (M4 owns that) so the probe is not a
   full cut. M16 should measure either way.

7. **minor — a poisoning that happens during `wait_inflight` is not
   re-checked.** `coord.rs:188-191` clamps `c` below `poisoned_at` once; a
   failed append at `p <= c` during the wait removes `p` from the in-flight
   set (`queue.rs:375-380`, `:617`) and the cut proceeds with `cut_seq = c`
   while record `p` is missing from it. The guest saw `EIO` for `p`, so the
   image is a legal post-failure state, but the commit claims a prefix it
   does not hold and the journal below `c` is reclaimed. **Fix:** re-read
   `poisoned_at` after the wait and clamp (or fail this cut) before C2.

8. **minor — an index insert failure skips the rest of the cut's entries
   for good.** `coord.rs:430-438`: `break` on the first `insert` error; the
   comment says `catch_up` restores them, but `catch_up` only covers
   segments the *retained* chain names, so after two more commits the
   extents are never indexed (dedup misses only, no data risk). Insert the
   rest, log once, and let `persist` run anyway.

9. **minor — stale ring files are not reclaimed at open.** After a crash
   between C6 and C9, `recover` replays records above the tip's cut and
   charges them (`recover.rs:224-235`), but the files wholly below
   `tail_after(tip.cut_seq)` stay until the next commit's C9; an idle volume
   after reopen keeps them and `fill()` overstates the ring. The tip is
   durable, so `reclaim(tail_after(tip.cut_seq))` right after `recover` is
   safe; it also makes the ledger exact at open.

10. **minor — a dropped queue handler leaves its in-flight set behind.**
    `QueueShared` stays in `inner.queues` after the `Queue` is dropped
    (`engine.rs:384-395`, no `Drop` for `Queue`); a handler dropped with an
    appended-but-uncompleted record makes every later C1 wait
    `inflight_timeout` (60 s) and fail with `Busy`, including `quiesce`.
    Clear the slot and the set in `Drop`, or document that the ublk layer
    drains before dropping (M10's W1/W2 rules).

11. **minor — the fallback for `Cached::Hit` taken on the queue thread
    reads the entry it saw, not the current one.** `queue.rs:541-553`,
    `read.rs:191-194`: the helper reads `job.entry` as the queue thread saw
    it; if a cut committed its map transaction in between, the helper reads
    the previous extent. The bytes are the same for every block the overlay
    did not hold at lookup time (the new extent differs only in blocks the
    cut wrote, which the overlay served), so this is correct today; it stops
    being correct if a future change lets the overlay release before the
    read's lookup completes. One sentence in `map_parts` stating the
    invariant it relies on ("the overlay still holds every block the new
    extent changed until C9") would protect it.

12. **minor — `reserve_bounds` undercounts interior nodes.** `coord.rs:94`:
    `leaves + 4` node extents; a cut touching leaves in many subtrees of a
    depth-3 tree rewrites one path per subtree. Only the ledger's
    transient excess is affected (headroom is a bound, `transfer` never
    fails), so a warning-level issue; `leaves * depth` is the safe bound.

13. **minor — background `tick` on a failed engine warns every 100 ms.**
    `engine.rs:296-301` skips `is_failed()`, but a destage error that is
    not fatal (major 3) logs `background commit failed` ten times a second
    for as long as it persists. Rate-limit or log on class change only.

### Nits

14. `README.md:14` still lists "the open-segment wait" in `read.rs`; it was
    removed with decision 3.
15. `queue.rs:666`: `std::mem::replace(&mut r.kind, Kind::Flush)` as a
    temporary placeholder while `map_parts` runs; a `Kind::Taken` variant or
    taking `pending`/`buf` out of the state would say what it means.
16. `queue.rs:286-298`: `advance` for a `Hole` computes
    `(max_chunks_per_cut / 2).max(1) * chunk_blocks` with a plain `*`;
    `saturating_mul` (`num-overflow-explicit`).
17. `queue.rs:212-218`: `ops_*` are counted before validation, so `EINVAL`
    requests inflate the op counters.
18. `engine.rs:160-170`: `wait_inflight` polls every 5 ms with the condvar
    as a sleep; checking `busy` under `progress.0` before waiting makes the
    wakeup exact and removes the timer.
19. `destage.rs:62-64`: `DestageCtx::space` is unused by `SimpleDestager`
    (no `is_read_only` check); M9 uses it, fine, but the stand-in should
    refuse a read-only volume too so the seam's contract is exercised.
20. `coord.rs:430-446`: `persist` runs only when `new_segments` is non-empty;
    entries `catch_up` inserted at open wait for a cut with segments. Fine,
    but say so where `upto_seg` is set.
21. `lib.rs` invariants list and `coord.rs` module docs say C4 < C5 < C6;
    with decision 3, C4 is a ledger step and the durable order is "C3
    (segments sealed) < C5 < C6"; the `engine.c4.sealed` crash point sits
    between two non-durable steps. Rename it `engine.c4.named` or note that
    it marks the point where `new_segments` is fixed.

## Protocol walk (what I checked and found right)

- **C1.** `last_issued()` is read before the sets; a queue inserts the number
  under the same lock as `try_reserve` and removes it after the overlay
  insert (`queue.rs:335-347`, `:607-613`). A reserved slot that `append`
  rejects is removed after poisoning. A write parked on `Full`/`NoSpace`
  holds no number (`:349-355`). `bound_cut` runs after the wait, so every
  `c' <= c` is a cut.
- **FLUSH/FUA.** `upto = last_issued()` after the waiters are taken
  (`flush.rs:63`); one group in flight; a refused submission completes its
  waiters with `EIO`; the callback takes no lock the submitter holds. A FUA
  write joins the group only after every part's `Append` completion
  (`queue.rs:636-640`); with `upto >= own seq` it is covered. Poisoning:
  groups at or above the failed number fail, below complete; a FUA whose
  group fails gets `EIO`. Confirmed by `queue::a_thousand_flushes...` and
  the `flush-immediate`/`fua-ignored` mutations.
- **C3-C6.** Segments sealed before the map transaction commits (M9's
  revised contract, mirrored by `SimpleDestager`), so no map entry ever
  names an `.open` file; the pack is sealed and dropped when empty; the FULL
  carries `commit_seq = parent + 1`, `cut_seq = c'`, `chain_floor =
  max(parent, seq - (retain_last - 1))`, the `created_at` clamp,
  `index_packs = runs()` (durable before C6, D3), `new_segments` plus the
  applied moves' output segments (M17 rule 4). `GStore::put` checks every
  named object exists sealed; the `unsealed-pack` mutation proves the check.
- **C7-C9.** Checkpoint then floor, both after `put`; `tail_after(c)` after
  C6 and reused at C9, conservative for records appended between (a lane's
  current file is never below the tail); `reclaim` only after C6; the
  overlay released after the reclaim; `JournalReserved` release clamped to
  the engine's charge (M3 decision 6). The `reclaim-before-c6` and
  `floor-before-c6` mutations are caught.
- **Orphan sweep vs the map.** Every segment the tip's map references has
  an id below the tip's root pack (segments of a cut are allocated before
  its pack; compaction output is allocated after the pack and named only by
  the cut that applies its re-hints), and `high_water` includes
  `map_root.pack` (`recover.rs:80-98`), so the mark is never below a
  referenced segment. A pending `moved` record lost in a crash leaves the
  index memtable's `relocate` unpersisted (it is persisted only once the
  output segment is named), so the manifest still points at the old copy.
  Correct, but it rests on "root pack id > every referenced segment id",
  which is nowhere written down: add it to `recover.rs`'s module docs and
  to DESIGN §4.4 step 5.
- **R1.** `open_at` is the one `read_extent` call; `Expectation` has no
  "none"; `id_len = 0` becomes `verify_id`; `Auth` at the hint counts and
  still tries index candidates with the full id, skipping the hint itself;
  `NotFound`/`Corrupt` is a stale hint and counts nothing. The read cache
  key `(hint, prefix)` cannot go stale: ids are never reused in a process
  (allocator floor at open, monotonic in process), segments are immutable,
  a re-hint or an overwrite changes the key. The six `r1` tests cover swap,
  older extent, damaged tag, wrong offset, stale-hint fallback with full-id
  confirmation and a candidate with another id.
- **R4.** `verify_chain` to `chain_floor`, then the floor compare, then the
  first write (the sweep). `Rollback`, `Fork`, first attach, raise after a
  verified open, raise only after C6. The checkpoint never anchors
  (`id_high` is still taken from a beyond-tip checkpoint, which only raises
  the allocator and is safe).
- **Recovery idempotence.** Writes during `open` are the sweep (idempotent)
  and the floor raise (raise-only); the model crashes at every
  `engine.recover.*` point and reopens.
- **ADR-18.** Queue-thread paths take short locks only (`inflight`,
  `deferred`, the cache, the pool queue, the flush state), call
  non-blocking trait methods, and copy at most one request buffer; every
  blocking fake method is wrapped with `not_on_queue` and no engine test
  trips it. The coordinator and helpers never take a lock a queue thread
  holds across a call.

## Recommendations on the implementer's decisions (report numbering)

1. C1 visibility through the shared lock: **confirm.** Add the deterministic
   test of major 5.
2. Index inserts after C6: **confirm.**
3. Segments sealed inside the destager, C4 a ledger step: **confirm**; fix
   the wording (nit 21) and the plan's step 3/4 text (spec gaps below).
4. Compaction output named by the cut that applies the re-hints; chunks
   found by a leaf walk: **confirm the naming**, change the order (major 2),
   and accept request 2 so the walk goes away.
5. `T` from the first dirty tick: **confirm.** It matches "idle volumes cut
   nothing" and gives a steady writer a commit every `T`.
6. Bounded cuts by chunk count, hole records split: **confirm the mechanism,
   change the quantity** (minor 6). The MapFull retry at `c' = prev + (c -
   prev) / 2` is sound: `c' <= c` after the wait is a prefix cut, the
   overlay view at `c'` holds every write `<= c'`, `release(cut)` removes
   only `<= c'`, and the loop ends at `c' = prev + 1` (then `Err`). It
   cannot livelock, but each attempt destages the whole cut and deletes it;
   with M9's pre-probe (`set_hole` for every chunk, no bytes written) the
   cost is a map probe per halving. Ask the adapter to keep M9's probe and
   give `SimpleDestager` the same.
7. `JournalReserved` releases clamped: **confirm.**
8. Read cache keyed by `(hint, prefix)`: **confirm** (see R1 above).
9. FUA joins the next group with `upto = last_issued()`: **confirm**; a
   superset of ADR-28's "its own seq", costing at most a wait for higher
   numbers already appended.
10. R1 fallback classification (`Auth` counts and falls back,
    `NotFound`/`Corrupt` is stale): **confirm**; it is what the plan's
    step 4 and the gate both say.
11. Grow as a forced commit, size visible after C6: **confirm**, with the
    `grow` moved past the destage (major 4).

## Requests to the lead

1. **`VolumeDir` rollback-floor methods** (`rollback_floor`,
   `raise_rollback_floor` with defaults): **accept as proposed**, additive
   and default-implemented so no existing impl breaks. Two notes: `raise`
   must refuse an *equal* `max_commit_seq` with a different hash too
   (`Refused`), so a buggy caller cannot turn a floor into a fork; and the
   engine's `RollbackFloorStore` should stay as the engine-side trait with a
   blanket impl over `VolumeDir` in M12, so the engine never depends on the
   iface change landing first.
2. **`Moved` carries `ci: ChunkNo`**: **accept.** M17 knows the chunk from
   its mark walk; the engine then calls `rehint(ci, old, new)` directly, no
   leaf walk per cut (today a full walk of the tip's map at every cut that
   applies moves, `coord.rs:521-528`). Keep `old` as the guard. If the lead
   prefers not to touch `lunix-iface`, M17 can hand the engine
   `Vec<(ChunkNo, Moved)>` through the collector's `GcProgress` instead.
3. **Destage seam / M9 adapter**: the seam carries M9's revised contract in
   shape (borrowed cut; every segment sealed before the map commit; inserts
   returned, applied after C6; failed-cut segments deleted by the destager;
   `SpaceGate` for `ReadOnly`/`fatal_io` only; `DestageStats` and
   `DestagedSegment.entries` map onto `DestageOutcome`). Three gaps to close
   in M11 before the adapter is written: (a) the **error class and chunk
   number** are dropped (major 3); (b) **`extent_auth_failures`** from M9's
   own base reads (M9 does not use `DestageCtx::reader`) has no field in
   `DestageOutcome`, so the counter undercounts in production: add it; (c)
   M9's per-cut **pressure** input: the adapter can derive it from
   `ctx.journal.fill()`, which the ctx has; say so in `DestageCtx`'s docs.
   M9's "cut too large" (`CutTooLarge`) should map to the retry-smaller
   path, and so should `AfterMapSeal`: the engine never seals mid-cut, and
   once major 2 applies the re-hints before the destage their leaves are
   dirty at C3, so a `MapFull` there is "cut smaller" in both classes.
4. **`GcControl` deferred to M12**: **accept.** `run_pass`/`run_urgent` need
   to run on the coordinator between cuts with the map at the tip, so M12
   should implement them as requests the coordinator services at C10 (or
   from `tick` when idle), not as direct `Collector` calls; `Engine::commit_now`
   is not enough on its own because an urgent pass must also be able to run
   without a dirty overlay. A small `Engine::run_gc(req) -> Result<...>` that
   takes the coordinator mutex and calls `urgent`/`dry_run` under the same
   preconditions as `gc_step` is the natural home.

## Spec gaps (recommendations)

- **Sweep order.** DESIGN §4.4 (journal, then sweep, then catch-up) and the
  plan's step 6 ("orphan sweep first") differ only in where the journal sits.
  The engine's order (verify, R4, sweep, catch-up, map, checkpoint, journal,
  overlay, floor) satisfies both constraints that matter (sweep before
  catch-up; R4 before any write). Recommend rewording §4.4 to "the sweep
  runs before the index catch-up and before the map is opened; its position
  relative to journal recovery is free", and adding the invariant the sweep
  relies on (root pack id above every referenced segment id).
- **Verify to `chain_floor` at mount.** D5 already says mount verifies to
  `chain_floor`; the plan's step 6 "verify the last link" is the stale
  wording. Keep the engine's behaviour; fix the plan text.
- **C3/C4 wording.** Agree with the M9 review: DESIGN §8.2 local order steps
  2-3 should read "destage the cut: extents to segments, every segment
  sealed (footer, trailer, `fdatasync`, rename, directory `fsync`), then the
  map transaction committed in memory; index inserts held back", and step 3
  becomes the ledger true-up and `new_segments` bookkeeping. The plan's C4
  text likewise.
- **Truncate-tip and reclaimed records.** Correct: records of the set-aside
  commit's cut were reclaimed at its C9 and are gone; a sentence in the fsck
  how-to ("`--truncate-tip` loses the writes between the previous commit's
  cut and the damaged tip's cut that are no longer in the journal") is
  owed by M14.
- **Journal-damage logging granularity.** The M12 note asks for "every
  damaged range with seq bounds"; `recover.rs:213-219` logs one `warn!` with
  the `Debug` of the vectors, which carries the bounds but as one line. Fine
  for M11; M12's gate test should accept it or ask for one line per range.

## rust-skills notes

`#![forbid(unsafe_code)]`; no `unwrap` outside tests, `expect` only on
thread spawns (`err-no-unwrap-prod`); no indexing on data read from disk in
library code (`st.buf[..]` slices are request-relative and bounds-derived);
`thiserror` for `OpenError` with `#[source]` chains (`err-thiserror-lib`,
`err-source-chain`); `tracing` with structured fields (`obs-structured-fields`);
`Relaxed` for counters, `Acquire`/`Release` for flags (`conc-atomic-ordering`);
`u32::try_from`/`usize::try_from` on narrowing (`num-cast-try-from`); the
`counters!` macro replaces 19 hand-written pairs (`macro-prefer-functions`
respected: a function cannot generate fields). Where it departs:
`anti-empty-catch` in `on_jread` (blocker 1) and the `break` in C8 (minor
8); `num-overflow-explicit` in the hole-size arithmetic (nit 16);
`err-doc-errors` on `Destager::destage` lists `MapFull` only (major 3).
Public items are documented; `#[non_exhaustive]` on `OpenError` only, which
matches the workspace.

## Re-review (fix commits 31fc879 and 22e7edd, diff from 987fda3)

### Verdict: **merge**

The fix diff addresses the blocker and the four majors as described in the
report's "Review fixes" table, and the new tests bite. Re-run on the host in a
fresh directory at 22e7edd: `cargo test -p lunix-engine` **46 passed** (unit 1,
`api` 3, `basic` 18, `model` 4 incl. the two in-flight cases, `queue` 8, `r1`
6, `review` 6). Three mutations in a scratch copy: unconditional map fallback
on a failed journal read makes `review::..._undestaged_write_is_eio_not_stale_data`
fail with the stale bytes; `wait_inflight` removed makes both
`model::in_flight_writes_at_a_cut_*` fail at "C1 waits for the in-flight
writes"; a re-hint failure made fatal again makes
`review::a_failed_rehint_is_deferred...` fail. Host directory and scratch copy
deleted. The two new findings below are minor and can follow in a later
commit.

1. **Blocker fix, complete.** The only journal reads in the engine are the
   queue's `submit_read` (`start_read`) and `on_jread`; both now route a
   failure through `journal_read_failed`, which falls back to the map only
   when the piece's `write_seq <= durable_cut` and otherwise fails the read
   with the journal's errno and logs (`queue.rs:718-745`). The read helper
   pool and the decoded-chunk cache never touch the journal (map and store
   only), so they need no change. `durable_cut` is initialised from the
   verified tip's `cut_seq` at open (`engine.rs:267`) and stored at C6 after
   `commits.put` returned and before the `engine.c6.committed` point
   (`coord.rs:483`, `:508`); the value stored is the bounded `c'` actually
   committed. The fallback's correctness argument holds: a piece at or below
   `durable_cut` was in the cut that commit destaged, the map transaction
   committed at C3 before C6, and the overlay is released only at C9, so the
   map has that version or a newer one. Both failure paths are tested, in
   both directions (undestaged: `EIO`; destaged, stopped after C6: data).
2. **Re-hints before the destage, `pending_named`.** `apply_moved` runs first
   in C3 in its own transaction; every error path returns before
   `co.moved.clear()`, so a failed transaction (dropped, nothing applied)
   keeps the records for the next cut. On success the output segments go to
   `co.pending_named`, which C4 folds into `new_segments` and C6 clears, and
   a non-empty `pending_named` is a commit trigger and defeats the idle
   return. Sweep safety: an output segment named by no durable commit is
   above the mark and swept at open, and the map is reloaded from the tip,
   whose leaves never saw the re-hint (the in-memory re-hinted leaves were
   never sealed into a named pack: C5/C6 either both happen or the process
   is stopped/crashed before C6); the index's `relocate` lives in the
   memtable and is persisted only once the segment is named. Reads: the
   collector returns `Moved.new` only for a sealed segment (it has already
   told the index), so an in-memory re-hint never points at an `.open` file;
   an unnamed one is readable. GC cannot run while re-hinted leaves are
   dirty (`gc_step` requires root == tip root and no dirty leaf), so no new
   `moved` records arrive while `pending_named` is outstanding. Correct.
3. **Classification and stop behaviour.** `Retry::classify` is M9's table
   (host `EIO`/`EROFS` -> `Stop`; `NoSpace` -> `AfterSpace`; `MapFull` ->
   `AfterMapSeal`/`CutTooLarge` by `dirty_leaf_count` taken before the
   destage; `Io`/`Busy` -> `Later`; `ReadOnly`/`Closed` -> `Stop`; else
   `Never`). The coordinator cuts smaller on both `MapFull` classes, fails
   the attempt on `Later`/`AfterSpace`, and on `Never`/`Stop` sets `failed`
   with one `error!` naming the chunk's LBA range; the background tick logs
   once per distinct error. `SimpleDestager` refuses a read-only volume and
   reports the chunk it was on (`None` for the final-transaction `MapFull`).
   The damaged-base-extent test shows an R1 `Auth` on a partial overwrite is
   `Never`, counted, and not retried. Correct; the seam now carries M9's
   contract 1:1 (`extent_auth_failures` added to the outcome, the pressure
   source documented).
4. **`map.grow` at C5.** Now after C4 and immediately before `seal_map`,
   inside the fatal zone; a destage failure leaves the map at the committed
   size (test: grow with a failing destage, plain commit, reopen, grow
   again). The argument that this cut holds no chunk past the old size
   (`self.size` is published after C6, writes beyond it are `EINVAL`) is
   right. Correct.
5. **In-flight model cases.** `JournalMode::Manual` holds three appends
   (reserved and appended, completion withheld) on two lanes while
   `commit_now` runs on another thread; the test asserts the commit has not
   returned after 100 ms, releases the completions, and checks the cut
   covers them (`cut_seq >= 4`) and that a reopen with every unflushed
   record lost still reads them: that is exactly a write in flight at C1,
   and the mutation confirms the first assertion fires without the wait. The
   window reads (stopped at `c3.destaged`, `c5.map_sealed`, `c6.committed`)
   exercise the overlay-plus-map hand-off for a partially overwritten chunk
   and the "C6 keeps the overwrite" boundary. The case does not cover a slot
   reserved but not yet appended, which cannot be produced from outside the
   queue thread (reserve and append are adjacent), nor a read racing C9's
   release, which the background test still covers nondeterministically.
   Adequate for the gate.
6. **Minor 9 deferred: acceptable.** The stated reason is not quite right
   (DESIGN §4.1 lets a `reclaim` perform the stale-tail wipe too, and the
   damage line is logged and `refuse` decided before any reclaim could run),
   but the consequence is only that `fill()` overstates the ring after a
   crash between C6 and C9 until the first commit, and the first write
   triggers one. Fine to leave; M12 may call `commit_now(false)` once after
   open if it wants an exact ledger.

### New findings (minor)

7. **minor — dirty re-hint leaves are not counted against the cut bound, so
   a persistent `MapFull` can stall commits.** `bound_cut` (`coord.rs:266-304`)
   bounds the cut's own leaves at `max_leaves_per_cut` but ignores
   `map.dirty_leaf_count()`, which after a failed cut holds the re-hinted
   leaves. If re-hints plus one record's leaves exceed the map's cap, every
   attempt is `AfterMapSeal`, the halving ends at `c = prev + 1` with an
   error, and the next tick repeats it (writing and deleting the cut's
   segments each time); nothing ever seals the re-hinted leaves. Unreachable
   with defaults (one `Collector::step` moves at most a budget's worth of
   extents, far below `map_dirty_leaves_max / 2`), so minor. **Fix:** use
   `cap - dirty_leaf_count()` (at least 1) as the bound, and when `Smaller`
   arrives at `c <= prev + 1` with dirty leaves present, run
   `cut_and_commit(co, prev, size)` once: an empty cut that seals the
   re-hinted leaves and names `pending_named`, after which the normal cut
   fits.
8. **minor — `EngineConfig::from_volume_json` hard-codes `map_fanout: 512`.**
   `config.rs:82`: the field's doc says "the descriptor's `map_fanout`", but
   the constructor has no parameter for it, so M12 can forget to set it. A
   256-fanout volume then bounds cuts and headroom at twice the intended
   leaves (still under the cap, so no failure, but the bound is off). Take
   `map_fanout` as a constructor argument next to `chunk_size` and `id_len`.
9. nit — `Queue::drop` poisons the volume when numbers are still in flight
   (`queue.rs:842-861`). Safe (no cut may pass an unacknowledged record whose
   completion will never be seen) and the process is going down, but it
   turns a shutdown-order mistake in the ublk layer into `fatal_io` and a
   read-only volume for the rest of the process; a `tracing::error!` plus
   clearing the set, leaving the records to the journal (they are replayed or
   lost at reopen either way, both legal for unacknowledged writes), would be
   gentler. M10/M12 should document that handlers are dropped only after a
   drain.
