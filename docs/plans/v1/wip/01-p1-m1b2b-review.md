# M1.B2b review: `lunix-testkit` part 2

Independent review of branch `p1/m1-b2b` at `73f198a` (report
`docs/plans/v1/reports/p1-m1b2b.md`) against the frozen `lunix-iface` rustdoc
(`journal.rs`, `store.rs`, `index.rs`, `block.rs`, `crypto.rs`, `ctl/`), the
plan's M1.B2 section and the needs of M9, M10, M11, M13 and M17, and the
merged `lunix-store`, `lunix-index` and `lunix-crypto` where the fakes claim to
match them. 2026-11-12. `rust-skills` loaded. Nothing was built or run
locally; the gate was re-run on the test host from a fresh sync in its own
directory (deleted afterwards).

## Verdict: **merge after fixes**

No blocker. Two majors: `MemStore` only understands the unencrypted extent
layout, which stops M9's and M11's `AesGcm` gate items from running on it, and
`MemJournal` completes appends in order and (by default) before `append`
returns, which is the one place the journal fake is more lenient than an
io_uring lane. The rest is minor. The contract fidelity of `MemJournal`
(sequence numbers at `try_reserve`, `Full`/`NoSpace` consume nothing, the
flush keyed on `upto` that waits for an outstanding slot, poisoning at the
lowest failed number, dropped and foreign slots) is correct and tested; the
crash models are faithful enough for M11's crash tests (section "Crash
models"); `MemBlockEngine` and the oracle are a trustworthy reference for M10.

## Gate reproduced

Fresh sync of `73f198a` through `tools/remote.sh run bash tools/ci.sh`
(`LUNIX_CPUS=0-3`, `LUNIX_JOBS=4`, own host directory, deleted afterwards),
then `cargo test -p lunix-testkit`:

- tool tests, `cargo fmt --check`, `clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace` (every crate green, the `lunix-store`
  damage tests included), `lunix-iface` with crashpoints: green.
- `lunix-store` with crashpoints: **`damaged_map_packs_are_detected_too`
  failed** with `open: Busy { what: "volume lock" }` (`damage.rs:238`), which
  stopped `ci.sh` (`rc=101`). A retry of that step failed on
  `a_flipped_byte_in_trailer_footer_or_extents_is_detected` at the `open`
  of `Sealed::reopen` (`damage.rs:67`), 80 of 81 passing each time. This is
  the known flakiness on code the branch does not touch; ignored here as
  instructed. The steps after it were run by hand and are green: `lunix-crypto`
  lib lint, `check-features: ok`, `lunixd --features crashpoints` builds,
  `check-ownership: m1b2b ok (25 changed paths)`.
- `lunix-testkit`: **191 passed, 0 failed**, by file exactly as the report
  lists them (`contract_journal` 26 = 13 × 2 ring sizes, `contract_plain` 18,
  `contract_store` 19, `mem_journal` 18, `mem_store` 19, `mem_index` 12,
  `mem_engine` 7, `model_oracle` 3, `fake_ctl` 3, ...). Timings as reported:
  the 10 000-request engine run 115 s, the two 2 000-request runs 90 s
  together, the oracle proptests 19 s each.
- No host name, address or user in the logs.

## Crash models (check 2)

- **Flushed records kept.** `crash_with` never touches a `durable` record;
  the coordinator marks every *completed* record durable at a flush (all
  lanes, records above `upto` included, as `fdatasync` would). Correct.
- **Unflushed records dropped or torn.** Each non-durable record (completed
  or still pending) is independently kept, dropped or torn; a torn record keeps
  its length and becomes a `DamagedRange` with `prev_seq`/`next_seq`; recovery
  skips it and stops at the first missing number. 300 seeds produce gaps,
  damage and clean prefixes; 200 random workloads check "everything a
  completed flush covered survives". Correct.
