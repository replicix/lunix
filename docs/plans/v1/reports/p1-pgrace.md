# p1-pgrace: the short GC grace for segments under space pressure (ADR-32 amendment, FREEZE F3)

Branch `p1/pgrace`, cut from `origin/main` a95c5828; ownership entry `[pgrace]`. The design is the lead's note
`pressure-grace-design.md`; the maintainer approved the rule as worded in its sign-off section (segments only; each
segment's own flag starts its clock; `gc_pressure_grace_ms` 0 or 2000 to 10000; the recheck the only judge; packs, runs
and commit objects keep the floor; `read_timeout` not enforced; no on-disk change) and chose an ADR-32 amendment plus
FREEZE rule change F3. Lead decisions: S3 existing condemns covered from their own flag, S4 range 2000-10000, S5
`read_timeout` not enforced and narrowed in the docs, S7 the existing evidence reason (no new phrase), S8 default off.
Builds and tests ran only on the remote host (kernel 7.0.0), CPUs 0-15, `-j 16`, next to the `p1/gracepre` kill loops.
Commits: M1, M2, M3 and the docs, in that order, signed, no trailers.

## M1: reader groundwork, no behaviour change

- **fd cache (row 12) reproduced, then fixed.** `open_container` now reads the link count of the new descriptor
  under the cache lock and does not insert an unlinked file. A `#[cfg(test)]` hook in `open_container` (between the
  validated open and the insert) lets a test delete the segment there. With the check disabled
  (`linked = ... || true`) the test fails (`the cache must not hold the deleted segment`: the unlinked file stays in
  the cache and in `/proc/self/fd`); with it, the read returns the right bytes, the cache has no entry and no
  descriptor of the deleted file is left. A second test: a `locate` taken before a delete reads the unlinked file and
  closes it on drop.
- **Contract tests (settle the two inferred points; neither contradicts the note).**
  `lunix-iface` store contract `a_location_outlives_the_delete_and_new_reads_are_not_found`, run on the real store
  (plain, integrity, encrypted), the testkit store and the trivial store: a location taken before `delete` reads the
  extent's bytes afterwards; `read_extent`, `locate` and `footer` after it are `NotFound`. The trivial store's
  `locate` returned `/dev/null`; it now returns a memfd holding the extent region (a descriptor that can be read, as
  for the other two). Index contract `delete_run_never_fails_a_lookup_in_flight` (every fixture) and, on the real
  index with compaction's dead runs (`behaviour.rs`), `delete_run_does_not_fail_or_change_a_lookup_in_flight`: four
  readers loop over point lookups, prefix lookups and full scans while every dead run is deleted: no error, the
  same answers throughout. The reason it holds: a lookup holds the run list under the read lock and `delete_run`
  needs the write lock before it unlinks; a scan holds each run by `Arc` (its open file).
- **Reader-age counters.** `ReadJob::born`, the age of a hint that went stale: when a part's read fails `NotFound`
  and the block's entry changed, `now - held_since` is recorded in two atomics published as
  `EngineStats::read_stale_age_min_ms` / `_max_ms` (0 and 0 without a sample). `held_since` is the stamp of the
  first part submitted for a request (cache hits and holes pay nothing), the helper's own lookup for a part without
  an entry, and the retry time after each retry (a retried part holds its new hint from then). They are fields of
  `EngineStats` but **not checkpoint slots**: `SLOTS` stays 26, `to_slots`/`from_slots` ignore them, so a restart
  does not invalidate the checkpoint hint and clears them. `lunix status` prints `stale hint age`. Test: the age is
  exactly the manual clock's advance.
- Gate: `tools/ci.sh` green on the M1 tree.

## M2: the rule

- `Collector::set_segment_grace(Option<Duration>)` (defaulted). `Gc`: `seg_grace` (stored only while the engine's
  pressure is set; a short grace below `GcConfig::segment_grace_min`, default 2 s, is raised to it; tests set 0),
  `grace_of` (segments: `min(short, grace in force)`; every other kind: the grace in force), `urgent_grace_of`
  (floor base), `last_mark_seg_grace` for the shrink-at-onset test per object kind, used by `grace_ended_since_mark`,
  `fast_due`, the pipelined and whole delete phases and the urgent pass. `recheck_verdict` is untouched. The whole
  sequence's `Grace` phase still waits `params.grace` (the note keeps it; the daemon runs the pipelined schedule).
  `dry_run` uses no grace today, so there was nothing to change there.
