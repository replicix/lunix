# gcrate report: GC reclaim rate under sustained random overwrite

Branch `p1/gcrate` from `main` `6d16fbf`. Trigger: the M15 rerun (`fio-verify`, 64 KiB chunks,
integrity, profile 1, a 4 GiB working set rewritten under verify) grew the store past the harness's
80 GiB cap on raw, ext4 and xfs (xfs: 107 GiB, `gc --status`: `state: condemning`,
`condemned_pending: 3000`, last pass `segments_condemned: 1, segments_deleted: 1, packs_deleted: 9`).

## Root cause

**The pass serialises the grace.** A pass was `mark -> condemn -> wait out the grace (60 s) -> mark
again (recheck) -> delete -> compact`, then the next pass waited `pass_interval` (30 s) after it.
A pass can only delete what was already dead when its first mark began; what dies during the 60 s wait
is condemned by the *next* pass, which starts at least 90 s later and waits another full grace. Under a
write rate `R` the store therefore holds about `R x (2 G + pass_interval + 2 x pass work)` of garbage,
in a sawtooth, and the reclaim lag grows with the pass work.

**The pass work runs at a third of the coordinator's time, and grows with the backlog.** The collector
gets one 50 ms step per background tick (100 ms period plus the tick's own work: about one step every
180 ms measured, so about 28 % of the time). The work of a pass is per object: a condemn is one index
manifest swap (about 5.5 ms on the test host), a delete an unlink, a directory sync and two manifest
swaps (about 13 ms). With 3 000 objects (the M15 state) that is 16 s of condemns and 40 s of deletes,
so about 1 min and 2.5 min of wall time at that duty cycle, on top of the grace: the longer the pass,
the more garbage the next one finds, the longer it is. That is the M15 picture: a pass stuck in
`condemning` with 3 000 objects while the last finished pass (the first of the profile) condemned one
segment. Compaction at `gc_rate` is capped per step too (`gc_rate x step budget` = 3.2 MiB per step),
so at about 5.5 steps/s it ran at about 18 MiB/s, not 64.

None of the other bounds in the brief binds under this load (measured on the repro below):

| suspect | finding |
|---|---|
| evidence bounds (1024 objects kept, 16 pruned per pass) | one evidence object per pass; never more than 16 to prune |
| condemned-list cap (`MAX_ENTRIES`, about 10.7 M entries) | at most a few thousand entries |
| compaction threshold (output at least 1 KiB smaller) | whole dead segments are deleted, not compacted; about 100 MiB compacted per pass |
| rollback floor and retention pins | only commit objects; `retain_last` 2 keeps two commit spacings (about 2.5 s) of segments named |
| one cut per tip (`cut_wanted`) | under load the tip moves every 1 to 3 s |
| moves deferred under the dirty-leaf cap (`moves_deferred`, MapFull halving) | no "compaction re-hints" line in any run |
| the urgent pass and the space ladder (question 3) | never ran: the daemon logs of the M15 run have no urgent-pass line, and the repro neither. With the default `volume.json` there is no `cache_max` and the free-space floor is 10 % of the host file system (about 48 GiB here), so `NoSpace` comes long after the harness's 80 GiB cap. The urgent pass does what M12c meant when it runs (mark, delete what passed the floor grace, condemn, compact if short); it is a last resort for a full file system, not a reclaim-rate mechanism |

## What the design intends (question 2)

DESIGN section 11, the plan (M17 rules, M17.B gate) and `docs/reference/gc.md` state no reclaim rate
and no space amplification under sustained overwrite. The only space target is at rest: the M17.B churn
oracle, "after the final pass and grace, the store is at most 1.3 x live data plus 4 x the largest
commit's map-pack bytes plus the open segment". Compaction is rate-limited at `gc_rate` (64 MiB/s).
Implicitly, GC must keep up with writes on average, with a lag. The actual lag on `main` was
`2 G + pass_interval + 2 x pass work`, about 170 s with the defaults, and unstable when the pass work
grows with the backlog.

The bound this branch adopts and tests (derivation in `crates/lunixd/tests/stack_gc_rate.rs`):