- **Stale records above a gap.** They stay on "disk", are reported as
  `ignored_after_gap` with `gap_at == next_seq`, and are retired at the first
  `open_lane` after `recover` (deviation 5), so a refilled gap never produces
  duplicates. Accepted: `recover` stays read-only and idempotent as the trait
  demands, and a real journal has the file `gen` for the same purpose. Two
  consequences: the iface doc should say who invalidates records beyond the
  gap (lead action, M3's brief), and the fake never shows M11 a stale *older*
  record inside the current pass (a dropped record's bytes in a reused
  preallocated file are old data, not an absence); M3's own tests must cover
  that, the fake cannot (finding 13).
- **`.open` leftovers.** `MemStore::crash(seed)` leaves an `*.open` per open
  writer at random; `leftovers()` lists them, `sweep_orphans` removes them and
  counts them in `deleted_temp`; `from_image` and the sweep feed the highest id
  (leftovers included) and the checkpoint `id_high` to the allocator, as
  `LocalVolume::open` does. A `Fault::Crash` at `x.before`/`x.after` leaves the
  state a kill there would leave (checked for `segment.seal.*`, `commit.put.*`).
  Every completed create is durable at return, as the store contract says, so
  "unsynced store files" are exactly the open writers. Correct.

## Deviations (check 4)

- **2, integrity-mode tag and MAC differ from `lunix-crypto`.** Accept. The
  real tag is `blake3::keyed_hash` under a KDF-derived key and the real MAC is
  HMAC-SHA-256; matching them would pull `lunix-crypto`'s KDF into the testkit
  for no test value. Plain-mode bytes are compatible (checked against
  `extent.rs` and `meta.rs`). Already documented in `plain.rs`; M12 uses the
  real codec on both sides.
- **10, `catch_up` drops every segment whose footer is `NotFound`.** Accept.
  That is the natural reading of "drops manifest entries whose segment
  `footers` reports `NotFound`", and `lunix-index` keeps `dropped` in its
  manifest the same way.
- **6, `recover` after a lane is open is `Invalid`.** Accept; stricter than
  the trait, which only says "call before". Worth the one sentence in the
  iface doc.
- **`FakeCtlServer`** (beyond the brief, in the plan's B2b list). Accept. Real
  framing, version check, `Request::validate`, scripted answers by method,
  a request log; unknown version answered `invalid` as `ctl/mod.rs` says.

## Findings

Severity: **M** major (fix before merge), **m** minor.

1. **M — `MemStore` hardcodes the unencrypted extent layout.**
   `store.rs` `parse_extent` (`CryptoMode::Plain.extent_ovh()`),
   `read_extent` and `locate` (same). `LocalVolume` takes `ovh` from
   `meta.mode().extent_ovh()` (`volume.rs:433`, `container.rs:218`). An extent
   sealed by `lunix-crypto`'s `AesGcmExtents` is refused with "extent length
   does not match its clen", so M9's "output determinism with the `AesGcm`
   codec" and M11's R1 cases "over the encrypted one" cannot run on the fake.
   Fix: `MemStoreConfig { mode: CryptoMode, .. }` (default `Integrity`), use
   `mode.extent_ovh()` in the three places (the `clen` field stays at 48..52
   in both layouts), and one test that appends an `ExtentLayout::Encrypted`
   extent built with `lunix_format::extent::Extent { crypto: Some(..) }`.
2. **M — `MemJournal` completes appends in order and, by default, inside
   `append`.** `journal.rs` `append` (`completes_now`), `complete_appends`
   (FIFO), `completed_watermark`. The record is `completed` and its
   `JournalCompletion::Append` is queued before `append` returns, so a flush can
   finish before the engine has polled, and in manual mode completions are
   strictly FIFO. An io_uring lane completes later and out of order; that is why
   `completed_watermark` is defined as "the highest `s` such that every append
   `<= s` has completed". Engine code that treats a returned `append` as
   "reached the file", or `poll` order as `write_seq` order, passes against
   this fake. `Full` from the pool (`pool_depth`) is likewise unreachable
   outside manual mode. Fix: a `CompletionMode { Immediate, Manual,
   Mixed(seed) }` like the block engine's; `Mixed` holds a seeded subset of
   appends until a later `poll` and completes held appends in a seeded order
   (`completed_watermark` = lowest pending seq − 1, which the current code
   already computes from the queue front; make it a min). M11's model test
   should run `Mixed`.
3. **m — A flush below the poison point makes records above it durable.**
   `coordinator`: after `fail_next_flush` poisons at the lowest unflushed
   number, a later group with `upto` below it marks *every* completed record
   durable, those at or above the failed number included, and the crash model
   then keeps them. The contract promises only that such a flush completes;
   nothing above the failed number is promised. Fix: skip `seq >= poisoned`
   in the durable pass.
4. **m — `open_lane` is accepted before `recover` on a reopened image.**
   `open_lane` has no check; on `from_image` the counters are 1/0, new records
   reuse on-disk sequence numbers and `retire_from` never runs. The trait says
   "call once per lane, after `recover`". Fix: `Invalid` until `recover` has
   run on this instance (every contract and own test already recovers first).
5. **m — `reclaim` trusts the tail.** `reclaim` deletes every file below the
   given ordinal (never the current one) even when it holds a record above the
   last cut; the contract says "never touches a file holding a record above the
   tail". Fix: remember the highest `cut` passed to `tail_after` and refuse
   (`Invalid`) a tail that would drop a non-torn record above it; count
   `reclaim` calls so M11's order test ("reclaim before the commit is durable")
   has a hook.
6. **m — `CheckpointStore::load` picks the newest slot before filtering by
   volume.** `store.rs` `load`: `max_by_key` then `filter`, so a newest slot of
   another volume yields `None` instead of the older valid slot, unlike a torn
   newest slot. Fix: filter first.
7. **m — Fault points missing where M11 and M17 will crash.**
   `store_local_pins`, `store_device_record`, `clear_device_record`,
   `CheckpointStore::zero`, `remove_leftovers` have none (R4: "a crash between
   C6 and the floor write" is covered by `commit.put.after`, a torn pins write
   is not); `footer`, `read_extent`, `read_node`, `get` never fail `Io` or
   `Corrupt`, so the read-helper error path has no knob. Fix: `pins.store.*`,
   `device.store.*`, `checkpoint.zero.*`, and `Fault::Io` points
   `segment.read`, `segment.footer`, `pack.read`. Also name the pack append
   point `pack.append` (it shares `segment.append`).
8. **m — Two leftover rules.** A writer dropped after a `Fault::Crash` always
   leaves an `*.open` (`Builder::drop`); one still open at `crash(seed)` leaves
   one at random. Both are legal outcomes; make both seeded, or document.
9. **m — `MemBlockEngine` never frees a queue.** `queue_handler` inserts into
   `taken`; `MemQueue` has no `Drop`, so a ublk-layer test that rebuilds a
   handler (recovery in-process) gets `Invalid`. Fix: remove in `Drop`.
10. **m — Deferred completions are FIFO and all-at-once.** `MemQueue::poll`
    completes everything held, in submission order; a tag never stays pending
    across a `poll` while a later one completes. M10's deferred path gets the
    wake/poll mechanics but not the interleavings. Fix: in `Mixed`, hold a
    seeded subset for one more `poll` and shuffle.
11. **m — `IoSpec::is_valid` ignores `max_io_buf_bytes`.** `strategies.rs`;
    the engine answers `EINVAL` above `max_io` (`block.rs` `io_ok`) while the
    oracle says valid. Harmless now (`MAX_IO_BYTES` = the test's `max_io`);
    give `is_valid` a `max_io` parameter or say so in the doc.
12. **m — `PlainCodec::seal` ignores `desc.dict_id`.** `plain.rs`; the
    contract wants `Invalid` unless the codec is `ZstdDict`. Fix: the same
    check `MemStore::parse_extent` does.
13. **m — The journal crash model cannot produce a stale older record in the
    current pass** (see "Crash models"). Not fixable without modelling file
    reuse; record it in M3's brief so the real journal's `gen` rule gets its
    own test.
14. **m — `MemIndex` has no direct way to plant a `Hinted` entry.** M9's
    dedup-confirmation cases need a run entry naming a wrong extent; today that
    takes insert, `persist`, `image`, `from_image`. Fix: `insert_hinted(id,
    hint)` (or `plant_run`).
15. **m — `FakeCtlServer::conns` grows without bound** and `method_name` is
    derived from `Debug` output. Fine for a fake; prune closed connections and
    note the `Debug` dependency.
## Leniency table (check 1)

| fake | accepts what the contract forbids / never produces | finding |
|---|---|---|
| `MemJournal` | in-order, synchronous completions; pool `Full` unreachable | 2 |
| `MemJournal` | `open_lane` before `recover`; `reclaim` past the cut | 4, 5 |
| `MemJournal` | durable above the poison point after a later low flush | 3 |
| `MemStore` | refuses the encrypted layout (too strict, blocks gate items) | 1 |
| `MemStore` | reads and footers never `Io`/`Corrupt`; pins/device writes cannot crash | 7 |
| `MemStore` | `load` ignores the older slot behind a foreign newest one | 6 |
| `MemIndex` | none found; `insert` cannot check "segment sealed" (no store access) | 14 |
| `MemBlockEngine` | FIFO deferred completions; queue never released | 9, 10 |
| `DeviceOracle` | validity ignores `max_io` | 11 |
| `PlainCodec` | `dict_id` on a non-codec-3 extent | 12 |
| `FakeCtlServer` | none | 15 |

## Interface notes for the lead

- `Journal::recover` doc: say that records at or above `next_seq` are
  invalidated by the journal before the first append (file `gen` or
  truncation) and that `recover` after `open_lane` is `Invalid` (deviations 5
  and 6); put both in M3's brief.
- `OracleLog::first_match` needs the log in `write_seq` order: with several
  queues, M11's harness must log at `try_reserve` time, not at submission.