- Engine: `EngineConfig::gc_pressure_grace`, `Pressure::segment_grace`, one call next to `set_pressure` in
  `with_collector` (steps, requested and urgent passes, proposals and dry runs all pass through it); the ladder's
  back-off bound is the short grace plus 1 s (`condemn_wait`). Daemon: `gc_pressure_grace_ms` (0 = off, else
  2000..=10000, otherwise a parse error), attach warning without `gc_fast_recheck`.
- Metrics: `GcStatusSnapshot::segment_grace_ms`; `GcReclaimStats::{pgrace_deletes, pgrace_bytes,
  pgrace_min_wait_ms, pgrace_uncondemned}` (a segment whose flag was younger than the grace floor at the start of the
  judging mark). The `gc: the grace in force changed` line carries `segment_from_ms` / `segment_to_ms`.
  `lunix gc --status` prints `segment grace` and `short grace` lines.
- Tests: `lunix-gc/src/tests/pgrace.rs` (deferred 1 ms before `t + g` and deleted at `t + g`; scope; no pressure no
  effect; onset; pressure ending before the verdict; validation still refuses a grace below the floor; the
  collector's minimum; urgent pass; counters and the unchanged reason; wrong proposals un-condemned; restart),
  **negative controls**: `control_scope_fails_if_packs_and_commits_get_the_short_grace` and
  `control_onset_fails_if_the_clock_starts_at_the_pressure_onset` (a thread-local `PgraceMutant` makes the collector
  give packs/commits the short grace, or start a segment's clock at the onset; the scenarios must then fail with the
  expected message) and `control_pgrace_unfiltered_oracle_fails_without_the_pin_roots_in_the_recheck`; oracles
  `oracle_pgrace_*` (grace 0 and 1 ms, the lean `id_len` 0, unfiltered proposals with the recheck alone judging, I-R's
  `live_bytes == 0` assert live); the crash matrix `every_crash_point_with_the_short_grace_*` (16K, 64K, lean, with
  proposals); `lunix-engine/tests/pressure.rs` (the exact value per state, none 5 s after the gate opens, none without
  the key, the urgent pass is told); `lunix-engine/tests/revalidate.rs` property test over the real collector,
  manual clock, grace in {0, 1 ms, 2 s}, the six modes, ops Write, Discard, Read, Held, Cut, GcStep, Pressure,
  Drain, Tamper: P1 each block is a version it could see (zeros only for a hole), P2/P3 `extent_auth_failures` equals
  the tampers, no read fails, `read_revalidate_exhausted == 0`, P4 `read_stale_age_min_ms >=` the grace, P5
  `read_revalidated >=` the parked reads that lost their segment (the property test compacts nothing:
  `rate_bytes_per_s = 0`).

## M3: stress and kill loops

- `lunixd/tests/stack_pgrace.rs`: the real stack held in slowdown (96 MiB cap, a 4 MiB hot set rewritten with
  256 KiB writes, `gc_grace_ms` 60 s), `gc_pressure_grace_ms = 2000`, `gc_fast_recheck`, `gc_condemn_on_cut = on`;
  writers of stamped blocks, readers of random 4 KiB-aligned ranges; a block is accepted only if it is a version the
  write history allows for the request's interval (overlapping writes may land in either order). A store wrapper
  (`OpenParams::segment_read_delay`, tests only, applied to the `lunix-read-*` helper threads only) holds 5 % of the
  guest `read_extent` calls, before the read or after it. Cells: plain, integrity, encrypted, each with the default
  16-byte and the 32-byte id prefix, partial-chunk writers (16 KiB at 4 KiB boundaries, so the destage base reads
  run) in the integrity/16-byte cell, `lunix-fsck --deep` after the close; an `#[ignore]` soak (10 min).
- `stack_gc_kill.rs`: arm `pgrace` (the condemn arm's schedule, key 2000 ms, `fast_recheck` without `_always`, the
  engine's pressure forced Raised through the test hook `EngineConfig::gc_pressure_force`); the counting run asserts
  `pgrace_deletes > 0`, `pgrace_min_wait_ms >= 2000`, no exhausted revalidation and no auth failure.

Stress results (60 s cells of load, `--test-threads=2`, final tree):

```
pg-enc-id32    writes 11509 reads 243069 revalidated 63 exhausted 0 stale age 6236..16000 ms auth 0 pgrace deletes 47 least wait 2013 ms
pg-enc         writes 11524 reads 267735 revalidated 61 exhausted 0 stale age 6192..15929 ms auth 0 pgrace deletes 47 least wait 2019 ms
pg-int         writes 15215 reads 221378 revalidated 78 exhausted 0 stale age 6186..15815 ms auth 0 pgrace deletes 48 least wait 2005 ms
pg-int-id32    writes 11728 reads 258584 revalidated 68 exhausted 0 stale age 5801..15832 ms auth 0 pgrace deletes 48 least wait 2003 ms
pg-plain       writes 11699 reads 240154 revalidated 79 exhausted 0 stale age 6263..15823 ms auth 0 pgrace deletes 48 least wait 2009 ms
pg-plain-id32  writes 11710 reads 252904 revalidated 71 exhausted 0 stale age 6438..15991 ms auth 0 pgrace deletes 48 least wait 2003 ms
```

All five alarms of the note's 3.3 held in every cell: `read_revalidate_exhausted` 0, `extent_auth_failures` 0, no guest
error, `pgrace_min_wait_ms` >= 2000, `read_stale_age_min_ms` (5.9 to 6.7 s) >= the grace, every block verified, fsck clean.

## Gate

- `tools/ci.sh` green after merging origin/main (gracepre) and the read hook; the only later change is the removal of an
  assertion from `stack_gc_kill.rs` (a test file, compiled by the kill-loop rebuild, which passed) and docs.
- `LUNIX_GC_CASES=400 cargo test --release -p lunix-gc --lib oracle`: 17 passed (the existing oracles and the new
  `oracle_pgrace_*` with their controls).
- `PROPTEST_CASES=2000 cargo test --release -p lunix-engine --test revalidate`: 15 passed.
- The M3 stress: 6 of 6 cells.
- Kill loops (`LUNIX_KILL_HITS` as the file says: first hit for the plain arms, first, middle and last for the condemn and
  pgrace arms; all run in parallel on the host, `lunix-fsck` clean, oracle equal, no reference-table warning):
  plain 16K, integrity 16K, encrypted 16K, plain 64K and the condemn arm all exit 0 (`read_revalidated` 0: those arms
  have no read hook, as gracepre found). The pgrace arm (plain 16K, 30 kills over 10 points, hits 1/middle/last, and
  encrypted 16K) with the store's read hook: plain: 27 runs, 27 kills at 10 points (first, middle, last
  hit; an earlier run of the same arm with 30 kills also passed), encrypted: 27 runs, 27 kills; recovery,
  `lunix-fsck` and the oracle clean in all, `read_revalidate_exhausted` 0, `extent_auth_failures` 0, `pgrace_deletes`
  > 0 with `pgrace_min_wait_ms >= 2000` in the counting run. The loops' reader revalidated 0 parts (24 000 idle reads;
  see deviation 9).

