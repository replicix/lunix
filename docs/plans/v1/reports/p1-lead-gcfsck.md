# Lead pass report: GC and fsck follow-ups (`lead/p1-gcfsck`)

- Branch `lead/p1-gcfsck`, based on `origin/main` `5a29311`, 2026-11-16.
- Host: Linux 7.0.0, 8 CPUs (`-j 8`), `RUST_TEST_THREADS=1` (8 MiB memlock), no root.
- Sources: the N12 recommendation of the M17.A re-review, the M14 re-review (R-1 to R-3, the
  `p1/m14` minors, P-1 to P-4), the lead to-do.

## What was built, per item

**(1) N12, option (b) (plan Spec gaps N24).** GC's retained set is every FULL from the lowest
`chain_floor` any retained FULL records up to the tip, for roots and for "named by a retained
commit" alike.

- `lunix_gc::mark::retained_fulls(view)` (new, public) returns that set: the view's FULLs plus the
  FULLs below the tip's `chain_floor`, read from the commit store walking down from the floor. A
  missing commit ends the walk (after a truncation the floor commit's own floor may be gone); one
  that exists but does not open fails the mark. `mark::keep_floor(view)` is the lowest floor.
- `mark::root_set` (signature unchanged) uses it, so `lunix_gc::liveness` and every mark do;
  the collector's `Facts` (named set, retained seqs) and `observe_named` use it too.
- The test model follows independently (`Sim::gc_retained`, used by `roots`, `kept_by_rule`,
  `check_commits`, `prune_model`).
- New test `truncate_tip_then_mount_reads_every_root_of_the_predecessors_window`: a workload with
  passes until garbage is deleted, the tip set aside, a crash-reopen on the predecessor
  (`verify_chain` to its own `chain_floor`), every block of every root of the new window read
  against the model (the older root's data was overwritten by the dropped tip), then four more
  commits and passes with full checks.
- Three tests assumed the two-root window and were adjusted: `low_live_segment` commits
  `2·retain_last − 2` times after the overwrite; `rule12_ii_*` one more commit; the liveness
  recount's candidate floor is `volumes / 20` (one window more stays live on short histories).
- `gc.md`, plan M17 rule 1 and Spec gaps N24 state the rule. The plan's own spec-gap table already
  used N12 and N13, so the GC review's N12 is recorded as **N24**.

**(2) Atomic index rebuild (R-1).** `LsmIndex::rebuild` writes one new run from the footers first,
then one manifest swap replaces every run (old run files stay for GC); a crash leaves the old or
the new index, never an empty manifest. The iface rustdoc of `ChunkIndex::rebuild` now says so and
that the caller passes only durable segments; `MemIndex` follows. fsck's `--rebuild-index` passes
the good segments at or below the high-water mark and no longer calls `persist` (the entries above
it never survived the run before either). fsck: `index_missing` (warning) now covers only segments
the retained chain names ("mount's catch-up restores them"); the new error **`index_lost`** covers
segments in use that the chain does not name ("only `--rebuild-index` restores them"), and plain
`--repair` rebuilds on it.

**(3) Streaming resolution.** The distinct-chunk count uses a sorted, deduplicated vector (40 bytes
per resolved extent, freed at once) instead of a hash set; the index comparison uses sorted id
vectors with a seen bit (32 bytes and a bit per id) instead of two hash sets; the roots' entries are
freed as soon as they are resolved. `memory_distinct`: **peak RSS 81 MiB for 320 000 distinct and
400 000 mapped chunks, 266 bytes per distinct chunk** (328 before). The 1 M-chunk gate stays at
84 MiB. The tip's entries (64 bytes per mapped chunk) are now the largest term. Crate docs and
README updated.

**(4) M14 re-review minors.**

- R-2: `--truncate-tip` with `retain_last < 2` and a tip that does not authenticate is no longer
  refused flat; the walk of the previous root decides, and the action says the previous commit was
  not a retained root. Test with `retain_last = 1`.
- R-3: the index comparison is against segments GC's retained set names (retained chain plus the
  FULLs below it, N24) or the retained roots reference (no longer every commit file on disk). The
  window between the rollback floor and GC's retained set is the note **`floor_window`** (test).
  fsck also walks the FULLs of GC's retained set below `chain_floor` as roots (they are kept, so an
  object only they reach is not an orphan and `--delete-orphans` cannot remove it).