```text
store <= 2 W + R (G + 3 P + 2 C) + slack
```

`W` working set; the live side is at most `2 W` because compaction leaves segments at or above half live
(`gc_live_threshold` 0.5); `R` write rate; `G` grace; `P` pass period (`busy_interval` plus a pass's
work); `C` commit spacing (`retain_last` 2 keeps the segments named by the two newest FULLs). `R G` is
the floor any schedule has while the grace is 60 s: at 38 MiB/s, 2.2 GiB.

## The fix

1. **Pipelined passes** (`GcConfig::pipeline`, `crates/lunix-gc/src/{config,collector}.rs`). A
   scheduled pass no longer waits out the grace. Order: mark, condemn what is newly dead, delete the
   objects listed before this pass whose grace ended before the mark began (the urgent pass's rule, with
   the full grace: `grace start + G <= mark start`, both on the monotonic clock, the mark's start read in
   the unit that pins its roots), compact from the same mark. Objects whose grace still runs are skipped
   without an evidence line, and the condemned list is rewritten only when an entry went. A **requested**
   pass (`lunix gc NAME`, `request_pass`) still runs the whole sequence, so `GcControl::run_pass` keeps its
   meaning. `GcConfig::default()` keeps the whole sequence for every pass (the collector's tests, the
   offline `fsck --repair --gc`); the daemon uses `GcConfig::daemon()`.
2. **Pacing** (review M-4, m-2, m-7). A pass is "busy" only when it did reclaim work worth hurrying for:
   a segment condemned, deleted or compacted, or 64 MiB freed (the commit objects and packs every cut
   leaves dead no longer count). After a busy pass the next is due after `busy_interval` (5 s), else
   after `pass_interval` (30 s); a grace that ends starts a pass without a new tip. Whatever the pace, a
   pass follows the last one at the earliest after 3 x that pass's own work (time inside its units), so
   marking takes at most about a quarter of the coordinator however large the map. An abandoned pass is
   retried after `pass_interval`; an idle check clears "busy".
3. **Busy ticking** (`crates/lunix-engine/src/{engine,coord}.rs`). While the pass in progress has work a
   step can do now, the background thread ticks every 10 ms instead of 100 ms. Not while a whole
   sequence waits out its grace, nor while compaction sits at its rate cap (no bytes read, nothing moved
   or freed), nor once the engine failed or closes (m-1, m-4).
4. **A coordinator share for GC after cuts** (`gc_share`, coord.rs; found by the root check). Under a
   sustained overwrite the coordinator cuts back to back, and GC got one 50 ms step per cut (a few
   percent of the coordinator). After a cut, while the pass is busy, GC now steps for up to a third of
   that cut's time: about a quarter of the coordinator. The next cut waits that long; each step is still
   bounded by `gc_step_budget_ms`. With a manual clock this adds nothing (the tests are unchanged).
5. **Compaction throughput** (root check). The daemon's per-step byte allowance was `gc_rate x 50 ms`
   (3.2 MiB): with few steps per second compaction ran at about 12 MiB/s. It is now one second of
   `gc_rate`: the step's time budget bounds a step and the collector's own rate cap keeps the mean at
   `gc_rate` (`crates/lunixd/src/stack/mod.rs`). Under writes, pending moves cut at most once a second
   while a pass is busy (a cut per tick left the coordinator to the cuts; review m-5); on an idle volume,
   or when no pass is busy, they cut at once as before. The daemon compacts 256 MiB per pass instead of 1 GiB, so a pass's compaction phase does not
   hold back the deletes the next pass makes.
6. **A pass profile line** (`gc pass done`; INFO for a pass that wrote evidence, DEBUG otherwise): wall
   time, steps (now counted correctly), per phase the active time, units and when the pass entered it.

No format, interface or `volume.json` change. `GcConfig` (not an interface type) gained `pipeline` and
`busy_interval`.

## Reproduction without root (256 MiB working set)