## Deviations and open choices

1. **Lean volumes cannot be created in v1** (`create_volume` refuses `id_len 0`), so the stress cells use the 16- and
   32-byte prefixes. The lean path (no index fallback, no compaction) is covered by the engine property test (the
   six modes include `IdLen::Zero`) and by the collector oracles and crash matrix.
2. **Read holds.** The brief's 1 % of calls held 0 to 3 graces never lost a segment: a segment is deleted about 6 s
   after a read took its hint at the earliest (the table of lifetimes: the chunk is overwritten, the cut, the flag, 2 s,
   the next mark). The cells hold 5 % of the guest calls for 0 to 8 graces (16 s; `LUNIX_PGRACE_HOLD_GRACES`), the
   read cache is 2 MiB, 32 read helpers, so that holds do not starve the others. With these `read_revalidated` is
   61 to 79 per cell, always above 0.
3. **The kill-loop arm forces the pressure.** The child processes never fill a volume, and the gate's `Raised`
   needs an active writer; `EngineConfig::gc_pressure_force` (a `#[doc(hidden)]` test hook) makes the engine report
   `Raised`. The real gate is covered by `stack_pgrace`.
4. `pgrace_*` are in `GcReclaimStats` (counters), `segment_grace_ms` is a field of `GcStatusSnapshot`. The
   `pgrace` test is "wait < grace floor at the mark's start", as the note says.
5. `stack_pgrace` is not in `tools/ci.sh` (6 cells, about 5 min); the command is in the file header, as for the kill
   loops.
