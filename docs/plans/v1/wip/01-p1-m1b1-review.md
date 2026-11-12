# Review of M1.B1 — `lunix-iface` (branch `p1/m1-b1` at 8752c0a)

Independent review against the plan's M1.B1 section and gate, the walk of
M2-M17, M18a/b (steps, gates, "Reads"), D1-D17, the cross-cutting rules,
DESIGN §4.1/§4.4/§5/§11, ADR-28 and `lunix-format` on main. The gate was
re-run on the remote host in a fresh directory (deleted afterwards).
`rust-skills` rule ids are cited where they apply; plan and spec win.

## Verdict: **merge after fixes**

The crate is well shaped: every trait item states its thread and blocking
class, the control protocol is bounded and redacted, the crashpoint macro and
the contract suites do what the plan asks, and the walk below finds every
milestone expressible **except** for one contract that, as written, makes
M11 lose fsync-acknowledged data (finding 1) and five gaps that would force a
mid-wave change (findings 2-6). All fixes are inside `lunix-iface`; none
touches `lunix-format`. Fix 1 should land before any wave 1a branch is cut,
because M3 and M11 both build on it.

## Gate re-run (host, `8752c0a`, `tools/remote.sh -m m1b1`, CPUs pinned)

| command | result |
|---|---|
| `run bash tools/ci.sh` | exit 0: fmt, clippy `-D warnings`, `cargo test --workspace`, feature-edge `ok`, `lunixd --features crashpoints` builds, `check-ownership: m1b1 ok (41 changed paths)` (report says 40: its own report commit is the 41st) |
| `cargo clippy -p lunix-iface --all-targets --features crashpoints -- -D warnings` | exit 0 |
| `cargo test -p lunix-iface` | ctl 13, contract_trivial 52 (journal 9, overlay 7, map 7, index 9, store 12, space 8), chain 2, doctest 1 ignored |
| `cargo test -p lunix-iface --features crashpoints` | crashpoints 6 (registry listed/sorted/unique, SIGKILL at hit 3 in a child, default hit 1, bad specs inert) plus the above |

Counts match the report. The branch changes only `crates/lunix-iface/**`,
`Cargo.lock` and the report (step 12 of M1.B1, filling the skeleton
`Cargo.toml`s, was not done and is not listed under "Skipped": finding 18).

## The walk

One row per milestone: what it must do, mapped to iface items; gaps refer to
the numbered findings.

