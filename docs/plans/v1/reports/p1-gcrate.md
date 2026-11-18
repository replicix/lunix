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
   scheduled pass no longer waits out the grace. Its one mark is the recheck of every listed object
   whose grace ended before the mark began (exactly the urgent pass's rule, with the full grace: the
   recheck mark's roots are pinned after the flag and the grace), so it deletes those, then condemns
   what is newly dead, then compacts from the same mark. Objects whose grace still runs are left alone
   without an evidence line. A pass that changed something is followed after `busy_interval` (5 s)
   instead of `pass_interval` (30 s), and a grace that ends starts a pass (after `busy_interval`) even
   without a new tip. A dead object now waits `G + 2 P` plus the retained window, instead of up to
   `2 G + pass_interval + 2 P`. A **requested** pass (`lunix gc NAME`, `request_pass`) still runs the
   whole sequence (condemn, grace, recheck, delete), so `GcControl::run_pass` and its tests keep their
   meaning. `GcConfig::default()` keeps the whole sequence for every pass (the collector's tests, the
   offline `fsck --repair --gc`); the daemon uses `GcConfig::daemon()` (`pipeline` on) when no config
   is given (`crates/lunixd/src/stack/mod.rs`).
2. **Busy ticking** (`crates/lunix-engine/src/{engine,coord}.rs`). After a step that leaves its pass
   with work it can do now (not idle, not in the whole sequence's grace), the background thread ticks
   again after 10 ms instead of 100 ms. The collector then gets about 80 % of the coordinator; each step
   is still bounded by `gc_step_budget_ms` and the cuts still come between steps. Pass wall time on the
   repro went from about 10 s to about 3 s.
3. **A pass profile line** (`gc pass done`, INFO, once per pass): wall time, steps, and per phase the
   active time, units and when the pass entered it. This is what located the bottleneck; it stays for
   field diagnosis.

No format, interface or `volume.json` change. `GcConfig` (not an interface type) gained `pipeline` and
`busy_interval`.

## Reproduction and numbers

`crates/lunixd/tests/stack_gc_rate.rs` on the stack, no root: a 256 MiB working set, 64 KiB chunks,
integrity, incompressible random 64 KiB overwrites at QD 1 (36 to 44 MiB/s on the test host), the
daemon's engine (background coordinator, `volume.json` commit triggers), the store (segments, map
packs, index runs, commits) sampled every 2 s.

Daemon defaults (`G` 60 s), 300 s, `sustained_overwrite_daemon_defaults`:

| build | store over the run | peak | passes | pass wall |
|---|---|---|---|---|
| `main` `6d16fbf` | sawtooth 4.4 to 7.0 GiB, still rising each cycle | 7 009 MiB (27.4 x W) | 3 | 65 to 83 s (grace 60 s, about 5 s active) |
| this branch, `pipeline` off (M17 sequence, busy ticking on) | sawtooth 4.2 to 7.4 GiB | 7 407 MiB (28.9 x W) | 4 | 63 to 72 s |
| this branch (daemon: `pipeline` on) | steady 3.9 to 4.2 GiB from 110 s on | 4 249 MiB (16.6 x W, the warm-up crest at 100 s) | 34 | about 3 s |

The steady state with the defaults is about `W + R x 95 s`; `R G` alone is 60 s of it. For the M15
shape (`W` 4 GiB) the store should settle near `2 W + R x 95 s`: about 22 GiB at 150 MiB/s, and the
80 GiB cap is reached only above about 750 MiB/s of sustained writes. That is an estimate from this
model, not a measurement: the root rerun is not done (see Open).

CI test `sustained_overwrite_stays_bounded` (`G` 10 s, the collector's minimum with `read_timeout`
0; 140 s; samples after a warm-up of `pass_interval + 2 G + 3 P` checked against the bound with `P` =
5 s + 4 s of work and the run's own `R` and `C`):

| build | steady store | bound |
|---|---|---|
| pipeline, no busy ticking | 1.75 to 2.2 GiB (passes 10 s apart, 15 s period) | (would fail) |
| pipeline and busy ticking | 1.42 to 1.83 GiB (5.6 to 7.1 x W) | about 2.0 GiB (7.8 x W) |

The time constant `k` asked for in the brief is not a constant multiple of `W`: the garbage term
scales with `R`, so the test states `k = 2 + R (G + 3P + 2C) / W` and checks it with the measured `R`
and `C`.

## Tests

- `crates/lunix-gc/src/tests/pipeline.rs` (3): a scheduled pass condemns without waiting and a later one
  deletes after the grace, nothing listed goes early; a pass starts when a grace ends without a new tip
  and passes that only find graces running write no evidence; a requested pass still waits out its
  grace and deletes in the same pass.
- `crates/lunix-gc/src/tests/crash.rs`: `every_crash_point_pipelined_16k`, the crash sweep with the
  pipelined schedule: every GC crash point hit by 10 of 10 workloads, roots read back, ledger exact,
  convergence. `Sim::quiesce` runs one more pass without a commit under `pipeline`.
- `crates/lunixd/tests/stack_gc_rate.rs`: the bound above (CI), and the by-hand comparison with the
  daemon defaults (`LUNIX_GC_RATE_PIPELINE=0` for the M17 sequence).
- `tools/ci.sh` runs `stack_gc_rate` with the other stack GC tests.

## Gate

`tools/ci.sh` on the test host (`LUNIX_CPUS=0-7`, `-j 8`) at the committed tree: **all green**.
1 819 tests passed, 0 failed, 28 ignored (summed over every `test result` line; `main` `6d16fbf`
reported 1 814). New: 3 pipeline unit tests, the pipelined crash sweep, and
`sustained_overwrite_stays_bounded` (passed, about 140 s). fmt, clippy `-D warnings` (workspace and
`lunixd` with `testsupport,crashpoints`), the feature edges, the release stack tests with `lunix-fsck`,
and `check-ownership: gcrate ok (15 changed paths)`. No root run (see Open).

Remote disk: about 20 GiB under the milestone directory, all of it `target/` (debug and release
builds, as for the other milestones); the scratch volumes of the tests are removed by the tests.

## Deviations and choices

- `tools/remote.sh` accepted only milestone ids `m<digits>...`; `gcrate` failed it. The check is now
  `^[a-z][a-z0-9]*$` (the ownership table's own id syntax).
- `GcConfig::default()` keeps the whole sequence rather than the daemon's schedule: two of its three
  users (the collector's test harnesses and `fsck --repair --gc`, which runs offline passes until one
  frees nothing) rely on a pass deleting what it condemned. The stack tests that pass their own
  `GcConfig` (`pass_interval` 1 s) also keep the whole sequence; the stack runs without one (the
  daemon, `control_gc`, the reopens in the ladder tests) get the pipelined schedule.
- rust-skills: no conflict with the project's conventions in this change.

## Ownership

`[gcrate]` in `tools/ownership.toml`: `lunix-gc` `collector.rs`, `config.rs`, `tests/{mod,crash,
pipeline,sim}.rs`; `lunix-engine` `coord.rs`, `engine.rs`; `lunixd` `src/stack/mod.rs`,
`tests/stack_gc_rate.rs`; `docs/reference/gc.md`; `tools/{ci.sh,remote.sh,ownership.toml}`.

## Open

1. **The grace is now the floor.** With `gc_grace_ms` 60 s, `R G` is the larger part of the garbage
   (2.2 GiB at 38 MiB/s, 9 GiB at 150 MiB/s). Lowering the default toward its floor (30 s) halves it; it
   is a policy choice (the grace covers in-flight cuts, destage and reads) for the maintainer.
2. **No back-pressure on garbage.** Nothing slows writes when garbage outruns GC; the space ladder acts
   only at `NoSpace` (host free-space floor or `cache_max`). A rate above `(cap - 2 W) / (G + 3 P + 2 C)`
   still fills any cap. The pre-GC 245 GB parking is that same missing back-pressure, not this root
   cause.
3. **Per-object index manifest swaps.** Each condemn and delete rewrites and syncs the index manifest
   (a condemn 5.5 ms, a delete 13 ms here). Batched flags would need a `ChunkIndex` batch call (frozen
   interface); not needed at these rates now that the pass runs at about 80 % duty.
4. **The M15 root rerun** with this branch (profile 1 on raw, ext4, xfs) has not been run: root runs
   need the lead's go-ahead (proposed: the same `fio-verify.sh` invocation with this branch's binaries,
   one cell at a time on CPUs 0-7, `gc --status` sampled every 10 s).
