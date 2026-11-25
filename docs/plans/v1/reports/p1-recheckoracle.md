# p1-recheckoracle: `oracle_fast_recheck_{16k,64k}` fail at 400 cases

Branch `p1/recheckoracle` from `origin/main` fcacaf4. Builds and tests ran only on the remote test host (kernel 7.0.0)
through `tools/remote.sh`, CPUs 0-15, `-j 16`. No product code changed, no on-disk format change.

## Failure

`LUNIX_GC_CASES=400 cargo test --release -p lunix-gc --lib oracle_fast_recheck` fails in both tests with
`op N interleaved pass: ledger [segments, packs, commits, runs] != files`: only the pack figure differs, the ledger
above the files by exactly one pack (64k: 8033 against 6728, one pack of 1305 bytes; 16k: 317474 against 303658).
Proptest does not shrink the case below 300 ops (fixed-length op vector, 1600 shrink iterations); the seed is the
oracle's fixed `RngSeed` (`0x6c75_6e69_7867_6300 ^ salt`, salt 6 for 16k and 7 for 64k), so the failure is
deterministic. The 64k case, as reported (kept to the ops that matter):

* op 50 `Commit`: pack `0f` (1305 bytes) charged to the ledger; it becomes unreferenced a few commits later;
* op 99 `Faulty(44, 1, false, 4)`: fault point 44 mod 18 = `pack.delete.after`, first hit. The Whole pass deletes
  pack `0f`; the store unlinks the pack and then returns the injected I/O error (`MemStore::delete_pack`,
  `lunix-testkit/src/store.rs:872-881`). The collector keeps the pack in `uncertain` with its listed size
  (`collector.rs:1936`) and abandons the pass;
* op 102 `InterleavedPass`: `Sim::run_pass_with` steps once, the collector is `Idle` (no pass due: the 30 s
  `pass_interval` has not passed since the abandoned pass), so it returns having done nothing. The oracle counts it as a
  pass and calls `after_pass`, which asserts ledger == files. The ledger still holds the 1305 bytes of the pack.

## Root cause and classification: (b), a test and oracle modelling bug

`tests/oracle.rs` `Op::InterleavedPass` (before the fix, line 149-156) treated every return of `run_pass_with` as a
finished pass and demanded ledger == files. That is only owed by a pass that ran: a delete that fails after its unlink
leaves the bytes in the collector's `uncertain` set, and they are released only when a later pass rechecks the object and
finds it gone (`forget`, `collector.rs:1850-1857`) or deletes it again. That is by design: the ledger over-counts
(never under-counts) until the next pass, and the next pass is due as soon as the interval is over
(`collector.rs:1109`, `!self.uncertain.is_empty()`). The default config of the other oracles has a short interval, so a
pass is always due when they ask; the fast oracles use the daemon's 30 s / 20 s intervals and meet the idle case at
400 cases (the default 48 cases never draw the sequence). Not a product bug: no object is lost or kept wrongly, the
files are right, the over-count is transient, bounded by one pass interval and conservative for space accounting. No
delete-safety involvement: the object was already unreferenced and past its grace.

The pass that does run settles it: checked in the new test, and by the oracles' end-of-case `check_quiescent`, which
still asserts ledger == files at quiescence.

## Fix (tests only)

* `tests/sim.rs`: `Sim::run_pass_with` returns `Result<bool>`, true when a pass ran (a step that was not `Idle`, or
  that finished one); `run_pass` and `tests/crash.rs` drop the value.
* `tests/oracle.rs`: after an `InterleavedPass` that did not run, the oracle checks the roots only and does not count
  a pass; ledger == files is asserted after passes that ran, as before.
* `tests/recheck.rs`: `a_pack_delete_that_failed_after_its_unlink_is_settled_by_the_next_pass_that_runs` arms
  `pack.delete.after`, drives passes until a pack delete fails after its unlink, asserts the ledger differs from the
  files, that a call with no pass due returns `false` and settles nothing, and that the next pass that runs returns
  `true` and leaves ledger == files. Without the fix the test does not compile (`run_pass_with` returned `()`), and the
  oracle assertion it documents is the one that failed.

## Gate (release unless noted; remote host, CPUs 0-15, `-j 16`)

* `LUNIX_GC_CASES=2000 oracle_fast_recheck`: 2 passed (266 s). Before the fix: both failed at 400 cases.
* `LUNIX_GC_CASES=400` all seven gc oracles: 7 passed (53 s).
* New unit test and the whole `lunix-gc` lib: green (`ci.sh` runs it).
* `tools/ci.sh`: every step up to `lunixd` stack tests green; the `stack_throttle` test file fails there, **not caused
  by this change** (see below), so `ci.sh` stops at that step and is **not green as a whole**. The rest of the
  `lunixd` list (`--lib`, `stack_support`, `stack_model`, `stack_gc`, `stack_gc_ladder`, `stack_gc_rate`,
  `control_gc`, `status_lockfree`, `stack_window`, `stack_jread`, `stack_seal`), `stack_ring_recover` with
  crashpoints and `tools/check-ownership.sh` were run by hand: all green.
* `gc_kill_loop_recheck_plain_16k` (`LUNIX_KILL_HITS=all`, crashpoints, fsck with crashpoints): passed (1391 s).
  `gc_kill_loop_plain_16k` not run: no product change.

## Unrelated failure found: `stack_throttle`

`crates/lunixd/tests/stack_throttle.rs` fails in `ci.sh` (twice) and when the file runs alone (twice), one to four of 13
tests each time, a different one on different runs (seen: `with_the_band_off_the_volume_is_never_paced` at line 1021,
`a_volume_entering_the_band_is_paced_and_not_refused`, `e1_parked_writes_fail_with_enospc_...` and
`a_discard_on_a_full_volume_..._writes_succeed` at line 316, `e1_a_kill_and_a_restart_...` at line 436). Each test
passes when run alone (3 of 3). The throttle sits in `Stop` with one space waiter where the test expects `Open`.
This change touches only `#[cfg(test)]` code of `lunix-gc`, which `lunixd` does not build, so the `lunixd`
binaries equal main's; the failure is on main (not bisected). Probably contention between the file's tests (they
share the host's real time and I/O). Not investigated further; left for the lead.

## Ownership

`crates/lunix-gc/src/tests/sim.rs` and this report are added to the `fastrecheck` entry of `tools/ownership.toml`.
No collector.rs change, so no conflict with `p1/condemncut`.
