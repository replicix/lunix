# Milestone gracepre: prerequisites for a short GC grace

Base: origin/main `a95c5828`. Branch `p1/gracepre`, ownership entry `[gracepre]` (the new destage test, its test helper,
the `queue.rs` comment, the `stack_gc_kill.rs` soak counters, this report, `tools/ownership.toml`). Builds and tests ran
only on the remote test host (kernel 7.0.0), CPUs 0-15, `-j 16`. No product code changed (one doc comment).

## What was built

1. **`queue.rs` comment** (`crates/lunix-engine/src/queue.rs`, the `map_parts` doc, lines 698-701 still matched). It now
   says what C7 made true: the helper starts from the entry seen on the queue thread, and after a NotFound, Corrupt or
   Auth it looks the chunk up again and may read a newer entry (up to `REVALIDATE_MAX`), so parts of one request can
   reflect different map instants. The old claim about a replaced entry stays, with its reason.
2. **`read_base` invariant tests** (`crates/lunix-destage/tests/base_revalidate.rs`, helper
   `Rig::destage_with_index_via` in `tests/common/mod.rs`), each over all five codecs of `common::codecs` (testkit
   integrity and plain, real integrity, encrypted and plain):
   * `base_segment_deleted_after_a_move_the_index_finds_the_copy`: the map names S1, S1 is deleted (the GC after a
     grace), the index knows the moved copy in S2. The cut succeeds through the index fallback
     (`stale_hint_fallbacks == 1`), the patched chunk is the right base plus the patch, `extent_auth_failures == 0`.
   * `base_segment_deleted_with_no_copy_fails_the_cut_safely`: S1 deleted, no copy. The cut fails with `NotFound`,
     class `Permanent`, names chunk 0, stores nothing, the map is unchanged, no tamper count.
   * `a_damaged_moved_copy_is_never_patched_and_is_counted`: the only copy is damaged (integrity and encrypted codecs;
     the plain ones cannot detect a flip). The cut fails, nothing is patched, `extent_auth_failures == 1`.
   * `a_move_racing_the_base_read_is_found_through_the_index`: compaction commits the move and the GC deletes S1 between
     the cut's map read and its extent read (a store wrapper runs both at the first read of S1). Index knows the copy:
     right base, no tamper count.
   * `a_move_racing_the_base_read_with_a_lagging_index_fails_safely_today`: the same race with an index that does not yet
     know the copy. The cut fails with `NotFound`, nothing applied, no tamper count, and the map already names the copy.
   * `a_move_racing_the_base_read_is_found_through_the_map_when_the_index_lags` is `#[ignore]` (fails by design today):
     it asserts the revalidating outcome, a successful cut with the right base.
3. **Soak instrumentation** (`crates/lunixd/tests/stack_gc_kill.rs`): in every idle phase of the child workload one
   thread keeps reading random 4, 16 and 64 KiB ranges of the hot region through the driver (200 microsecond gap) while a
   second thread ticks the engine (GC steps, commits, deletes). A read error panics the child, which fails the loop. The
   child appends `read_revalidated`, `read_revalidate_exhausted` and the read count to `<progress>.rv` after each idle
   phase; the parent sums them over every child of the loop, prints `GRACEPRE_RV ...` and asserts `exhausted == 0`.

## Findings

**F1. `read_base` does not revalidate through the map; it is safe only by ordering.** `lunix-destage/src/read.rs`
`read_base` reads the entry the cut took from `map.get`. On NotFound, Corrupt, Auth or Unsupported it falls back to
`ChunkIndex::get_by_prefix`, confirms each candidate against its full id and tag, and otherwise returns the first error.
It never looks the chunk up in the map again. Evidence is the five tests above.
* No wrong data and no false tamper in any variant: a stale read is NotFound (not counted), a candidate is confirmed by
  full id and tag, and a damaged copy is counted as a real Auth. Nothing is ever merged with zeros.
