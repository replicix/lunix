# Milestone throttleflake: the flaky `stack_throttle` tests

Base: origin/main `61a06eec`. Branch `p1/throttleflake`, ownership entry `[throttleflake]` (the test file, this report,
`tools/ownership.toml`). Builds and tests ran only on the remote test host (kernel 7.0.0), CPUs 0-15, `-j 16`.
Test-only change; no product code, format or interface touched.

## Verdict

**Test-only, not a gate bug.** The tests assert instants that the engine's cut cycle decides, on a volume whose cap is
tight (64 MiB, 44 MiB written, a cut of 16-40 MiB). They never shared state: every test has its own `Scratch`
directory, and the host free space is not in play (the host had 250 GiB free; the limit is `cache_max`, 64 MiB).
No waiter was lost: every `Stop` ended at the next C9 and every write completed `Ok` (the fixed test asserts it).

## Reproduction (original tests, 13 tests per run, `--test-threads=13`)

| Condition | Runs | Bad runs | By test |
|---|---|---|---|
| quiet host | 30 | 12 | `with_the_band_off_the_volume_is_never_paced` 8, `a_volume_entering_the_band_is_paced_and_not_refused` 5 (one run had both) |
| load: `stress-ng --cpu 12 --vm 2` plus `fio` randwrite 64k direct, 2 jobs, 4 GiB file on the scratch file system | 30 | 13 | pace-off 6, band-entering 7 |
| heavy load (48 CPU workers, 4 hdd workers, fio), `e1_*` tests only | 30 | 0 | none |

So the flake is **not caused by load**: it is as frequent on a quiet host. The brief's "while kill loops or builds
shared the host" is coincidence plus the fact that those hosts' cuts are slower. The `e1_...` flake did not reproduce
in 30 heavy runs of the four `e1_` tests, but the same helper failed once during the fix iterations under load, in
`a_discard_on_a_full_volume_brings_the_space_back_and_writes_succeed` at `all_enospc` (`stack_throttle.rs:316` before the
change: `[Ok(262144), -28, -28, -28]`), see cause 1.

## Causes (file:line are the pre-change lines of `stack_throttle.rs`)

1. **`with_the_band_off_the_volume_is_never_paced` (:1021).** Observed `state: Stop, space_waiters: 1, headroom: 0,
   journal_uncut: 36-45 MiB, store_factor_milli 2000, stops: 1, delayed_writes: 0`. With the band off "the bounded
   stop and the latch stay" (`lunix-space/src/throttle.rs`, `ThrottleConfig::enabled`). The ledger charges a cut in
   flight twice, its segment bounds and the journal it reclaims, until C9 (`lunix-space/src/lib.rs:8-14`,
   `lunix-engine/src/coord.rs:1132`); `ledger.rs:throttle_state` (:319-330) reports `Stop` while the head does not
   `fits`. 44 MiB written fast (4 to 10 MB/s) into a 64 MiB cap meets that window whenever the cut covers more than
   about 20 MiB, so the assertion `state == Open` at every 1 ms poll is a race with the coordinator (the author's own
   comment at `fill_until_parked` names the double count). Not load dependent; 8/30 quiet.
   A second assertion in the same loop, `delayed_writes == 0`, failed after the stop (4 s wait): `ledger.rs:note_delay`
   (:360-376) counts every request admitted after a wait in the gate as delayed, a wait for space included.
   Both are by design (the instrument documents "the gate's delay").
2. **`a_volume_entering_the_band_is_paced_and_not_refused` (:646 `raised`, later :668 and :675).** The collector's
   pressure is set only by `update_pressure` at the coordinator's tick (`coord.rs:407-409`), which cannot run while a
   cut or commit holds the coordinator; the test sampled `gc_pressure()` only in the first polling loop, so a loop that
   crossed the band between two ticks never saw it (5/30 quiet). The later two failures are the same race on the far
   side: after the loop, a C9 hands the double count back, the headroom jumps from under 8 MiB to 22-32 MiB, the band
   is left, and the FUA write finds no pace to bypass (`bypassed_fua 0`) or the bulk writes no pace to wait for
   (`delayed_writes 0`).
3. **`fill_until_parked` (the `all_enospc` failure).** "Parked for good" was `PARKED` (2.5 s) with a waiter. A cut that
   a loaded host stretches past 2.5 s leaves the head parked, the helper returns, the cut then ends at C9 and the head
   is granted: status `Ok` where `ENOSPC` is expected.
4. **`deep_overwrites_report_their_wait_from_arrival` (:908, seen only under load, 2 in 87 runs).** The test allowed 5 s
   (`SLACK`) between the gate's `delay_max_ms` (10.0 s, the cap) and the slowest write seen (15.3 s and 15.5 s, with
   514 and 617 writes against 913 on a quiet host, where the slowest is 11.1 to 11.5 s). The extra time is commit and
   append, not the gate (`delay_max_ms` stays at the cap).

None of these is a lost wakeup: in the 50 clean runs below every write completed `Ok` and `fulls` stayed 0.

## Fix (`crates/lunixd/tests/stack_throttle.rs` only)

* pace-off: the band off allows `Open` and the bounded `Stop` (with a waiter); never `Slowdown`/`Full`, a rate, a
  paced waiter or a capped delay; a delay (`delayed_writes > 0`) only if a stop (`stops > 0`) explains it; every write
  must complete `Ok` and `fulls == 0` at the end (so a stop that never ended fails, bounded by `full_timeout`).
* band-entering: after the loop the test waits (20 s, condition-based) for the pressure to be raised instead of
  relying on the sampling; the delay of the bulk blocks is owed only if the rate was `<= 16 MiB/s` on both sides of
  their submit, the FUA bypass only if the band held on both sides of the FUA submit. The FUA latency assertions
  (`< SLACK`) are unchanged.
* `fill_until_parked`: parked for good now also needs the journal to hold nothing to reclaim (`JournalReserved == 0`)
  for `PARKED`; with bytes left a cut is pending, with none no C9 can free space.
* deep overwrites: the allowance is three `SLACK`s with the numbers in the comment.

## Gate

* `stack_throttle` 50 runs under the load above (`stress-ng --cpu 12 --vm 2 --vm-bytes 1G`, fio as above), 13 tests
  per run: **50 of 50 green** (650 test runs). Three earlier 50-run attempts under the same load each found one
  more load-sensitive assertion (listed above), which is why the suite changed between them.
* `tools/ci.sh` green twice in a row on the quiet host (exit 0 both times, `stack_throttle` included).
* The remote tree is removed.

## Deviations, conflicts

* No rust-skills conflict. The test comments say why; no `unwrap` outside tests.
* The brief's idea "isolate the space probes with a per-test fake" was not needed: the probes are not involved.
  A product follow-up, for the lead to judge: the gate counts space waits as `delayed_*`, so with the band off the
  `throttle` status shows delayed writes; not changed.
