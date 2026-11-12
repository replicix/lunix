# Review of M3 — `lunix-journal` (branch `p1/m3-journal` at d502454)

Independent review of the recovery scan, the wipe plan, the group flush and
poisoning, batching and io_uring, against the plan's M3 section and gate,
DESIGN §4.1 and §4.4, ADR-4 and its amendments, on-disk-formats §5 (5.1, 5.2,
5.3 and the torn-batch argument), the `lunix_iface::journal` rustdoc and its
contract suite. The gate was re-run on the remote host in a fresh directory
on a scratch copy with `origin/main` (490ebde) merged, deleted afterwards.
`rust-skills` ids are cited where they apply; plan and spec win.

## Verdict: **merge after fixes**

The recovery scan is right. For every case I could construct (a record torn
mid-batch, a zero-magic block, a hole hiding later records, ring wrap, a
reused inode, a stale `gen` or `off`, damage below and above the floor, a
wrong or stale hint, a crash in the middle of the wipe, a second crash after
recovery with new writes passing the stale numbers) the result is a
`write_seq` prefix that contains every FLUSH-acknowledged record, and the wipe
in the first `open_lane` is what makes sequence-number reuse safe: it is
durable (`fdatasync` per touched file, directory `fsync` after renames) and
ordered before any append, and a crash inside it only leaves a shorter
prefix of the same zeroing, which the next scan plans again. The group flush
cannot complete over a sequence number that was never written: the poison is
stored before the number leaves the pending set, and `group()` re-reads the
poison after the syncs, so the reserve/abort race closes on both sides. No
blocker. Two of the majors are a red gate step and an avoidable in-place
rewrite of an acknowledged block; the third is a contract consequence of the
wipe that the lead and M12 have to decide.

## Gate re-run (host, scratch copy = d502454 + `origin/main` 490ebde, `LUNIX_CPUS=4-7`, `-j 4`)

