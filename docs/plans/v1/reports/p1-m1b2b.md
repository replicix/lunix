# M1.B2b report: `lunix-testkit` part 2

- Milestone M1.B2b, branch `p1/m1-b2b`, rebased on `origin/main` `9d0671a` (M7 and the M2 review are in it), 2026-11-12.
- Code commits `e94b946` (everything), `470f73b` (MemStore aligned with the merged `lunix-store`), `fed2595` (MemStore open observes
  the checkpoint `id_high`); this report is the commit after them.
- Gate run through `tools/remote.sh run`, `LUNIX_CPUS=0-3`, `LUNIX_JOBS=4`, host dirs `lunix-p1/m1b2b/{src,target}`. No root.

## What was built (`crates/lunix-testkit`)

| item | file | notes |
|---|---|---|
| `PlainCodec` upgrade | `plain.rs` (`digest.rs` deleted) | chunk id = `BLAKE3(plaintext)` for data and nodes (D11); tag = BLAKE3-128 over every byte before it; `open` decodes raw, **LZ4** (raw block) and **zstd** for `OpenOutput::Plaintext` and for `verify_id` (also on `Stored` output), capped at the chunk size (64 KiB for nodes), `Corrupt` for an undecodable or short frame, `Unsupported` only for codec 3 |
| `MemJournal` | `journal.rs` | the frozen `Journal`/`JournalLane` contract, see below |
| `MemIndex` | `index.rs` | memtable over runs, manifest, condemn, relocate, drop, catch-up, rebuild |
| `MemStore` (+ `MemIds`, `StoreImage`, `MemStoreConfig`) | `store.rs` | `SegmentStore`, `PackStore`, `CommitStore`, `CheckpointStore`, `PinSource`, `GcStateStore`, `VolumeDir`, `IdAllocator` on one cloneable handle |
| `Faults` / `Fault` | `faults.rs` | named fault points shared by `MemStore` and `MemIndex` |
| `MemBlockEngine`, `TestCompleter`, `run_request` | `block.rs` | `BlockEngine` + `QueueHandler` over a sparse in-memory device; immediate, deferred or seeded-mixed completions |
| `DeviceOracle`, `OracleLog` | `oracle.rs` | flat `Vec<u8>` reference device; operation log with prefix-consistency search for the crash tests |
| `FakeCtlServer` | `ctl.rs` | control-protocol server on a Unix socket for M13 (in the plan's B2b list; the brief did not name it) |

### `MemJournal`, against the frozen contract

- **`write_seq` at `try_reserve`**: allocated and registered in flight in the same locked step; `Full` (no free ring file, or `pool_depth`
  appends unfinished) and `NoSpace` consume nothing; `last_issued` is the highest handed out.
- **Group flush keyed on `upto`**: a coordinator thread runs one group at a time, in submission order; it waits until no reserved slot
  and no unfinished append with `write_seq <= upto` is in flight on any lane, then makes **every completed record durable** (the sync is
  per lane, so records above `upto` that are complete become durable too, as `fdatasync` would). `done` runs on the coordinator
  thread, or inline in `submit_group_flush` for an immediate `Err(Io)` (poisoned) and never inside a lane call.
- **Poisoning**: a failed `append` (`fail_next_append`), a failed completion (`fail_next_completion`), a failed sync (`fail_next_flush`),
  mismatched data, a dropped slot (detected at the next `try_reserve`) and a foreign slot all poison at the failed number (the lowest wins).
  Flushes with `upto >=` it end `Err(Io)`, pending ones at the moment of poisoning, `try_reserve` and `append` return `Io`; a flush below it
  is unaffected. An outstanding slot is waited for by every covering flush, never skipped.
- **Recovery** is floor based and read-only (so idempotent): merges lanes by `write_seq`, stops at the first gap from `floor + 1`,
  reports `ignored_after_gap`, `gap_at` (always equal to `next_seq`) and `damaged` ranges with `prev_seq`/`next_seq`; sets the counters
  behind `last_issued`/next number. Records at or above the new `next_seq` that stay on disk are **retired when the first lane is
  opened**, otherwise new appends would reuse their numbers and a later recovery would see duplicates.
- **Crash model** (`crash`, `crash_with(seed, CrashModel)`): freezes the journal (later calls `Closed`, pending flushes `Err(Closed)`),
  keeps durable records, and for every other record (unflushed or never completed) picks keep, drop or tear at random. Torn records
  stay as damage that recovery skips by the resync rule; dropped ones leave gaps. `from_image` reopens the image on fresh lanes
  (a new file per lane, so a torn tail is never appended to).
- **Controls**: `set_manual_completions` + `complete_appends` (hold completions to test in-flight rules), `held_appends`, `set_nospace`,
  `is_poisoned`, `unflushed_records`, `config`.
- Ring files are modelled (`file_bytes`, `max_files`), so `tail_after`, `reclaim` (never the current file; the report's `bytes` is the sum of
  `record_bytes`), `fill` and `Full` behave as the contract describes; reads of reclaimed locations are `NotFound`
  (`submit_read` completes with an error status), reads before the append completed are `Invalid` (the contract precondition).

### `MemIndex`

`persist(upto_seg)` writes the dirty entries with `seg <= upto_seg` as one run (id from the shared allocator, named in the manifest at
once); relocated entries to higher segments stay in memory. Condemned flags and dropped segments are durable at the call. `image()` /
`from_image()` model a crash: the memtable is lost, the live table is rebuilt from the named runs, hits are `Hinted`; everything
inserted in-process is `Verified`. Fault points `index.persist.{before_run,before_manifest,after}`, `index.condemn.before`,
`index.drop.before`, `index.delete_run.before` (`before_manifest` leaves a run file that `all_runs` lists and `runs` does not).

### `MemStore`

Validates what `lunix-store` validates (fixed extent prefix, id equal to the one passed, `clen`/`ulen`/codec agreement, dictionary id only
for codec 3, empty containers refused, reversed `write_seq` range refused), builds real `FooterBody`s (sorted entries, `seg_hash`,
`dict_ids`) and sizes files from the real footer encoding, `locate` returns a real descriptor (an anonymous file made on demand). Aligned
with the merged M7 `LocalVolume`: sweep refuses a named id above the mark and feeds `highest_id_before` to the allocator, `read_extent`
compares the stored `clen` with the hint, `store` refuses and `load` ignores another volume's checkpoint slot, `load_checkpoint_hint` and
`tails_usable` (inherent, same shape as the real ones), open writers are not leftovers, a dropped writer vanishes at once, `open` observes
the checkpoint `id_high`.

**Crash model**: `crash(seed)` returns the durable image; each open writer leaves an `*.open` leftover at random, which `leftovers()`
reports and `sweep_orphans` removes. `close()` is a clean shutdown. After either, the old handle is dead (`Io`). **Crash points**: a
real `crashpoint!` kills the process, which a fake cannot do inside a test, so `Faults` marks the fake dead instead: `arm(point, nth,
Fault::Crash)` makes that call and all later ones fail `Io`, and the durable image holds what a kill there would leave (`*.before`: the
step did not happen, `*.after`: it did, unacknowledged). `Fault::NoSpace`/`Io` are one-shot errors with nothing changed. Points:
`segment.append`, `segment.{seal,delete}.{before,after}`, `pack.{seal,delete}.{before,after}`, `commit.{put,delete,set_aside}.{before,after}`,
`checkpoint.store.{before,after}`, `condemned.store.{before,after}`, `evidence.put.{before,after}`, `volume_json.store.{before,after}`,
`sweep.before`; `faults().hits()` gives the coverage counters M11 needs. Also: `set_capacity` (`NoSpace`), `set_pins`/`fail_pins`
(`PinSource`), `tear_newest_checkpoint`, `add_leftover`, `corrupt_commit`.

### `MemBlockEngine`

An engine for the ublk layer (M10) to be tested against and the worked example of the `QueueHandler` contract: sparse device, `EINVAL`
for unaligned/out-of-range/zero-length/over-`max_io_buf_bytes` requests, zeros for unwritten blocks, discard and write-zeroes zero,
FLUSH/FUA trivially durable (the engine has no crash model: M11's engine is tested over `MemJournal` and `MemStore`, not over this).
`CompletionMode::{Immediate, Deferred, Mixed(seed)}` chooses whether a request completes in `on_request` or from `poll` after the queue's
wake fd was signalled (deferred reads are copied into `Completer::tag_buf` from `poll`). Also `set_read_only` (writes `EIO`),
`fail_next(op, errno)`, grow with the resize hook (`Unsupported` for shrink, `Invalid` unaligned), `quiesce`, `stats`, `snapshot`.
`TestCompleter` and `run_request` drive a handler the way the ublk layer does.

### Device oracle

`DeviceOracle::apply(&IoSpec)` answers `Applied::{Invalid, Read, Done}`. `OracleLog` records the accepted mutating requests (and grows)
and `first_match(actual, min_n)` finds the smallest prefix `n >= min_n` whose state equals the device, in one replay plus the size of each
later request: M11's check is "`first_match(device, last_flush_n)` is `Some`".

## Gate (host, tip of the branch)

| gate item | result |
|---|---|
| every `ci.sh` step: tool tests, fmt, clippy `-D warnings` (workspace; `lunix-iface` and `lunix-store` with crashpoints), workspace tests, iface and store tests with crashpoints, feature edges, `lunixd` crashpoints build | green at `fed2595` (678 tests passed across the steps, no retry needed in the last run); ownership `m1b2b ok (25 changed paths)` |
| `lunix-testkit` tests | 191 pass, 0 fail (list below) |
| every fake passes its `lunix_iface::contract` suite | `MemJournal` 13 tests x2 (two ring-file sizes, 26), `MemIndex` 10, `MemStore` 12 (+ `FlatMap` on `MemStore`'s packs 7), `PlainCodec` crypto suite x2 modes with raw, LZ4 and zstd (18), plus B2a's overlay, map, space suites |
| `DeviceOracle`/`OracleLog` self-consistency, 1 000 sequences of 100 requests | `model_oracle`: oracle vs a sparse block model (valid and invalid requests), log replay and prefix search (1 000 cases each) |
| `MemBlockEngine` through `QueueHandler`, 10 000 random requests vs the oracle | `mem_engine::ten_thousand_requests_match_the_oracle_immediate` (valid and invalid requests; reads, final device and every status compared); `two_thousand_..._deferred` and `_mixed` run the same driver in the other completion modes |
| journal crash model | `mem_journal`: 300 crash seeds produce gaps (>20), damage (>20) and clean prefixes (>5); recovery is a contiguous prefix containing everything a completed flush covered (200 random workloads on 3 lanes); a refilled gap retires the stale records; 600 appends from two threads |
| store crash points | `mem_store`: crash before/after seal, one-shot faults, capacity, leftovers by seed, ids after crash |
| index | `mem_index`: faults around persist, orphan runs, shared allocator, real footers from `MemStore`, model of "memtable over runs" (300 cases) |

Tests by file: `contract_index` 10, `contract_journal` 26, `contract_map` 8, `contract_overlay` 7, `contract_plain` 18, `contract_space` 8,
`contract_store` 19, `fake_ctl` 3, `flat_map` 6, `mem_engine` 7, `mem_index` 12, `mem_journal` 18, `mem_space` 7, `mem_store` 19, `model_map` 1,
`model_oracle` 3, `model_overlay` 1, `plain_codec` 11, `strategies` 7 (the digest unit tests are gone with `digest.rs`).

## Deviations and open choices

1. **`MemStore` embeds nothing and replaces nothing.** `MemPacks` stays (B2a's tests and `FlatMap`'s fixtures use it); `FlatMap` passes the map
   contract on both (`contract_store.rs` runs it on `MemStore`'s packs). `MemPacks`' docs now say so.
2. **Integrity-mode metadata MAC is keyed BLAKE3**, not HMAC-SHA-256 (the testkit has no `hmac`/`sha2`), and the integrity-mode extent tag
   key is `blake3::derive_key("lunix testkit extent tag", test_key)`. Plain-mode extents, chunk ids, commit object hashes and plain-mode
   objects are byte-compatible with `lunix-crypto`'s plain mode (unkeyed BLAKE3); integrity-mode bytes are not. Fakes only have to agree
   with themselves.
3. **Node ids are no longer in a separate domain from data ids** (B2a had one): D11 gives both `BLAKE3(bytes)`. The B2a assertion was replaced.
4. **`PlainCodec::open` order**: tag, structure, id checks, then decompression/`verify_id`; `out` stays untouched on every error.
5. **Stale journal records are retired at the first `open_lane` after `recover`, not at `recover`**, to keep `recover` read-only and
   idempotent. A real journal needs an equivalent (file generation or truncation): without it, records beyond a gap come back to life
   when the gap is refilled. Worth a line in M3's brief (see "Interface notes").
6. **`recover` after a lane is open is `Invalid`**; the trait only says to call it before. `hint` is accepted and ignored.
7. **`MemJournal` flush callbacks run on the coordinator thread**; M11's callbacks must not assume the queue thread.
8. **`MemStore::dir()` is the label `/mem`**; nothing exists on disk. `locate` uses `tempfile::tempfile()` (already a dependency).
9. **The id allocator of a reopened `MemStore` starts at the highest id on disk and the checkpoint `id_high`**, as `LocalVolume::open`
   does; an abandoned `*.open` id is reissued unless the caller observes it. This keeps M11's startup honest.
10. **`MemIndex::catch_up` drops entries of every segment whose footer is `NotFound`, not only of listed ones** (a deleted segment is
    never in the chain list, which is exactly when its entries must go). Dropped segments are tombstones in the manifest until `rebuild`;
    run compaction is not modelled. `delete_run` does not check the manifest or the FULLs (the contract makes that the caller's job).
11. **Engine timings**: debug-build 10 000-request runs take about 150 s for the three modes together on 4 cores, so only the immediate
    mode runs 10 000 requests; deferred and mixed run 2 000. The oracle proptests (1 000 x 100) take about 20 s.
12. **`pattern()`** in `strategies.rs` was rewritten with `as_chunks_mut` (same output, several times faster in debug builds); no test changed.
13. **Mutation checks were not run this time** (B2a ran two). The contract suites are the iface's; the own tests assert the behaviours above.

## Interface notes and findings for the lead

- `SegmentStore` and `CommitStore` both have `list` and `delete`; a type that implements both (`MemStore`, `LocalVolume`) needs qualified
  calls (`SegmentStore::list(&s)`). M11/M12 should hold the traits as `Arc<dyn ...>` fields as the iface already suggests.
- `Journal::recover` leaves valid records beyond a gap on disk and `last_issued` below them; see deviation 5. The iface doc could say who
  invalidates them.
- Not an M1.B2b item, but it blocks a green `ci.sh`: **`lunix-store` `tests::damage::damaged_map_packs_are_detected_too` is flaky on the
  host**: `open: Busy { what: "volume lock" }` at `damage.rs:238` in 2 of 3 isolated `cargo test -p lunix-store --lib` runs (and in both
  `ci.sh` runs of this branch), on code this branch does not touch; the last full run passed it. Probably the test opens the volume
  while a lock-test child or an earlier handle of the same directory is still alive. The gate run that is reported above used a
  local copy of `ci.sh` (not committed) that excludes `lunix-store` from the workspace test and retries its two test runs up to three
  times; no retry was needed in it.

## rust-skills conflicts

- `err-no-unwrap-prod`: library code uses `expect` only for invariants (thread spawn, fixed-width slices, ids from the allocator);
  tests use `unwrap`/`expect` freely.
- `doc-all-public`: public items and modules are documented; `missing_docs` is not enforced workspace-wide.
- `unsafe-*`: none (`#![forbid(unsafe_code)]`; descriptors come from `tempfile`, `OwnedFd::from`).
- `own-mutex-interior` / `async-no-lock-await`: std `Mutex` + `Condvar` only, no lock is held while a callback runs.
- Plan/CONVENTIONS ground rule 1 (do not commit) versus the implementer rules (signed commits on the branch): followed the latter.

## Commits

See `git log origin/main..p1/m1-b2b`: the code commits above and this report.

## Remote disk use

`~/lunix-p1/m1b2b` is 7.0 GiB in total (target dir included), under the 10 GiB budget.