* A stale base read fails the cut with class `Permanent` (`lunixd` `retry_of`: `Retry::Never`; the class table says "the
  volume needs an operator"). So under a short grace, if a GC delete or compaction move could ever overlap a cut, the
  result would not be a retry but a permanent cut failure, whenever the index does not yet hold the moved copy (a
  compaction inserts into the index after its commit; the ignored test reproduces exactly that, with the map already
  naming the copy). Today it cannot overlap: a GC step runs only between cuts with the map at the tip
  (`coord.rs`, "between cuts"), and the grace is irrelevant to that ordering. This is the design-note item; not fixed here.
* Suggested shape for the note (not done): on a data error whose map entry has changed, re-run `map.get` and retry the
  base read (bounded, like C7), and classify a stale NotFound that survives the re-lookup as a retryable class.

**F2. The soak cannot make C7 fire on a local store at the 10 s floor.** See the counts below.

## Soak

Settings, all in the existing harness: `gc_condemn_on_cut = on` (shadow in the recheck arm), `gc_fast_recheck` 1 s,
pass interval 1 s (plain arms), 10 s recheck-arm and 20 s condemn-arm regular passes with `fast_recheck_always`,
`read_timeout` 0 and the collector's floor grace of 10 s (the smallest it accepts), `LUNIX_KILL_HITS=all` (kills at the
first, a middle and the last hit of each `gc.*`, `index.retire.*`, `store.evidence.*`, `store.delete.*`,
`store.segment_delete.*` point), the seven loops run in parallel on the host.

| Loop | Runs (kills) | read_revalidated | read_revalidate_exhausted | Reads in idle phases | Reader errors |
|---|---|---|---|---|---|
| plain 16K | 74 (74) | 0 | 0 | 8,719,189 | 0 |
| integrity 16K | 74 (74) | 0 | 0 | 8,728,759 | 0 |
| plain 64K | 74 (74) | 0 | 0 | 8,378,888 | 0 |
| integrity 64K | 74 (74) | 0 | 0 | 8,746,211 | 0 |
| encrypted 16K | 74 (74) | 0 | 0 | 8,206,679 | 0 |
| recheck arm (plain 16K) | 15 (15) | 0 | 0 | 2,330,198 | 0 |
| condemn arm (plain 16K, on-cut) | 30 (30) | 0 | 0 | 3,797,307 | 0 |
| **total** | 415 | **0** | **0** | 48,907,231 | **0** |

(Every loop exited 0: fsck clean, oracle equal, no reference-table warning.)

**Revalidation never fired: 0 in 48.9 million reads over 415 kills. This soak does not exercise C7.** The reason is
structural, not luck. A revalidation needs a read part to be outstanding while its entry is replaced, condemned, graced
and deleted. The floor grace is 10 s and a local read takes microseconds, so the window cannot open without a stalled
store read, and the real stack has no read-hold hook (`ReadFaults::hold` exists only in the testkit stores that
`lunix-engine/tests/revalidate.rs` uses). What the soak does show: the reader saw no error of any kind (no EIO, no
Auth or Corrupt) while the real collector deleted and compacted for hours, so the no-stall case is clean, and
`exhausted` is 0 trivially. To make C7 fire in a soak the real store needs a delayed read (a test-only hook in
`lunix-store`, outside this milestone), or a grace the collector accepts below the read time.

**Read-heavy fio mix against the daemon: not run.** The only fio users are the root scripts `m12a-*.sh` (lifecycle,
fault and memory-paging cells); none runs the collector under space pressure, and building one is a new root harness,
outside this milestone. A fio mix would hit the same wall as above (no stalled read), so it would not change F2.

## Gate

* `tools/ci.sh` on the host: all green (`ci: all green`), including the new tests (5 pass, 1 ignored) and the
  ownership check.
* The seven kill loops above: all green.
* rust-skills conflicts: none that matter. Test code uses `expect`/`unwrap`/`panic!` as the existing tests do.

## Deviations and open choices

* `stack_gc_kill.rs` is in the ownership entry because the soak needed a reader in the child; the workload's write
  plan, the kill points and every assertion are unchanged, so crash-point hit counts are as before (they depend on
  timing only, which the existing retry-at-last-hit logic already tolerates).
* The reader replaces the progress line of the tick thread with one written after each idle phase (the driver is not
  `Send`, so the reader runs on the main thread and the ticker on a second one); a kill inside an idle phase leaves an
  older progress line, which is a weaker (still valid) durable bound.

## Remote disk use

24 GiB peak, over the 10 GiB budget: the cargo target (debug for `ci.sh`, release for the stack tests and the loops) plus the seven parallel loops' scratch. All of it removed with the tree at the end.