Host load average was **203-328** on 16 CPUs for the whole session (other
agents' work), so every timing below is noisy; the host's `RLIMIT_MEMLOCK` is
8 MiB. Model tests ran with `LUNIX_MODEL_CASES` unset (the default 2 000).

| step | result |
|---|---|
| `tools/ci.sh` on d502454 + main | tool tests, `fmt` pass; **`clippy --workspace --all-targets -D warnings` fails**: `clippy::err_expect` at `crates/lunix-journal/tests/recovery.rs:542` (finding 1). The last commit's test was added after the report's gate run. |
| `cargo test -p lunix-journal --features crashpoints` (whole crate, same tree) | **56 passed, 0 failed**: unit 1, align 2, contract 13, crashpoints 8, flush 8, model 4, reclaim 5, recovery 15. Model: `LUNIX_MODEL_CASES` unset, so the default 2 000 sequences (4 x 500, lanes 1-8), 420 s. Crash points: all six, every hit in the test, killed child, prefix, no flushed record lost, second crash clean. |
| `tools/ci.sh` after the one-line `expect_err` fix in the scratch copy | **`ci: all green`**, exit 0: fmt, clippy, `cargo test --workspace` (729 tests, `lunix-journal` 48 with the feature off, model 4 x 500 in 202 s), iface and `lunix-store` crash-point steps (the store's lock flake did not recur with 490ebde), crypto lint, feature edges, `lunixd --features crashpoints`, ownership `m3 ok (23 changed paths)` (22 + the fix) |
| `clippy -p lunix-journal --all-targets --features crashpoints -D warnings` | clean |
| perf, 1 lane vs `spikes/journal-bench` group (4 KiB, batch/group 64, both on CPU 4, 5 s, two alternating pairs, release builds, host ext4 root; load 203-225 during the runs) | spike 6745 / 7071 rec/s (flush p99 31.9 / 21.4 us); ours 1 lane 8032 / 6681 rec/s (sync p99 31.4 / 34.3 us): ratio **1.19 / 0.94** (rule >= 0.8), p99 ratio **0.98 / 1.60** (rule <= 1.25; one of two pairs fails, same noise band as the report's 0.78-1.52); 4 lanes on CPUs 4-7: 29102 / 25304 rec/s = **3.6x / 3.8x** one lane (rule >= 1.5). Pass on throughput and scaling; the p99 clause is undecided on a host at load 200+. |

The branch touches `crates/lunix-journal/**`, `Cargo.lock` (its own `deps:`
commit a9ac248) and the report; ownership is as the report says.

## Findings

1. **Major (gate red): clippy fails on the branch head.**
   `crates/lunix-journal/tests/recovery.rs:542`
   `j.recover(None, WriteSeq(0)).err().expect("refused")` trips
   `clippy::err_expect` under `-D warnings` (`Recovered: Debug`, so the lint
   applies; the similar line 480 escapes only because `FileJournal` is not
   `Debug`). The report's gate ran before d502454 added this test. Fix:
   `.expect_err("refused")`; run `tools/ci.sh` on the final commit, as the
   plan requires (the rerun with exactly this change is in the table above).

2. **Major: every recovery rewrites, in place, the block that holds the tail
   of the last acknowledged record.** `scan.rs::plan_lane` line 405 plans a
   wipe whenever `f_end` (end of the last kept record) is not a block
   multiple, which is the normal case (a batch's zero padding follows), and
   `sys.rs::zero_range` then does a read-modify-write of that block even when
   the bytes after `f_end` are already zero. This is the only place in the
   journal where a block containing FLUSH-acknowledged bytes is written
   again; under sector atomicity the kept bytes are identical before and
   after, but the design is otherwise append-only and this turns a power
   cut during startup into the one failure the crate must never cause on a
   device that can scramble the sector in flight. Fix: in `zero_range`
   (after reading the block) skip the write when `blk[keep..]` is already
   zero, or compute in `scan_lane` whether `[f_end, round4k(f_end))` is zero
   and plan the wipe from `round4k(f_end)` then. Add a test: recovering a
   cleanly shut-down journal issues no write to any file holding records
   (trace or `mtime`). (`anti-premature-optimize` does not apply: this is a
   durability invariant, not speed.)

3. **Major (contract, for the lead and M12): the wipe is irreversible and
   runs on the ordinary mount path.** DESIGN §4.1 step 4 and ADR-4 say
   `fsck` *warns* about valid records beyond the gap, which presumes they
   still exist. After this branch they exist only between `recover` and the
   first `open_lane` (or `reclaim`, which also calls `prepare`). A single
   damaged record above the floor (media error) therefore ends recovery
   there, and the first `open_lane` zeroes every later record, acknowledged
   ones included, before any operator or `fsck` sees them; the journal cannot
   tell that case from a torn tail (both give `damaged` above the floor and,
   with two batches in flight, intact records after it). The decision is
   right (sequence reuse needs it; a `gen` bump would break contiguity), but
   the contract must say so and the engine must act on `Recovered` before
   opening lanes. Fix: amend the proposed `recover` sentence to: "Records
   beyond the stop point stay on disk until the first `open_lane` or
   `reclaim` after `recover`, which durably invalidates them (zeroes each
   lane's tail) before any append. This is irreversible: a caller that must
   preserve them (`fsck`) calls neither. `recover` itself never writes and
   returns `Invalid` once a lane is open." Add to M12: log `ignored_after_gap`
   and `damaged` before the first `open_lane`; a mount option (`refuse` or
   `wipe`, default `wipe`) when `damaged` has an entry with `prev_seq >=
   floor` and `ignored_after_gap` is `Some`; `fsck` read-only never opens a
   lane. Add a line to DESIGN §4.1 as the report asks.

4. **Minor: `recover`'s floor precondition is now load-bearing.** A floor
   below a cut that a `reclaim` already used gives a gap at `floor + 1`
   (those records are gone), `next_seq = floor + 1`, and the wipe then
   destroys every later record on disk, acknowledged and not yet destaged.
   Phase 1 commits are FULL only (C6/C9), so the engine's floor equals the
   cut, but the rule lives only in the report. Fix: state it in the
   `recover` and `reclaim` rustdoc ("`floor` is at least every cut passed to
   `tail_after` whose tail was reclaimed"); M12 asserts it at open.

5. **Minor: `unsynced_min` is lost when a sync fails.** `coord.rs::group`
   resets `fl.unsynced_min = u64::MAX` when it takes the dirty sets; on
   failure it restores `dirty` but not the minimum. A later group below the
   poison mark (allowed by the iface) that fails again poisons at
   `min(u64::MAX, upto) = upto`, below records the earlier successful sync
   made durable. No data loss (the journal is poisoned either way), but the
   mark then lies. Fix: capture the per-lane minima and `min` them back on
   failure, and poison at `min_unsynced` only.

6. **Minor: which write path a lane uses is an accident of the host.**
   `lane.rs::new` sets `fixed = register_buffers(..).is_ok()`; the charge is
   `RLIMIT_MEMLOCK`, cumulative per user across rings and processes
   (`io_account_mem`). With 8 MiB and the tests' 4 x (1 MiB + 4 KiB) pool the
   first lane registers and the second does not, so both `WriteFixed` and
   `Write` ran in the gate by chance, while production (8 x 1 MiB per lane)
   never registers. Nothing reports the mode. Fix: expose it (a `TraceEvent`
   or a `fixed()` getter), add a test that pins each path (`pool_buffers =
   2` registers; a `JournalConfig::fixed_buffers: Option<bool>` would make it
   deterministic), and note for M12 that `LimitMEMLOCK` must be at least
   `lanes x pool_buffers x (1 MiB + 4 KiB)` for the fixed path. The plan's
   "registered files" (`types::Fixed`) are not used either; `types::Fd` is
   fine for phase 1.

7. **Minor: capacity silently exceeds `journal_max`.**
   `config.rs::files_per_lane` takes `max(2)`, so 8 lanes at the 256 MiB
   minimum preallocate 1 GiB (4x) and `validate()` accepts it; the
   operator's bound is also what §4.3's `fs_free - free_floor` has to absorb.
   Fix: `validate()` returns `Invalid` when `journal_max < lanes x 2 x
   file_len`, naming the minimum (the ">= 2 files" rule itself is right).

8. **Minor: `FileJournal::open` is not read-only.** `ring.rs::open_rings`
   creates missing files, removes `q*.j.tmp` and `fsync`s the directory. The
   report's "fsck can call `recover` read-only" holds for `recover` only.
   Fix: say so in the rustdoc of `open`; M14 may want an `open_readonly`.

9. **Minor (process): the report's fixed-buffer and gate statements.** "Fixed
   buffer mode exercised only in the fallback" is not established (finding
   6); "gate green" was for the commit before the last test (finding 1).
   Both are report corrections, no code.

10. **Minor: the model test leaks its template directory.** `tests/model.rs`
    keeps the `TempDir` in a `static TEMPLATES`, which is never dropped, so
    every run leaves a `lunix-journal-test-*` directory (eight template
    rings, about 280 MB) in the scratch base: the "crashed or killed runs"
    leftovers the report mentions are also produced by clean runs. Two from
    this review's runs are still under the host's `m3/scratch`. Fix: drop the
    templates at the end of each test (or build them per test).

## Checks that passed (so the lead need not redo them)

- **Torn record mid-batch, zero magic, hole.** `scan_file` (format crate,
  normative) stops at an aligned zero magic, resyncs after damage with
  `gen`, `off`, rising `write_seq` and the CRC checked before acceptance;
  `nonzero_end` covers records hidden behind a hole, so the wipe (`(f_end,
  max(nz_end, round4k(f_end)))` for the active file, `(4096, nz_end)` for
  every later file) removes them too; `ignored_after_gap` is then a lower
  bound, as the report says.
- **Ring wrap, reused inode.** Ordinals only rise (`take_ordinal`), free
  files always have higher ordinals than used ones (created, recycled and
  re-initialised files take the next ordinal), so file order is write order
  and `prev` carries across files; a reinit crash leaves either an old header
  over a zeroed prefix (scanned as empty, reclaimed or wiped) or a header
  that does not match its name (`bad`, re-initialised in `prepare`).
- **Wrong hint.** A hint that hides a record above the floor always makes the
  resume file a skipped one (every later file holds only higher numbers), so
  `HintUnusable` forces the full rescan; a hint with the wrong lane count is
  ignored; `usable_hint` is the D17 guard.
- **Wipe durability and order.** `prepare` runs under the `st` lock before
  `FileLane::new` and before any `reclaim`; zero writes go through the
  `O_DIRECT` fd, `sync_data` per touched file, `fsync_dir` after renames;
  the kept records are synced too (volatile cache after a process crash).
- **Flush.** `pending` is updated under the lane's flight lock in the same
  critical section as the counter, so `last_issued()` never races a
  reservation; `lanes_clear` and the dirty take both lock per lane, and a
  write at or below `upto` is reaped (and its file marked dirty) before the
  wait ends; `abort`, `fail_batch` and `Drop` all poison before removing from
  `pending`; `group()` re-checks the poison after the syncs. The
  `notify`/`waiting`/`epoch` handshake has no lost wake-up (the notifier
  takes the queue lock the waiter holds until `wait_timeout` releases it),
  with a 50 ms backstop. The real `fdatasync` failure path (`cqe < 0`,
  `submit_and_wait` error, serial `sync_data` fallback) poisons like the hook.
- **Batching, buffers, alignment.** An open batch with appended records
  always has a batch in flight (otherwise `append` submitted it), so a
  completion, the wake fd and `poll` always push it out; buffer indexes are
  held by `Batch` until reaped and leaked on a failed submit; offsets,
  lengths and buffer addresses are block multiples (`align` test confirms on
  ext4 with `O_DIRECT`); reads cover `[floor4k, round4k)` and copy out;
  `ring` is declared before `pool` so it drops first (`mem-drop-order`).
- **Reclaim guard.** `Used`, `ordinal < tail`, `inflight == 0`, `max_seq <=`
  the largest cut asked; the wild-tail test shows it.
- **No blocking call on the lane thread**: short mutexes, allocation, ring
  submit only.

## Open choices

| choice | verdict |
|---|---|
| files per lane >= 2, capacity may exceed `journal_max` | accept the rule; **fix** the silent overshoot (finding 7) |
| reducing the lane count is `Unsupported` | accept (their records could not be reclaimed; phase 1) |
| a foreign uuid gives `Corrupt` | accept (re-initialising would destroy another volume's journal) |
| space charging of the active file | accept: the ledger identity holds for rolled files, the active file's records are released when it rolls and is recycled, and after a restart the charge is rebuilt from `Recovered.records` |
| proposed iface sentence for `recover` | accept **with the amendment in finding 3** (irreversible; `reclaim` also triggers it; fsck calls neither) plus finding 4 |
| poison before unreserve (decision 2) | accept; correct and necessary |
| `append` after the poison lowers the mark (decision 3) | accept |
| batch policy (decision 4) | accept |
| `usable_hint` in the public API | accept |

## Notes for the report and M12

- `tools/ci.sh`: add the two `lunix-journal --features crashpoints` lines the
  report asks for; this review ran them by hand.
- The `lunix-store` `Busy { what: "volume lock" }` flake the report saw is
  the one 490ebde fixes; it did not reproduce here.
- The perf gate's p99 clause was not demonstrable on this host either (load
  200-330; one of two pairs at 1.60x); repeat on a quiet host before the
  number is quoted anywhere. Throughput and 4-lane scaling pass with margin.