- `allow_floor_reset` is set only with `--repair` and one of `--truncate-tip`, `--reset-floor`,
  `--repair-pins`.
- A failed `set_aside` after the floor was lowered returns false (the action already records the
  lowering; no later repair runs).
- `\r` handling: already identical to `lunix-cli` (`secret_input.rs`: one trailing `\n`, a `\r`
  only together with it; a lone trailing `\r` stays). No change.
- P-1: `ci.sh` greps the identifier `allow_floor_reset` in any form outside `lunix-store/src`,
  `lunix-fsck` and `lunix-testkit`, ignoring comment lines.
- P-2: `state/floor-resets.log` in on-disk-formats §20.1 (line format) and the local-file table.
- P-3: the audit syncs `state/` on every append.
- P-4: `FloorReset::RepairPins { max_keyring_seq }` (iface change); fsck passes the `keyring_seq`
  the attach check unlocked (0 for plain), the store writes it into the fresh file. Store and fsck
  tests assert it.

**(5) Mark cost.** `gc.md` "Steps and their bounds": a leaf unit traverses the whole root, so a mark
costs about `leaves² / 32` skip-set lookups (about 125 000 leaves per TiB at 16 KiB). Plan Spec gaps
**N25**: a range walk (or cursor) in `BlockMap`, and M17.B measures a 64 M-chunk mark.

**For M17.B (shared contract).** `code.rs` gains `gc_liveness` (error) and `gc_dry_run` (note)
verbatim as agreed; `tools/ownership.toml` gives `m17b` `crates/lunix-fsck/tests/gc*.rs`.
`Facts` gains `gc_extra` and `gc_floor`; no field M17.B uses changed.

## Gate

`tools/remote.sh run env RUST_TEST_THREADS=1 LUNIX_SKIP_OWNERSHIP=1 bash tools/ci.sh` on the code
head (`322ba05`, rebased on `5a29311`): **all green**. fmt, clippy `-D warnings`, `cargo test
--workspace` 1 295 passed, 0 failed (`lunix-gc --lib` 52, the new truncate test among them), the
crashpoints lines, the `allow_floor_reset` identifier grep, the feature edges, the `lunixd`
testsupport stack tests in release with `lunix-fsck`. The first run found one stale expectation
(`lunix-testkit` `mem_index`: "rebuild starts from scratch"), fixed before the commits.

## Deviations and open choices

1. N12 is recorded as N24 (the number was taken in the plan's table).
2. fsck checks the FULLs of GC's retained set below `chain_floor` like retained roots (data must
   resolve). A volume collected under the old rule (a) would show unresolved entries for the
   oldest root; no compatibility is kept (maintainer rule 2026-11-14).
3. Stack tests after kills may now report `index_lost` where `index_missing` was allowed, if an
   engine persist failed before the naming FULL left the window (N13 of the GC review); that is a
   real loss of catch-up coverage, so it is an error on purpose.

## rust-skills conflicts

None.

## Commits

- `e46428d` lunix-gc: GC's retained set (N24), gc.md, plan N24/N25
- `368e5cd` lunix-index: atomic rebuild; iface doc; MemIndex
- `0c51cac` lunix-store/iface: P-1 to P-4
- `322ba05` lunix-fsck: N24 walk, `index_lost`, R-2, R-3, streaming, gating, codes for M17.B; ownership
- this report

## Remote disk use

12 GiB after the full CI (11 GiB of it `target/debug`), pruned to 1.2 GiB at the end.