| milestone | operations → iface | gaps |
|---|---|---|
| M2 crypto | `ExtentCodec::{mode,chunk_id,seal,open}` with `OpenExpect`/`OpenOutput`; `MetaCodec::{seal,open}` (bucket-relative name, D9); `CommitCodec::{seal_commit,open_commit,object_hash,verify_chain}`; `Keys`/`KeyMaterial` (`as_any` for the concrete type). Keyring, `check_attach`, `integrity.key`, lease are M2's own API, consumed by M12/M13b directly (allowed: `lunixd` and `lunix-cli` depend on `lunix-crypto`). | 3 (open allocates; node cap unspecified), 16 (open order wording), 14 (no suite) |
| M3 journal | `Journal::{lanes,open_lane,submit_group_flush,read_at,recover,tail_after,reclaim,fill}`, `JournalLane::{try_append,submit_read,poll,completed_watermark}`, `AppendRecord`→`RecordHeader` (journal fills `gen`/`off`), `Recovered`, `JournalTail`, `ReclaimReport`. | **1** (seq/flush contract), 2 (cross-lane reads, alignment), 5 (reclaim bytes), 6 (damage classification), 17 |
| M4 overlay | `Overlay::{insert_write,insert_hole,lookup,cut,release,dirty_bytes}`, `OverlayCut`/`CutChunk`/`Piece`; chunk size via constructor (the suite passes `chunk_blocks`). | none |
| M5 map | `BlockMap::{chunk_count,get,try_get_cached,begin_update,seal,grow,root,dirty_leaf_count,walk_*}`, `MapTxn::{set,set_hole,rehint,commit}`, `NodeSource` (blanket over `PackRead`), `SealResult`, `MapFull`; `open(root)` is the constructor. | 9 (root in `walk_nodes`; node `chunk_size`) |
| M6 index | every step maps 1:1 (`get_for_dedup` with `Confirm`, `get`, `get_by_prefix`, `insert`, `relocate`, `mark/unmark_condemned`, `condemned`, `drop_segment`, `persist`, `runs`, `catch_up(&[SegId], &dyn SegmentFooters)`, `rebuild`, `delete_run`, `iter_entries`). | 4 (no listing of on-disk runs), 7 (`persist` invariant) |
| M7 store | `VolumeDir` (lock in the constructor; `volume_bytes`, `volume_json`, device record, pins, `ids()`, leftovers, `sweep_orphans`), `SegmentWriter`/`SegmentStore` (`locate` for the engine's ring, `footer`, `list`, `delete`), `PackWrite`/`PackRead`/`PackStore`, `CommitStore` (`delete(seq, chain_floor)`, `set_aside`), `CheckpointStore`, `PinSource`, `GcStateStore` (`put_evidence`, `evidence_passes`), `IdAllocator`. Durable order and crash points are M7's. | 8 (`slot_seq` owner), 11 (`dir()`), 12 (dict id at `append_extent`) |
| M8 space | `SpaceGate` (`reserve` with `Waiter`, `reserve_headroom`, `release`, `transfer`, `tick`, `snapshot`, `reconfigure`), `Ladder::register`, `LadderStep::run`, `SpaceEvents`; walk-through in module docs and README. `fs_free` source is a constructor argument (the suite's `SpaceSetup.capacity` needs it). | 5 (journal units), 10 (startup charge) |
| M9 destage | `Journal::read_at`, `Overlay` cut pieces, `BlockMap::get` + `MapTxn`, `ChunkIndex::get_for_dedup` → `SegmentStore::read_extent` → `ExtentCodec::open(Stored, chunk_id: Some)` (D17 confirmation), `chunk_id`, `seal`, `SegmentWriter::{append_extent,is_full,seal(Some(range))}`, `SpaceGate::reserve_headroom`. Compression and the worker pool are M9's own; `moved` rehints can be a separate `MapTxn` before C3's. | 3 (per-chunk `Vec` from `open`) |
| M10 ublk | `BlockEngine::{size_bytes,queue_handler,grow,set_on_resize,stats}`, `QueueHandler::{on_request,poll,wake_fd}`, `Completer::{complete,tag_buf}`, `IoRequest`/`IoOp`/`IoStatus`, `QueueConfig`, `ResizeHook`. Threading rule and deferred-READ copy documented on the trait. | 10 (`len: u32` vs discard cap), 11 (`quiesce`) |
| M11 engine | write path (`reserve` → `try_append` → `insert_write` → `Append` completion), FLUSH/FUA (`submit_group_flush` + `FlushDone` → wake fds → `poll`), holes (`RecordKind::Hole` + `insert_hole`), read path (`lookup`, `submit_read`, `try_get_cached`, `locate`, helper pool with `get`/`read_extent`/`open`, `get_by_prefix` fallback), C1-C10 (`cut`, destage, `seal`, `map.seal`, `Full` + `seal_commit` + `put`, `tail_after` + `store`, `persist`, `reclaim` + `release`, `Collector::step`), startup (`tip`/`open_commit`/`verify_chain`, `load`, `recover`, `sweep_orphans` → `observe`, `catch_up`), grow (`map.grow` + FULL + `ResizeHook`), `GcControl` wiring, `EngineStats::{to_slots,from_slots}`. | **1**, 2, 3, 5, 6, 10 |
| M12 lunixd | `ctl` frames (`frame_len` for the tokio reader, `read_frame`/`write_frame` sync), all 11 `Request` variants with matching `Response` bodies, `Request::validate`, `Secret` (zeroizing, redacted), `ErrorCode`; `GcControl::{run_pass,status}`, `SpaceGate::{reconfigure,snapshot}` → `SpaceFigures`, `EngineStats`, `crashpoint::registered()` and `dump_counts()` at clean exit. Detach's final cut is a concrete-engine call (see 11). | 11, 13 |
| M13a cli | `ctl` types, `ErrorCode::exit_code()`; D16 flags map to `VolumeMode`/`key_file`/`passphrase`; `Space` fields match the flags. | 13 |
| M13b cli | `fsck`/`key` link `lunix-fsck`/`lunix-crypto` directly; exit codes from `ErrorCode`. | none |
| M14 fsck | `VolumeDir` (lock ⇒ `Busy` when attached, `volume_bytes`, `leftovers`, `remove_leftovers`), `CommitStore::{list,get,set_aside}`, `CommitCodec::{open_commit,verify_chain}`, `BlockMap::walk_*` over `PackRead`, `SegmentStore::{list,footer,read_extent}` + `ExtentCodec::open(verify_id, Plaintext)` for `--deep`, `ChunkIndex::{runs,iter_entries,rebuild}`, `Journal::recover` (`ignored_after_gap`, `damaged`), `CheckpointStore::{load,store,zero}`, `--repair --gc` via `Collector` + `GcView`. | 6 (damage above/below floor), 8 |
| M15 exit | `lunixd --list-crashpoints` (`registered`, `check_registry`), `LUNIX_CRASH_AT=<name>:<hit>`, `LUNIX_CRASH_COUNT` + `dump_counts` for middle/last hits; `lunix status --json` fields (`dedup_ratio`, `compression_ratio`). | none |
| M16 perf | `EngineStats` (`cold_rmw`, `stage_ns`, flush group sizes), `verify_reads` via `volume.json` (format), `status --json`. | none |
| M17.A gc | `Collector::{step,urgent,dry_run,stats}` over `GcView` (retained commits, `TipInfo.high_water_mark`, pins, map walks with skip sets, index `get`/`mark_condemned`/`unmark`/`relocate`/`drop_segment`/`delete_run`, `SegmentStore::{footer,read_extent,open_segment,delete}`, `PackStore::{list_packs,delete_pack}`, `CommitStore::{list,delete}`, `ExtentCodec::{open(Stored),seal}` re-seal with fresh salt, `GcStateStore`, `SpaceGate::{reserve_headroom,transfer,release}`, `Clock`), `Moved`, `GcProgress`, `GcPassSummary`, `GcStatusSnapshot`, `GcControl` for the ladder. Liveness resolution through `footer(seg).entries` (one read per segment) avoids a per-chunk extent read. | 4 (dead index runs invisible), 9 (root node's pack) |
| M17.B gc stack | fsck's `check_gc` uses the same traits plus `GcStateStore::load_condemned`; ladder test through `Ladder::register(Compact, ..)` → `GcControl::run_urgent`. | 4 |
| M18a codec 3 | `Codec::ZstdDict` exists in format; `EncodingDesc` carries codec/level/ulen; `open` returns `Unsupported` for codec 3; footer `dict_ids` exists in format. Phase 4 needs the dict id at `SegmentWriter::append_extent` (ciphertext hides the zstd `dictID`) and in `EncodingDesc`. | 12 (make both additive now) |
| M18b study | stage 3 reads the stored chunk-id set through `lunix-fsck`'s walk (`walk_leaves` + footers); no other iface use. | none |

## Findings

Severity: **blocker** (M11 built to the documented contract is incorrect),
**major** (a wave 1 milestone cannot be built or integrated without a trait
or contract change), **minor** (doc or additive).

**1. Blocker — the `write_seq`/group-flush contract loses acknowledged
writes.** `journal.rs` module docs, rule (2), and `Journal::submit_group_flush`.
The documented rule tells M11 to pass `upto` = the contiguous *completed*
prefix. Counter-example: queue 2 allocates seq 9 and gets `Full` (parked);
queue 1 allocates 10, appends, completes and acknowledges it; a FLUSH
arrives. With the documented rule `upto` = 8: the FLUSH completes without 10
durable, the guest's fsync returns, a crash loses 10. With DESIGN §4.1's rule
(`upto` = highest waiting seq = 10) the coordinator finds no lane with an
in-flight append ≤ 10 (9 was never accepted), syncs, completes; a crash
leaves a gap at 9 and recovery discards 10 anyway. Both readings are wrong
because a `write_seq` can exist that no lane has seen, and the journal cannot
wait for it. Worse, a write that fails after allocation (`NoSpace`, a fatal
`Io`) leaves a permanent gap that makes every later record unrecoverable,
and reusing the seq is unsafe (newest-wins ordering breaks). Also, `Full`
plus "retry with the same seq" plus "strictly rising within a lane" forces
the queue to stall every later write on that lane behind the parked one,
which nothing states.
*Fix (preferred, A):* let the lane assign the sequence. `Journal` owns the
counter (`recover` sets it from `next_seq`; add `fn last_issued(&self) ->
WriteSeq` for C1); `JournalLane::try_reserve(&mut self, data_len: u32) ->
Result<Option<Slot>>` (non-blocking; `None` = full/pool exhausted) then
`append(&mut self, slot: Slot, kind, lba, blocks, data) -> Result<Appended
{write_seq, loc}>` (infallible except a fatal `Io`, which makes the volume
read-only). `AppendRecord` loses `write_seq`; `RecoveredRecord` keeps it. The
lane registers the seq as in-flight in the same step that allocates it, so
`submit_group_flush(upto)`'s wait ("no in-flight append ≤ upto on any lane")
is exact, and the gap hazard is an internal invariant of the crate that owns
the gap rule. D7's "assigned by the caller" is wording about who increments
the atomic, not about recovery; note the discrepancy for the lead.
*Fix (minimal, B):* keep the API and rewrite the hazard paragraph: `upto` is
the highest acknowledged seq (DESIGN §4.1); the engine tracks allocated-not-
yet-accepted seqs and must not submit a group whose `upto` exceeds any of
them; a lane that returns `Full` parks every later write of that queue behind
the parked one (FIFO); `NoSpace`/`Io` after allocation is fatal for the
volume (read-only, no further appends anywhere, so the gap is at the tail).
Either way the M3 suite needs one more test: a flush submitted while a lower
seq is unaccepted completes only after that seq is accepted and synced.

**2. Major — `JournalLane::submit_read` is own-lane only.** `journal.rs`
L142-148 ("`Invalid` for a location on another lane"). Overlay hits for an
LBA were usually written by another queue (the kernel spreads a CPU's I/O
over hw queues), so almost every journal-backed READ would fall to the
blocking `read_at` on the helper pool. *Fix:* `submit_read` accepts any
`JournalLoc`; precondition: the location's append has **completed** (the
engine inserts into the overlay on the `Append` completion, which is already
hinted at in `overlay.rs` L79-82 and should be stated as the rule in both
places). Also state the buffer contract: `JournalLoc::offset` is not 4 KiB
aligned (records are 8-byte packed), so the lane reads the covering aligned
range into its own `O_DIRECT` buffer and copies into `buf`; `buf` has no
alignment requirement and may be recycled by the caller (no per-I/O
allocation needed).

**3. Major — `ExtentCodec::open` allocates per call.** `crypto.rs` L72-81,
L117. `Opened.bytes: Vec<u8>` is a fresh allocation on every read, RMW,
dedup confirmation and compaction move, while `seal` writes into a caller
buffer (`own-borrow-over-clone`, `mem-reuse-collections`, `anti-format-hot-path`
in spirit). *Fix:* `fn open(&self, kind, bytes, expect, out: &mut Vec<u8>) ->
Result<OpenedMeta { desc, chunk_id }>` (appends the payload). Two doc
sentences: for `ExtentKind::Node` the `chunk_size` field is ignored and the
cap is `lunix_format::node::MAX_NODE_ULEN`; `verify_id = true` with
`OpenOutput::Stored` decompresses internally and returns the stored form.

**4. Major — GC cannot see dead index runs.** `index.rs` L118-119: `runs()`
returns only what the manifest names. A run dropped by run compaction but
still named by a retained FULL's `index_packs[]` (M17 rule 6) is live; one
named by neither is dead and must be deleted by GC, but nothing lists the run
files on disk. *Fix:* `fn all_runs(&self) -> Result<Vec<RunRef>>` (every
`index/<run>` file present, named or not; blocking), and say in `delete_run`
that the caller checks both the manifest and the retained FULLs.

**5. Major — `JournalReserved` units are unspecified.** `space.rs` step 1
charges "record_bytes"; `journal.rs` L221-229 releases `ReclaimReport::bytes`
= "reserved capacity that became free". Ring files are preallocated at
`journal_max`, so file capacity and record bytes are different quantities;
charge in one and release in the other and the ledger drifts (M8's "exactly
zero" and M11's "ledger equals the files" gates fail at integration). *Fix:*
define both as the **encoded record length** (`align8(64 + data)`, i.e.
`Record::encoded_len`): admission charges it, the journal sums it per file,
`ReclaimReport::bytes` is the sum over the recycled files. Add the sentence to
`AppendRecord` (or `Slot`), `ReclaimReport` and walk-through step 1.

**6. Major — fsck cannot classify damage.** `journal.rs` L174-185, L196-211.
M14 step 6 needs "error if above the floor (ends recovery), warning at or
below". `DamagedRange` has no sequence context and `Recovered` has no gap
position. *Fix:* add `DamagedRange { prev_seq: WriteSeq, next_seq:
Option<WriteSeq> }` (the accepted records around the damage in that lane's
file order) and `Recovered.gap_at: Option<WriteSeq>` (the first missing seq
when `ignored_after_gap` is set). Add them now: these are plain structs that
M3 and the testkit will construct literally.

**7. Minor — `ChunkIndex::persist(upto_seg)` assumes a prefix.** `index.rs`
L111-116. "Entries of higher segments stay in memory" is only correct because
the coordinator seals compaction output and the next cut names it before the
next `persist`, so "named by a durable commit" is a prefix in id order. State
the invariant the caller must keep ("every segment with id ≤ `upto_seg` that
still exists is named by a durable commit; the C-step order of M11 ensures
it") or take `named: &dyn Fn(SegId) -> bool`.

**8. Minor — who sets `slot_seq`.** `store.rs` L294-296 "the caller sets
it" cannot work without a load first. Say the store assigns `slot_seq` =
newest + 1 and ignores the caller's value (the suite's `slot(1,4)`/`slot(2,5)`
then still passes).

**9. Minor — map contract sentences.** `map.rs` L186-187: say `walk_nodes`
reports the root node too (GC pack liveness needs the root's pack). L176-184:
say leaves are reported in ascending chunk order with holes omitted (the
suite asserts it). `crypto.rs` L54-58 node cap (see 3).

**10. Minor — two numeric edges.** `block.rs` L72-73: `IoRequest::len: u32`
and `IoStatus::ok(u32)` (saturating at `i32::MAX`) cap a single DISCARD or
WRITE_ZEROES at 2 GiB; add to `QueueConfig`/`IoRequest` docs that the ublk
layer sets `max_discard_sectors` and `max_write_zeroes_sectors` to at most
`i32::MAX / 512` (`num-cast-try-from`). `space.rs`: startup must charge the
files found on disk without a limit check; `transfer(k, k, 0, bytes)` works
by accident. Add `fn charge(&self, kind: SpaceKind, bytes: u64)` ("startup
accounting of existing files; never parks or fails") with a default that
calls `transfer`.

**11. Minor — `BlockEngine::quiesce` and `VolumeDir::dir`.** `lunixd` can
call the concrete engine for detach's final cut (it depends on
`lunix-engine`), so M12 is expressible; still, `fn quiesce(&self) ->
Result<()> { Ok(()) }` ("after the ublk layer stopped delivering requests:
final cut and commit; dropping the engine afterwards loses nothing;
idempotent") lets the daemon and M10's `lunix-ublk-testd` hold `Arc<dyn
BlockEngine>` and is additive. No `detach` method: W1/W2 belong to the ublk
layer. Add `fn dir(&self) -> &Path` to `VolumeDir` (keyring and
`integrity.key` paths for M2/M12/M13b, fsck output).

**12. Minor — M18a hooks that will not be additive later.** `EncodingDesc`
(`crypto.rs` L31-39) and `SegmentWriter::append_extent` (`store.rs` L118).
Codec 3 needs a `dict_id: u32` in the footer's `dict_ids`, and on encrypted
volumes the writer cannot read it from the ciphertext. Add `dict_id:
Option<u32>` to `EncodingDesc` now (`Invalid` unless `codec == ZstdDict`,
always `None` in phase 1) and `fn append_extent(&mut self, chunk_id, dict_id:
Option<u32>, extent)`. Same reasoning for the other growth-prone plain
structs: `Recovered`, `DamagedRange` (6), `GcParams`, `GcBudget`, `OpenExpect`;
either add the foreseeable fields now or give them `#[non_exhaustive]` plus a
constructor (`api-non-exhaustive`; the implementer's choice to keep literal
construction is fine for the stable ones).

**13. Minor — `ctl` details.** `msg.rs`: `#[serde(flatten)]` on
`RequestFrame`/`ResponseFrame` means an unknown top-level key is accepted
(serde cannot combine `flatten` with `deny_unknown_fields`); document it or
parse `v`/`id`/`method`/`params` as a plain struct and dispatch on `method`.
`ErrorCode` is closed: a newer daemon's code is `Malformed` to an older client
within v1; add `#[serde(other)] Unknown` (exit 1). `Attach.max_io_buf_bytes`
has a floor but no ceiling; bound it (≤ 32 MiB). Add `From<CryptoMode> for
VolumeMode` and the reverse (`conv-tryfrom-fallible`). Everything else in the
frame layer is right: length checked before allocation, payloads zeroizing,
`FrameError` and `RequestInvalid` never echo input, `Secret` has no `Display`
(`obs-no-sensitive-data`).

**14. Minor — crypto contract suite.** The plan lists six suites and crypto
is not one, so skipping it meets the gate. Still, M9/M11/M17 are built against
`PlainCodec` and swapped to M2 in M12; a 6-test suite (seal/open round trip
per codec, any flipped byte ⇒ `Auth`, `MetaCodec` name/kind binding ⇒ `Auth`,
`seal_commit`/`open_commit` round trip, `verify_chain` on a two-link chain and
on a broken `parent_hash`, `chunk_id` stable across two seals) is cheap and
additive. Recommend adding it with B2b (the lead lands the macro).

**15. Minor — `tools/ci.sh` request: accept.** Add `cargo test -p
lunix-iface --features crashpoints` and `cargo clippy -p lunix-iface
--all-targets --features crashpoints -- -D warnings` (the feature-on code is
otherwise never linted in CI). Lead's change to M1.A's tools.

**16. Minor — `open` order text.** `crypto.rs` L108-111 says tag before id
checks; M2 step 4 says header sanity including `chunk_id` before the AEAD
tag. Outcomes agree (both `Auth`); align the sentence with the plan or state
that the order is unobservable.

**17. Minor — `completed_watermark` equivalence.** `journal.rs` L266-269:
"equivalently each lane's `completed_watermark() >= upto`" is false when a
lane's highest accepted seq is below `upto` with nothing in flight (its
watermark is that lower seq). Keep the primary wording ("no in-flight append
with `write_seq <= upto`") and drop the "equivalently" clause, or define the
watermark as "lowest in-flight seq − 1, or `WriteSeq::MAX` when idle". Moot
under fix 1A.

**18. Minor — step 12 not done, not reported.** M1.B1 step 12 ("fill every
skeleton crate's `Cargo.toml`") conflicts with the ownership rule (B1 owns
only `crates/lunix-iface`), and the report's item 7 notes M2 lacks `lz4`/`zstd`.
Report it under "Skipped" and let the lead add the dependencies to the
skeletons on `main` before wave 1a starts (one `deps:` commit, no mid-wave
rebase).

## The implementer's open choices

| # | choice | verdict |
|---|---|---|
| 1 | `AppendRecord` instead of `RecordHeader`; `JournalLoc` addresses data, `advance` for sub-ranges | accept (under fix 1A `write_seq` becomes an output) |
| 2 | `submit_read(loc, Vec<u8>, token)`, buffer returned in the completion | accept the ownership; fix the lane restriction and state the copy/alignment rule (2) |
| 3 | `FlushDone` callback, `open_lane(lane, WakeFd)`, `tail_after`, `fill` | accept; the callback must only push and signal (documented) |
| 4 | `Completer::tag_buf` | accept |
| 5 | `CommitStore::delete(seq, chain_floor)`, `set_aside` | accept |
| 6 | `read_extent` raw, one verification in `open` | accept (M7's gate line is met by composition; say so in M7's brief) |
| 7 | `OpenExpect`/`OpenOutput`, decompression in M2 | accept the placement (M2's gate already round-trips every codec); change the signature (3) |
| 8 | `MapTxn` batch, `walk_leaves_skipping`, `NodeSource::load_node` | accept (9) |
| 9 | concrete `Waiter`, `reserve(kind, bytes, wake)`, `Headroom` pseudo-kind, `tick` | accept; `trait-dyn-vs-generic` reasoning is right (10 adds `charge`) |
| 10 | `IdAllocator`, `sweep_orphans`/`SweepReport`, `leftovers` | accept |
| 11 | `GcControl` as the single-caller funnel | accept; no deadlock: the ladder waits for the coordinator, never the reverse |
| 12 | `ctl` envelope with `v`, hex `Secret`, strict requests, tolerant responses | accept (13) |
| 13 | two `crashpoint!` definitions selected by `cfg(feature)` in iface; `inventory`; `LUNIX_CRASH_COUNT` | accept (`macro-rules-hygiene`, `$crate` paths, `proj-feature-additive` all hold) |
| 14 | contract macros over plain generic functions and `*Fixture` traits | accept (`macro-prefer-functions`: the macro only wires the constructor) |
| — | no crypto suite | see 14: add a small one, not a merge condition |
| — | no `BlockEngine` detach/quiesce | see 11: add `quiesce` with a default; no `detach` |
| — | `ci.sh` addition | accept (15) |

## API quality notes

- **Hot paths:** `reserve` allocates only when parking; `lookup`, `poll`,
  `read_extent`, `seal` and `tag_buf` write into caller buffers; `try_get_cached`
  and `Hint` are `Copy`. The exceptions are `open` (3) and, by omission,
  `submit_read`'s unstated copy (2). `FlushDone` boxes once per flush group,
  acceptable (`closure-fn-trait-bounds`: `FnOnce` is the least restrictive).
- **`Send`/`Sync`:** match the threading rules: `JournalLane`, `MapTxn`,
  `SegmentWriter`, `PackWrite`, `Collector`, `LadderStep` are `Send` only;
  shared traits are `Send + Sync`; `QueueHandler` has no bound and is built
  on its thread (`conc-`, `own-arc-shared`). `GcView<'a>` is `Copy` over `&dyn`.
- **Errors:** one `non_exhaustive` `Error` with `&'static str` payloads (no
  allocation, nothing from disk echoed), `kind()` for matching, `errno()` for
  the device (`err-thiserror-lib`, `err-lowercase-msg`, `err-doc-errors`
  sections present). `Error::Format` folds into `ErrorKind::Corrupt`: fine.
- **`non_exhaustive`:** right on `Error`, `ErrorKind`, `FrameError`; closed
  protocol enums are deliberate; see 12 and 13 for the structs and
  `ErrorCode`.
- **Control bounds and redaction:** `MAX_FRAME` 1 MiB checked in `frame_len`
  before allocation, `MAX_SECRET_LEN` 4096, names 64, paths 4096, CPU list
  256, message 512 chars, `Secret` zeroizing with redacted `Debug` and
  constant-time `PartialEq`, `Keys` redacted `Debug`, `Zeroizing` payloads
  both ways, `serde_json` errors discarded (they can echo input). Good.
- **rust-skills conflicts the report lists** (`missing_docs` not a lint,
  `expect` in suites, closed plan enums) are accepted; the project's
  conventions win.

## Summary for the lead

Fix 1 (A preferred) before cutting M3 and M11; fixes 2-6 before any wave 1a
branch; 7-18 can land with the same commit or as additive follow-ups. After
1-6 the walk has no row with a missing method.