`crates/lunixd/tests/stack_gc_rate.rs` on the stack: a 256 MiB working set, 64 KiB chunks, integrity,
incompressible random 64 KiB overwrites at QD 1 (36 to 44 MiB/s), the daemon's engine, the store
(segments, map packs, index runs, commits) sampled every 2 s. Numbers from the first round (5773f3a):

| build, daemon defaults (`G` 60 s), 300 s | store over the run | peak | passes |
|---|---|---|---|
| `main` `6d16fbf` | sawtooth 4.4 to 7.0 GiB | 7 009 MiB (27.4 x W) | 3 |
| 5773f3a, `pipeline` off | sawtooth 4.2 to 7.4 GiB | 7 407 MiB (28.9 x W) | 4 |
| 5773f3a, `pipeline` on | steady 3.9 to 4.2 GiB from 110 s on | 4 249 MiB (16.6 x W) | 34 |

With whole 64 KiB overwrites of a small working set, segments die whole and GC is grace-bound: the
steady state is about `W + R x 95 s`, of which `R G` is 60 s.

## Root check: M15 `fio-verify`, profile 1 (QD 32, 4 KiB to 1 MiB random writes, 4 GiB per job)

`tests/exit/fio-verify.sh` of `p1/m15` with this branch's release binaries, `PROFILES="32:1"`, a fresh
32 GiB volume per cell, daemon on CPUs 0-3 and fio on 4-7, `tools/remote.sh root`, the 80 GiB cap on,
one root run at a time. A sampler next to the harness recorded the device's written bytes, the store
(`segments`, `map`, `index`, `state/commits`) and `lunix gc --status --json` every 10 s. `R` below is the
store's intake (store plus freed, per second), which is above the device rate: a 4 KiB write rewrites
its whole chunk. The model's bound is `2 W + R (G + 3P + 2C) + 0.5 GiB` with `W` 4.25 GiB (the 4 GiB
region plus the trim region), `G` 60 s, `P` 9 s, `C` 5 s. Every cell passed: fio verify, trimcheck,
guest fsck, `lunix fsck --deep` before and after the detach and reattach, `verify_only`. No cell reached
the cap.