6. A host trap: files synced with an older mtime than the artifacts of a previous run on the host (an edit made
   while another run was already compiling) are not rebuilt by cargo; I touched all changed files before each
   sync. Not a repository problem.
7. rust-skills: no conflict with the project's conventions that I knew of. The tests use `as` casts for block
   arithmetic on small bounded values, as the neighbouring tests do.
8. **Read hook in `lunix-store`** (after the coordinator's message on p1/gracepre): `ReadHook`/`ReadStage` behind `test`
   and the `crashpoints` feature, called inside `SegmentStore::read_extent` before the open and after it;
   `OpenParams::segment_read_hook`. `stack_pgrace` and the pgrace kill arm therefore need `--features
   testsupport,crashpoints`. The first M3 commit's lunixd wrapper store was replaced by it. The stress shows
   `read_revalidated` 61 to 79 per run with `exhausted` 0 on all six cells.
9. **Kill-loop reader and C7.** The loops' reader runs in idle phases where nothing is overwritten; only compaction
   changes entries there. With the hook the reader revalidated 0 parts in the pgrace arm (27 000 idle reads: the holds
   dominate its time), so the loop only reports it (`pgrace arm: the loop's reader revalidated no read part`) and
   asserts `exhausted == 0`; `stack_pgrace` is the C7 gate.
10. **destage `read_base` (gracepre F1).** It does not revalidate through the map; a delete overlapping a cut would
    fail the cut permanently. The invariant that prevents it is I-C: a cut and a collector call both need the
    coordinator mutex and the collector needs the map at the tip. New test
    `no_collector_call_overlaps_a_cut_under_the_short_grace` (all six modes): a cut parked at its base read, the clock
    30 s on, pressure raised with the short grace in force: a collector tick does not run until the cut ends, and the
    cut reads the right base. The note's suggestion (re-lookup in `read_base`) is not done; the short grace does not
    change the ordering.
11. Merged origin/main (gracepre) before the final gate; the `[gracepre]` and `[pgrace]` ownership entries both stay.
12. Not done, as instructed: T6 (arm C and C2), the default decision (S8 stays off).

Remote disk: 38 GiB peak (target debug and release, scratch of the parallel loops), over the 10 GiB budget; the tree is removed at the end.

## Review fixes (review of f612fe2d)

- **S1** `EngineConfig::gc_pressure_force` and its use in `pressure_now` are behind the engine's `testsupport` feature:
  not in release builds. The kill-loop arm (built with `testsupport,crashpoints`) is the only user.
- **S2** The ladder's back-off bound is `condemn_wait(floor, key, short_in_force)`: the short grace plus 1 s only when
  the engine's pressure is not normal, the key is set and a listed object is a segment (new status figure
  `GcStatusSnapshot::condemned_segments`); otherwise the floor, as on main. Unit tests for both cases
  (`backoff_tests`).
- **S3** `tools/ci.sh` runs one `stack_pgrace` cell (integrity, 16-byte ids, partial-chunk writers, `LUNIX_PGRACE_SECS=40`);
  the test still asserts `read_revalidated > 0`, `exhausted == 0` and no auth failure. It took 81.6 s in the gate run.
- **Nits.** The collector's module safety argument, `urgent_inner` comments and the three grace selectors (explicit
  `ObjectRef` arms); docs (five properties, requested-pass wording, the note no longer cited as a tree path, pointers
  from the M8 row and the GC-duty bullet, the 5 s hold in the key's row, one set of revalidation figures, the kill hook's
  comment); `segment_grace_ms` is 0 unless a short grace is in force, so `lunix gc --status` prints no `segment grace`
  line with the key off; a failed directory fsync after an unlink still evicts the descriptor (`container.rs`), and a
  failing `fstat` or a file system that keeps `st_nlink` at 1 for an unlinked file is noted in the comment (the first
  does not cache).
- **Left as they are, as asked:** the re-flag stamp in `reconcile` is read before the flag write (the grace of a
  re-flagged segment can be younger than its flag by the scan and the write; the recheck mark still starts after the
  flag, and no reader depends on the length); `ReadJob::born` is stamped once per request, so `read_stale_age_*` can
  overstate the age by the lookup loop (microseconds), in the direction that hides an alarm, not raises one.
- Gate: `tools/ci.sh` green (with the new cell), oracles at 400 cases 17 passed, the pgrace kill-loop arm on plain 16K
  once: 27 runs, 27 kills at 10 points, recovery, fsck and oracle clean, exhausted 0.