| binaries | cell | device rate | `R` | store at 600 s | model bound | freed |
|---|---|---|---|---|---|---|
| `main` (M15's log, p1 after p0) | raw / ext4 / xfs 64K integrity | | | +53 / +40 / +51 GiB in p1 (81 / 86 / 107 GiB) | | |
| 5773f3a | raw 64K integrity | 62 MiB/s | 97 MiB/s | 49 GiB, still rising | 18 GiB | 8 GiB |
| 5773f3a | ext4 / xfs 64K integrity | | 74 / 75 MiB/s | 56 / 57 GiB | 16 GiB | |
| + share and per-step bytes | raw / ext4 / xfs 64K integrity | 42 MiB/s (raw) | 78 / 58 / 52 MiB/s | 36 / 41 / 37 GiB | 14 to 16 GiB | 9.4 GiB (raw) |
| + 256 MiB per pass (final) | raw 64K integrity | 50 MiB/s | 88 MiB/s | 32 GiB (18 to 28 GiB in the middle, still drifting up) | 17 GiB | 19 GiB |
| final | raw / ext4 / xfs 64K encrypted | 50 MiB/s (raw) | 91 MiB/s (raw) | 28 / 29 / 26 GiB; raw flat at 24 GiB over its last 4 min | 18 GiB | 25 GiB (raw) |
| final | raw / ext4 / xfs 16K integrity | 31 MiB/s (raw) | 40 MiB/s (raw) | 11 / 13 / 12 GiB; raw 10 to 14 GiB over its last 6 min | 13 GiB | 12 GiB (raw) |
| committed, merged with `main` 871e187 (p1/bstop) | raw / ext4 / xfs 64K integrity | 50 MiB/s (raw) | 89 MiB/s (raw) | 28 / 25 / 27 GiB; raw 20 to 33 GiB over its last 6 min | 17 GiB | 23 GiB (raw) |

(The sampler's written and freed counters are valid for the raw cells only: the device counter restarts
with each device and the file-system cells' status calls returned nothing usable.)

What this shows:

- **A second bottleneck, which the 256 MiB repro did not have: compaction.** With a 4 GiB working set and
  4 KiB to 1 MiB writes, a 16 MiB segment dies whole only after about six to eight working sets more have
  been written, so reclaim runs through compaction. On 5773f3a compaction ran at about 12 MiB/s: the
  per-step byte cap (3.2 MiB) and about three GC steps a second between back-to-back cuts. Passes 9 and
  10 of the raw cell took 111 and 174 s, 97 % of it waiting for steps.
- With the final binaries GC gets about 12 % of the run on average (16 % to 25 % in the long passes; the
  raw 64K integrity cell: 66 s of unit time in 535 s), the mark of this 4 GiB working set on a 32 GiB
  volume costs 0.2 to 0.65 s, and passes take 5 to 80 s. Foreground write throughput dropped (raw 64K
  integrity: 62 to 42 to 50 MiB/s across the three builds): that is the share GC now takes from the cuts.
- The store under the M15 profile is about half of `main`'s: 32 GiB against 81 GiB (raw), with the
  80 GiB cap far away. **It is not inside the model's bound.** 64K encrypted and 16K integrity plateau
  near 1.4 x and 1.1 x the bound; 64K integrity is still drifting up at 600 s, at about 1.6 to 1.9 x.
  The remaining gap is compaction throughput against the write rate under a fixed GC share: a policy
  choice (see Open 2).

## Large-map cost (review M-4)

Measured on the root cells (a 4 GiB working set on a 32 GiB volume, about 65 000 live chunks per root):
the mark costs 0.2 to 0.65 s of unit time, a pass's work 0.2 to 16 s, and the duty cap keeps the gap
after a pass at 3 x its work or more. A volume with a map ten times larger marks in seconds and is
paced down to one pass per 4 x mark time at worst; that is bounded, not measured here.

## Tests

- `crates/lunix-gc/src/tests/pipeline.rs` (8): the schedule (a scheduled pass condemns without waiting
  and a later one deletes after the grace; a pass starts when a grace ends without a new tip, and passes
  that only find graces running write no evidence; a requested pass still runs the whole sequence) and
  the delete boundary (review M-1): a grace that ends during the deleting pass's mark keeps the objects
  for the next mark (**this test fails when `<= l.started` in `recheck_verdict` is replaced by the
  current time**; checked by hand); a dedup hit between the mark and the flag in the pipelined order is
  un-condemned by the pass after the grace (the hit is asserted on the listed segment); an urgent pass
  during a pipelined pass's condemn and delete units; a request during a pipelined pass makes the next
  pass whole; a pin on a condemned commit between condemn and delete keeps the commit and everything its
  root reaches.
- `crates/lunix-gc/src/tests/crash.rs`: the pipelined crash sweep at 16 KiB / 16-byte ids, 64 KiB /
  32-byte ids and lean ids (every crash point hit by 10 of 10 workloads); every sweep now also checks the
  roots of the workloads that did not crash and drives them to quiescence (m-12).
- `crates/lunixd/tests/stack_gc.rs`: the churn oracle on the daemon's pipelined schedule
  (`churn_pipelined_plain_16k`, `churn_pipelined_encrypted_64k`): reads against the flat oracle during
  GC, reopen, `lunix-fsck --deep`, the at-rest store bound, no GC work left (M-2).
- `crates/lunixd/tests/stack_gc_kill.rs`: the kill loop runs the pipelined schedule by default
  (`LUNIX_GC_PIPELINE=0` for the whole sequence). The kill loops stay out of CI, as before; the plain
  16 KiB loop was run by hand (21 of 21, see Gate).
- `crates/lunixd/tests/stack_gc_rate.rs`, `sustained_overwrite_stays_bounded` (M-3): 150 s; `P` is the
  run's mean pass period and `R` the rate over `G + 3P` before each sample; it asserts that the store
  does not keep growing (the last third's peak is at most the middle third's plus `R x P`), a 1.5 x
  backstop over the bound, every chunk reads back its last write, and `lunix-fsck` is clean.
- `tools/tests/run.sh`: `remote.sh` ids (`gcrate`, `m12c`, `m18a` accepted; `cargo`, a slash, a dot
  refused) and the `p1/gcrate` branch guess.

## Gate

`tools/ci.sh` on the test host (`LUNIX_CPUS=0-7`, `-j 8`) on the tree merged with `main` 871e187
(5fd36d9 plus the report): 1 902 tests passed, 0 failed, 28 ignored (summed over every `test result`
line), fmt, clippy `-D warnings` (workspace and `lunixd` with `testsupport,crashpoints`), the feature
edges, the release stack tests with `lunix-fsck`, `tools/tests`. The script's ownership step compares
with the local `main` ref, which is behind `origin/main` in this worktree (it listed bstop's paths); the
same check against the merge base with `origin/main` (`--changed-list`) gives
`check-ownership: gcrate ok (17 changed paths)`. Before the merge, at a2e9408: all green, 1 828 passed.
Among the tests: the 5 new directed pipeline tests, the pipelined crash sweeps at 16 KiB, 64 KiB and
lean ids, `churn_pipelined_plain_16k`, `churn_pipelined_encrypted_64k`,
`sustained_overwrite_stays_bounded` and the `tools/tests` cases.

By hand: `gc_kill_loop_plain_16k` on the pipelined schedule: 21 points, 21 reached, 21 kills, all
verified (1 061 s). Root: seven `fio-verify` cells (above), all passing, host left clean (no ublk device,
mount or daemon of this milestone; `ublk-leftovers: clean` after every run).

Remote disk: 2.8 GiB after the gate (`target/debug` and the incremental caches removed, the release build kept); scratch 3 MiB (logs).

## Review response (`01-p1-gcrate-review.md`)

| finding | response |
|---|---|
| M-1 delete boundary untested | Five directed tests in `pipeline.rs` (above); the mid-mark grace test fails with the boundary moved to the current time. |
| M-2 no data-verifying test on the daemon schedule | Two pipelined churn-oracle tests in `stack_gc` (CI); the `stack_gc_kill` loop on the pipelined schedule by default (plain 16 KiB run by hand: 21 of 21); read-back and `fsck` in `stack_gc_rate`; pipelined crash sweeps at 64 KiB and lean ids. `stack_kill` and `stack_gc_ladder` keep their own `GcConfig` (they test the engine's kill recovery and the ladder, whose urgent pass is the same in both schedules). |
| M-3 flaky bound | `P` and `R` from the run, a no-growth assertion, a 1.5 x backstop. |
| M-4 continuous marking, cost not bounded | "Busy" means reclaim work (segments, or 64 MiB freed); a duty cap of 3 x the last pass's work; measured on the root cells (above). |
| m-1 `gc_busy` left set | Cleared on the failed and closing paths. |
| m-2 failing pass retried every 5 s | `abort_pass` clears busy; the grace trigger waits `pass_interval` after an abort. |
| m-3 todo condemned late | Pipelined order is now mark, condemn, delete, compact. |
| m-4 busy ticking at the rate cap | Not busy when a compaction step read, moved and freed nothing. |
| m-5 cuts during compaction | Under writes, move cuts at most once a second while a pass is busy. Commits per second, raw 64K integrity cells: not separated from the dirty-data cuts in the logs; the 256 MiB repro made 0.35 commits/s on `main` and 0.87/s on 5773f3a. |
| m-6 list rewrite every pipelined delete phase | Stored only when an entry went. |
| m-7 idle polling at 5 s | The idle check clears busy. |
| m-8 evidence history | gc.md says how long 1 024 objects last now and that condemn and delete lines are in different objects. |
| m-9 profile line | `steps` counted per step before the line; DEBUG for passes without evidence; `phases` stays one string. |
| m-10 module doc | The collector's header gives both orders and the boundary rule. |
| m-11 `condemned_pending` normally non-zero | Noted in gc.md. |
| m-12 uncrashed workloads unchecked | Checked and driven to quiescence. |
| m-13 typo guard | `remote.sh` requires an `[id]` entry in `tools/ownership.toml` (`m18a` excepted) and has a `check-id` subcommand for `tools/tests`. |

## Deviations and choices

- `tools/remote.sh`: ids `^[a-z][a-z0-9]*$` with an ownership entry (above).
- `GcConfig::default()` keeps the whole sequence (the collector's harnesses and `fsck --repair --gc`
  rely on a pass deleting what it condemned).
- The root cells' "final" binaries paced move cuts whenever a pass was busy; the committed code paces
  them only while the volume is also being written (new writes since the tip). The unconditional form
  made `stack_gc`'s `compaction_then_truncate_*` fail about half the time (a cut for the remaining
  moves of a pass landed inside the next pass on an idle volume, which the B-1 sequence forbids); the
  root cells were under writes throughout, where the two forms behave the same.
- The root check's sampler and the copy of M15's harness lived in the worktree untracked for the runs
  and are not committed (the harness is `p1/m15`'s). The harness ignores a TERM after its teardown:
  `teardown` clears the traps and returns, and the script goes on to the next cell (seen when a run was
  stopped early). That is for M15 to look at.
- `crates/lunixd/tests/wiring.rs` `journal_damage_is_reported_and_wiped_and_refuse_keeps_it` is flaky on
  the host: extra journal records (`next_seq` 4 or 5 instead of 3) after the three writes. It is not this
  branch: on `main` 3e6fb81 it failed 5 of 8 runs of the `wiring` suite, on this branch 1 of 6. Not
  changed here; worth an issue for its owner.
- Merge of `main` 871e187 (p1/bstop), conflicts resolved by judgement: `coord.rs` `tick()` keeps both
  the ADR-32 space cut (`space_cut`, `after_space_cut`) and `gc_share` (a space cut that commits also
  gives a busy collector its share, which a full volume needs most); `tools/ci.sh` runs both
  `stack_gc_rate` and `stack_throttle`; `tools/ownership.toml` keeps both entries. `tools/remote.sh`
  merged cleanly (bstop carried the same id check).
- rust-skills: one local `macro_rules!` picks the log level of the pass line (tracing needs a constant
  level); no conflict with the project's conventions.

## Ownership

`[gcrate]` in `tools/ownership.toml` (confirmed by the lead, with these additions from the review fixes:
`crates/lunixd/tests/stack_gc.rs`, `stack_gc_kill.rs`, `tools/tests/run.sh`): `lunix-gc` `collector.rs`,
`config.rs`, `tests/{mod,crash,pipeline,sim}.rs`; `lunix-engine` `coord.rs`, `engine.rs`; `lunixd`
`src/stack/mod.rs`, `tests/{stack_gc,stack_gc_kill,stack_gc_rate}.rs`; `docs/reference/gc.md`;
`tools/{ci.sh,remote.sh,ownership.toml,tests/run.sh}`.

## Open (for the maintainer; not implemented)

1. **The grace is the floor of the grace-bound case.** `R G` is most of the garbage when segments die
   whole (the 256 MiB repro). A lower default toward the 30 s floor halves it.
2. **GC's share against the writes, in the compaction-bound case.** The M15 profile with a 4 GiB working
   set needs compaction, and compaction runs on the coordinator between cuts. With the share of about a
   quarter (`gc_share`) the store settles for 64K encrypted and 16K integrity and still drifts for 64K
   integrity, at 1.1 to 1.9 x the model's bound. **The remaining gap to the bound (64K integrity, 1.6 to
   1.9 x) goes to ADR-32 T4 (GC pressure and boost)**, which is where the share against the writes is
   decided. **Compaction off the coordinator thread** is a candidate follow-up (an interface change: the
   collector runs under the coordinator mutex).
3. **No back-pressure on garbage.** Writes are not slowed when garbage outruns GC; the ladder acts only
   at `NoSpace`. The pre-GC 245 GB parking is the same gap.
4. **Per-object index manifest swaps** (condemn 5.5 ms, delete 13 ms): a batch call would need a frozen
   `ChunkIndex` change.
5. The grace default is unchanged (lead, 2026-11-18).
