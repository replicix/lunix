# Plan 01 — Phase 1: local-only volume

Read `docs/plans/v1/CONVENTIONS.md` first, then the "Execution rules for
this plan" section below. This plan overrides CONVENTIONS in the places
it names (commits, `CARGO_TARGET_DIR`, where code runs). Everything else
in CONVENTIONS still applies.

Spec context: `docs/explanation/DESIGN.md` (all of it; each milestone
names the sections that matter), `docs/explanation/DECISIONS.md` ADR-3,
4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 16, 17, 18, 19, 20, 24, 25, 26, 27, 28, 30 and
the gate-outcome blocks, `docs/explanation/GLOSSARY.md`,
`docs/plans/v1/ROADMAP.md` (Phase 1 and the format-freeze paragraph at
its top), and the phase 0 results in `bench/BASELINE.md` and the
`RESULTS.md` files it links.

**Depends on:** plan 00 (phase 0 is closed: all ADRs 1-30 are accepted
as of 2026-11-12). Phase 0 left reusable spike code; this plan tells
implementers where to start from it.

**Revised 2026-11-12** after the independent review
(`wip/01-p1-review.md`) and the maintainer's decisions of the same day
(the "Decisions already taken" table in M1). DESIGN.md, DECISIONS.md,
GLOSSARY.md and ROADMAP.md are being updated in a parallel edit to say the
same; where this plan states a rule the spec does not yet state, the
plan's text governs and your report notes the discrepancy. The plan never
edits the spec.

**Revised again 2026-11-12** after the second review
(`wip/01-p1-review-2.md`, findings 1-16 and gap resolutions N6-N13). The
spec edits it asked for have landed in DESIGN.md, DECISIONS.md and
GLOSSARY.md (floor no longer genesis, `gc/` evidence body, mount never
falls back, delete-time index invariant, plain-tag coverage). The lead
decided finding 1 (the plain-extent tag covers every byte before the tag).

**Revised 2026-11-12 (third time)** after the independent review of M1.A0
(`wip/01-p1-m1a0-review.md`) and the maintainer's decisions of the same
day: three volume modes with `integrity` as the default (D16), and the
review's spec-level rules (D17: dedup confirmation, the `created_at`
clamp, journal resync, checkpoint and allocator rules, `lineage_idx` in
interior map entries). DESIGN.md, DECISIONS.md, GLOSSARY.md and ROADMAP.md
carry the same rules.

## Goal

Build the first product code: a local-only Lunix volume (topology "local
only", local tier `store`, no backend, mode `local`) that passes the phase
1 exit, and freeze every on-disk and on-bucket format that ROADMAP lists,
so that later phases add code and never migrations.

Phase 1 delivers (ROADMAP "Phase 1"):

- `lunixd`, its control socket, and `lunix volume create|attach|detach|status`.
- The ublk device: parameters, queues, recovery on restart.
- The journal and the dirty overlay.
- The destage pipeline: zero, BLAKE3, dedup index, `auto` compression,
  segments.
- The map with local commits; DISCARD and WRITE_ZEROES.
- The space ladder with `on_full`; online grow.
- `fsck`, including index rebuild.
- Encryption: format, keyring with key slots, AEAD, the attach-time
  downgrade check (descriptor MAC and KMK fingerprint), hash-chained
  commits with an HMAC on every commit of `integrity` volumes, and the
  three volume modes (`encrypted`, `integrity` as the default, `plain`;
  D16).
- Local garbage collection and compaction (M17): mark from the GC roots,
  per-segment liveness, LFS cost-benefit compaction, condemn → grace →
  recheck → delete of segments, map packs, index runs and commit objects
  below `chain_floor`, the space ladder's compact step made real, and
  fsck integration.

**Phase 1 exit** (ROADMAP, verbatim in substance; the gates for it are in
M15, M16 and M17):

- fio verify passes.
- xfstests passes on ext4 and xfs over the device.
- A kill -9 crash loop at every step passes, with fsck clean.
- Dedup and compression ratios are reported.
- Local GC reclaims churn: the store stays within the bound of M17.B's
  gate and `lunix fsck` is clean after every crash point, including the
  `gc.*` ones.
- Perf meets the DESIGN §20 targets for local hits and writes, or the
  shortfall is explained.
- The default chunk size is decided (ADR-5): fio, a database (pgbench) and
  a VM-image workload at 16 KiB and 64 KiB, comparing dedup and
  compression ratio, map size, CPU per GiB and partial-chunk RMW. (The
  maintainer decides; M16 produces the evidence and a recommendation.)

## Status (2026-11-12)

Not started. The plan is written and was revised on 2026-11-12 after the
independent review; no milestone has run.

| milestone | crate / owner paths | model | review | status |
|---|---|---|---|---|
| M1.A0 byte layouts, `FREEZE.md` draft, attach matrix | `docs/reference/on-disk-formats.md`, `docs/reference/attach-matrix.md`, `crates/lunix-format/FREEZE.md` | design-heavy (or lead) | required (independent) + maintainer sign-off | merged |
| M1.A workspace, CI, `lunix-format` | root files, `tools/`, `crates/lunix-format`, crate skeletons | design-heavy | required (independent) | merged |
| M1.B1 `lunix-iface` | `crates/lunix-iface` | design-heavy | required (independent) | merged |
| M1.B2a `lunix-testkit` part 1 | `crates/lunix-testkit` | routine | lead skim | merged |
| M1.B2b `lunix-testkit` part 2 (fakes, contract tests) | `crates/lunix-testkit` | design-heavy | required (with B1) | merged |
| M2 `lunix-crypto` | `crates/lunix-crypto` | design-heavy | required (independent) | merged |
| M3 `lunix-journal` | `crates/lunix-journal` | design-heavy | required (recovery) | merged |
| M4 `lunix-overlay` | `crates/lunix-overlay` | routine | lead skim | merged |
| M5 `lunix-map` | `crates/lunix-map` | routine (spike-guided) | lead skim | merged |
| M6 `lunix-index` | `crates/lunix-index` | routine | lead skim | merged |
| M7 `lunix-store` | `crates/lunix-store` | routine | recommended (durability order) | merged |
| M8 `lunix-space` | `crates/lunix-space` | routine | lead skim | merged |
| M9 `lunix-destage` | `crates/lunix-destage` | design-heavy | required (with M11) | merged |
| M10 `lunix-ublk` | `crates/lunix-ublk` | design-heavy | required (independent) | merged (follow-up R1 open) |
| M11 `lunix-engine` | `crates/lunix-engine` | design-heavy | required (commit protocol) | merged |
| M12a `lunixd` daemon, control plane, wiring | `crates/lunixd` (except `tests/stack_*.rs`; creates the `testsupport` stub) | design-heavy | recommended (wiring) | not started |
| M12b in-process stack tests, `lunixd::testsupport` | `crates/lunixd/tests/stack_*.rs`, `crates/lunixd/src/testsupport.rs` | design-heavy | recommended | not started |
| M13a `lunix-cli`, socket commands | `crates/lunix-cli` (all, including the `fsck`/`key` stubs and their wiring) | routine | lead skim | not started |
| M13b `lunix-cli` `fsck` and `key` | `crates/lunix-cli/src/{fsck,key}.rs` | routine | lead skim | not started |
| M14 `lunix-fsck` | `crates/lunix-fsck` (M14 creates the `src/gc.rs` stub; M17.B fills it) | routine | recommended | not started |
| M17.A `lunix-gc` against fakes (mark, liveness, compaction, condemn/grace/recheck/delete) | `crates/lunix-gc` (except `tests/`), `docs/reference/gc.md` | design-heavy | required (independent) | not started |
| M17.B GC on the real stack: fsck integration, oracle churn test | `crates/lunix-gc/tests/**`, `crates/lunix-fsck/src/gc.rs`, `crates/lunixd/tests/stack_gc*.rs`, `bench/phase1/gc/` | design-heavy | required (with M17.A) | not started |
| M15 exit tests: fio verify, xfstests, crash loops, ratios | `tests/exit/`, `bench/phase1/exit/` | routine (triage by lead) | n/a | not started |
| M16 performance gate and the ADR-5 chunk-size benchmark | `tests/perf/`, `bench/phase1/perf/`, `bench/phase1/chunksize/` | routine (analysis reviewed) | n/a | not started |
| M18a replicix-core: codec id 3, dictionary blob, `replicix-zdict` | `/home/bra/cvs/replicix/replicix-core` (`crates/chunkfmt`, new `crates/zdict`) | design-heavy | required (independent, statistical for the evaluator) | not started |
| M18b Lunix dictionary study harness and note | `bench/dict-study/lunix/`, `bench/dict-study/constellation-arm/`, `tests/dictstudy/`, `docs/explanation/research/2026-*-zstd-dictionaries-on-block-devices.md` | design-heavy (method, analysis) | required (independent statistical review) | not started |

## Execution rules for this plan

### Where code runs

1. **Never compile, test or run anything on the maintainer's workstation.**
   Editing files and running `git` locally is fine. Every `cargo`
   invocation, every test, every script that touches a device or a
   kernel interface runs on the **remote test host**, over ssh. The lead's
   brief gives the ssh destination (as `LUNIX_HOST`), the base directory
   on the host (`LUNIX_HOST_BASE`) and any signing or credential
   environment. This plan, the repository and every report never record a
   host name, address or user. `tools/remote.sh` reads them from the
   environment; `.env.host` is git-ignored.
2. **One host directory per milestone:** `$LUNIX_HOST_BASE/<m>/` with
   `src/` (a synced copy of the worktree), `target/` (the cargo target
   dir) and `scratch/` (test files; `LUNIX_SPIKE_DIR` is set to it for every
   command, so CONVENTIONS' scratch-directory safety rule and this plan's
   scratch directory are the same). `<m>` is the work item id in lower
   case (`m1a0`, `m1a`, `m1b1`, `m1b2a`, `m1b2b`, `m2`, ..., `m12a`,
   `m13b`, `m17a`, `m18b`). All cargo runs on the host use
   `CARGO_TARGET_DIR=$LUNIX_HOST_BASE/<m>/target` and `-j 4` (this
   overrides CONVENTIONS' "unset CARGO_TARGET_DIR": that rule exists for
   sandboxes, and here the variable is set on purpose, per milestone).
   `tools/remote.sh cargo ...` does both. Test threads: `--test-threads 4`
   at most.
3. **Scratch must be on a real disk file system** (ext4 or xfs), never
   tmpfs: the journal and segment tests use `O_DIRECT`. A test that finds
   its scratch on tmpfs SKIPs loudly (exit 77 in scripts; `eprintln` and
   early return in `#[test]`s that cannot be ignored).
   **Disk budgets.** The test host's root file system had about 336 GB
   free on 2026-11-12. **The cap (stated once, here):** Lunix test use is
   limited to **about 200 GB in total**, which is that free space minus
   `LUNIX_MIN_FREE_GIB` (default 120) and leaves headroom for other work on
   the host. The cap and `LUNIX_MIN_FREE_GIB` move together: the lead's
   brief changes both or neither, and no other section restates the figure:
   - unprivileged scratch ≤ 15 GiB per milestone (at most 4 run at once:
     60 GiB); cargo target directories ≤ 8 GiB each (`cargo clean` the
     largest when over; 32 GiB);
   - root runs are serialised, so one at a time, each ≤ 100 GiB peak:
     M10 ≤ 40, M12a smoke ≤ 8, M15 ≤ 100, M16 ≤ 100, M18b ≤ 80 (one file
     system at a time);
   - every root script (a) calls `tools/remote.sh df-guard <path> <GiB>`
     before **every** `mkfs`, which refuses unless the file system keeps at
     least `LUNIX_MIN_FREE_GIB` (default 120) free afterwards; (b) deletes
     the volume and recreates it between cells by default; (c) sets
     `journal_max` explicitly (256 MiB for M18b, 512 MiB for M15, 1 GiB
     for M16; the 2 GiB default is for production); (d) runs under an outer
     `timeout -k` that signals the script, never a daemon (W8); (e) records the disk it used in its report.
   Delete large files at the end of a session (keep `raw/` results).
4. **Root and ublk tests are serialised across the whole project.** Only
   M10, M12a (its device smoke test only), M15, M16 and M18b (`mkfs`, loop
   devices and mounts only; no ublk) need root, and all but M18b need
   `ublk_drv`. They run **only** through
   `tools/remote.sh root ...`, which takes the host-wide lock
   `$LUNIX_HOST_BASE/root.lock` (`flock`, waits up to 1 h, prints who holds
   it), runs `spikes/common/ublk-leftovers.sh` before and after, and
   refuses to start if leftovers exist that the caller did not create. Its
   outer timeout follows W8: it signals the **script** with `TERM`, never
   the daemon process. At most one root run exists on the host at any time, so a milestone that
   waits for the lock does something else (unit tests) meanwhile. Root
   tests are `#[ignore]` (Rust) or scripts that check `LUNIX_ROOT_TESTS=1`
   and SKIP otherwise.
5. **ublk safety rules.** Follow CONVENTIONS "Safety rules for ublk work on
   a dev host" in full, plus the two wedge lessons of phase 0
   (`bench/ublk-recovery/RESULTS.md`, "QUIESCE handover" and P0.12):
   - **W1.** Never send `STOP_DEV` after `QUIESCE_DEV`. After a successful
     quiesce the daemon exits (the cdev closes, the device goes
     QUIESCED). libublk's `run_target()` sends `STOP_DEV` when queue
     threads end and deadlocked the kernel thread; it is not used on any
     path.
   - **W1a.** `DEL_DEV` of a LIVE device is a `STOP_DEV`. It is sent only
     after a successful `TRY_STOP_DEV`, or by another process for a device
     without a server. Never by the serving process as a fallback
     (`delete_by_id` refuses a LIVE device unless forced).
   - **W2.** Never stop a device and never exit while started requests are
     not being completed. A fatal internal error in a queue thread calls
     `std::process::abort()` (equivalent to `kill -9`, which phase 0
     proved recoverable through `USER_RECOVERY_REISSUE`), never a graceful
     exit path. `abort()` is the single sanctioned exit with started
     requests (ADR-16).
   - **W3.** Bounded test I/O: every fio run has `--runtime` or `--io_size`,
     every memory ring in a test is larger than the I/O volume written
     before a flush, and no test writes more than its scratch budget.
   - **W4.** Watchdogs on every control command (`ADD_DEV`, `START_DEV`,
     `START_USER_RECOVERY`, `END_USER_RECOVERY`, `QUIESCE_DEV`,
     `TRY_STOP_DEV`, `DEL_DEV`, `UPDATE_SIZE`): a timeout (default 30 s,
     `DEL_DEV` 60 s) after which the caller reports, stops the driver
     chain and sends no `SIGKILL` to a daemon that is stopping (wait 30 s
     instead). If a daemon in D state ignores `kill -9`, that is a
     finding: stop, write it down, and do not retry destructively. Only
     `umount` of the dead file system is allowed afterwards.
   - **W5.** `DEL_DEV` with a mounted file system blocks until `umount`:
     every harness unmounts first, and no queue thread ever issues a
     control command that can block.
   - **W6.** `BATCH_IO` is never combined with `QUIESCE`; copy mode is the
     default. `USER_COPY` is opt-in per volume (ADR-17).
   - **W7.** The daemon's own files, log and swap are never on a device it
     serves (`mlockall`, `PR_SET_IO_FLUSHER`).
   - **W8.** Every root script runs under an outer timeout (the wrapper
     in `tools/remote.sh root` enforces a default per script and the script
     states its own cap). The timeout signals the **script** (`TERM`), whose
     `trap` runs stop-fio, then `umount`, then detach with the W4
     watchdogs; the wrapper never signals the daemon process itself (a
     `SIGKILL` reaching a daemon blocked in `DEL_DEV` with a mounted file
     system is exactly what W4 forbids). The `-k` grace is at least the
     `DEL_DEV` watchdog (60 s) plus 60 s. A script still alive at `-k` is
     reported as a wedge (W9) and the wrapper writes the leftovers file, so
     the next run refuses to start. A wall-clock budget is stated for every
     loop (for example M15's crash-point runs), and a run that exceeds it is
     aborted and reported, not extended.
   - **W9.** At the top of every root milestone (M10, M12a, M15, M16, M18b):
     **stop and report, never retry** a wedged command. The `root` wrapper
     refuses to start if the previous run's leftovers file is non-empty.
6. **Host capacity.** The host has about 16 vCPUs and 30 GiB of RAM, about
   336 GB free on the root file system (see rule 3 for the budgets), and is
   shared. The lead runs at most **4 milestones concurrently**; each uses
   `-j 4`. Performance numbers (M3, M5, M16) are taken only when the lead
   says the host is quiet; every other milestone's gates are correctness
   gates, or relative gates ("within 1.5x of the spike measured in the
   same session") that tolerate noise.
7. **Host prerequisites** (the lead checks them once, before M1, and
   installs what is missing): rustup with toolchain 1.98 (BASELINE's host
   had 1.96; the lead installs it), network access to
   `github.com/replicix/replicix-core` for the git dependency (else the
   lead provides a vendored copy), `fio >= 3.36`, `e2fsprogs`,
   `xfsprogs`, `python3`, `jq`, `rsync`, `flock`, and for M15/M16 xfstests,
   PostgreSQL with `pgbench`, and the VM images M16 names. A missing tool
   is reported as "blocked: <tool>" at the start, not worked around.

### Git workflow for parallel agents

1. Each milestone works in its **own git worktree** on branch
   `p1/<milestone>` (for example `p1/m5-map`), created by the lead from
   the commit that carries the milestone's dependencies:
   `git worktree add ../lunix-wt/m5-map -b p1/m5-map <base>`. Work in that
   worktree only.
2. **Commits are allowed and required** (this overrides CONVENTIONS rule
   1 for this plan): small, signed (`git commit -S`), with a message of the
   form `<crate>: <what changed>`, **no trailers** (no `Co-Authored-By`).
   Signing needs whatever environment the lead's brief names. Never push,
   never force-push, never touch `main` or another milestone's branch,
   never rewrite a commit that is already merged.
3. **Ownership.** A milestone edits only the paths in its "Owns" line.
   `tools/check-ownership.sh <m>` (written in M1.A) compares the branch
   against its merge base and fails on any other changed path. Exceptions
   that are always allowed:
   - the milestone's report, `docs/plans/v1/reports/p1-<m>.md`;
   - `crates/<owned crate>/README.md` (at most 40 lines);
   - a new external dependency: add it to `[workspace.dependencies]` in
     the root `Cargo.toml` (exact version, `=x.y.z`) and let cargo update
     `Cargo.lock`, in one separate commit that touches only those two
     files and is titled `deps: add <crate> =<version> for <milestone>`.
     M1 pre-declares every dependency the plan expects, so this should be
     rare. On a rebase conflict in `Cargo.lock`, take main's version and
     re-run `tools/remote.sh cargo build` to regenerate.
4. **Frozen crates.** `crates/lunix-format` and `crates/lunix-iface` are
   changed after M1 **only through the lead**. A milestone that needs a
   change writes it in its report under "Requests to the lead" with the
   exact diff it wants, works around it locally if it can, and does not
   edit the crate. The lead lands the change on `main` and the waiting
   branches rebase. A change to `lunix-format` after M1 is a change to a
   frozen format and needs the maintainer; additive changes to
   `lunix-iface` (new method with a default, new type) do not.
5. **"Done" for a milestone** means all of:
   1. every gate in the milestone is green on the host, run at the
      **rebased head** (record the command, the commit id and the result
      in the report);
   2. `tools/ci.sh` is green on the host (`cargo fmt --all --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`, the ownership check). The workspace builds
      with every other crate as it is on `main`;
   3. the branch is **rebased on current `main`** (linear history, so the
      lead can fast-forward it), and gates were re-run after the rebase;
   4. all commits are signed and have no trailers;
   5. the report `docs/plans/v1/reports/p1-<m>.md` is committed as the last
      commit, and its text is also the final message.
6. The lead merges in dependency order, runs the independent review where
   the table says "required", and tells waiting branches to rebase. No
   wave 1 branch merges before `FREEZE.md` carries the maintainer's
   sign-off line.
7. **Independent review** means a reviewer who did not implement the
   milestone gets the diff and the milestone text, not the implementer's
   conclusions, and writes findings against the milestone's checklist. A
   milestone with "required" review is not merged until the findings are
   resolved or accepted by the lead.

8. **replicix-core work (M18a only).** The repository is
   `/home/bra/cvs/replicix/replicix-core`, a separate git repository
   that Constellation also depends on. M18a works in a worktree of it on
   branch `p1/m18a-zdict`, with the same commit rules, never pushes, and
   keeps `replicix-chunkfmt` changes additive. **The lead announces the
   change to the Constellation maintainers' session before it merges**; the
   lead merges and then bumps Lunix's `rev` in a `deps:` commit.

### Pace and priority

Start the long poles first: M1.A0 (its sign-off gates every merge), M11
(engine), M17 (GC), M2 (crypto), M10 (ublk), M3 (journal), M9 (destage). The lead may run fewer milestones at once; any
order consistent with the graph below works.

## Dependency graph

18 milestone ids, 26 work items (M1 has five, M12 and M13 two each, M17
two, M18 two). The lead may run M12a/M12b as one agent.

```
M1.A0 byte layouts, FREEZE draft, attach matrix        [docs; maintainer sign-off before any wave 1 merge]
 │
M1.A  workspace, tools, lunix-format ──(type checkpoint A5)──┐
 │                                                           │
 │                                      M1.B1 lunix-iface (traits, ctl, gc hooks, contract macros)
 │                                                           │
 │                              ┌────────────────────────────┤
 │                              │                            │
 │                       M1.B2a testkit part 1        wave 1a: need B1 only
 │                       (PlainCodec, FlatOverlay,       M2 crypto   M3 journal   M6 index
 │                        FlatMap, MemSpace)             M7 store    M8 space     M13a cli (no fsck/key)
 │                              │                        M18a replicix-core: codec 3 + replicix-zdict
 │                       M1.B2b testkit part 2           M18b dict study, stages 1-2 (loop images; no Lunix)
 │                       (MemJournal, MemIndex,
 │                        MemStore, MemBlockEngine,   wave 1b: need B2a    M4 overlay   M5 map
 │                        contract tests)
 │                              │                     wave 1c: need B2b    M9 destage   M10 ublk
 │                              └─────────────────────────────────────────  M11 engine  M17.A gc (against fakes;
 │                                                                          also needs M5 as a dev-dependency)
 │
 │   cap: 4 milestones at a time (rule 6); start the long poles first.
 │
 ├─ wave 2:
 │    M14  fsck           needs M2 M5 M6 M7
 │    M13b cli fsck/key   needs M2 M7 M14
 │    M12a lunixd         starts when M11 + M3 + M4 + M7 merge and wires the real crates incrementally;
 │                        its gate needs M2 M5 M6 M8 M9 M10 M17.A as well (M13a for CLI-driven tests)
 │    M12b stack tests    same inputs as M12a; exports `lunixd::testsupport` (the in-process volume builder)
 │    M17.B gc on the real stack + fsck integration + oracle churn test
 │                        needs M5 M6 M7 M11 M14 M17.A and M12b's testsupport
 │
 └─ wave 3 (root, serialised on the host):
      M15 exit tests   needs M12a M12b M13a M13b M14 M17.B
      M16 perf + ADR-5 needs M12a M13a M17.B (start after M15's first green fio verify)
      M18b stage 3     one Lunix validation cell; needs M12a M13a M14; any time after M12a
```

GC (M17) sits before the exit tests and before every long-running
measurement on purpose: the churn workloads of M15 and M16 and the crash
loops run against a store that reclaims, and the fsck used by M15 knows
about condemned files.

Crate dependencies (Cargo): `lunix-format` ← `lunix-iface` ← everything
else. `lunix-crypto`, `lunix-journal`, `lunix-map`, `lunix-index`,
`lunix-store`, `lunix-space`, `lunix-overlay` depend on `lunix-format` and
`lunix-iface` only. `lunix-destage`, `lunix-engine`, `lunix-gc` and
`lunix-ublk` depend on `lunix-format` and `lunix-iface` only, plus
`lunix-testkit` as a dev-dependency (engine, destage and gc are built and
tested against the fakes in wave 1); `lunix-gc` also takes `lunix-map` as a
dev-dependency (M17.A needs real pack ids and `rehint`, which `FlatMap`
does not model; `FlatMap` stays the content oracle). `lunix-fsck` depends on format,
iface, crypto, map, index, store and (from M17.B) gc. `lunixd` depends on
every crate. `lunix-cli` depends on iface (the control protocol) and, from
M13b, on crypto (offline key slots) and fsck (library).

## Layout produced by this plan

```
Cargo.toml  rust-toolchain.toml (1.98)  Cargo.lock  .github/workflows/ci.yml
crates/
  lunix-format/    every frozen type, encode/decode, golden vectors
  lunix-iface/     traits and shared types between crates; control protocol; crashpoints
  lunix-testkit/   in-memory fakes and oracles for the traits (dev-dependency only)
  lunix-crypto/    key hierarchy, extent AEAD, metadata sealing, keyring, downgrade check
  lunix-journal/   journal lanes, group commit, recovery scan, reclaim
  lunix-overlay/   dirty overlay with cuts
  lunix-map/       radix Merkle map
  lunix-index/     chunk index (LSM runs)
  lunix-store/     volume directory, segments, map packs, commit and checkpoint files
  lunix-space/     space ledger and ladder
  lunix-destage/   zero, hash, dedup, compress, seal, pack, map update
  lunix-ublk/      raw control ring, queues, recovery, QUIESCE, TRY_STOP, resize
  lunix-engine/    I/O handlers, flush groups, cut and commit, startup recovery
  lunixd/          daemon: config, registry, control server, wiring, signals
  lunix-cli/       `lunix`
  lunix-fsck/      library + `lunix-fsck` binary
  lunix-gc/        mark, liveness, compaction, condemn/grace/recheck/delete (M17)
tools/             ci.sh remote.sh check-ownership.sh ownership.toml
tests/exit/        M15 scripts          tests/perf/  M16 scripts      tests/dictstudy/  M18b
bench/phase1/{exit,perf,chunksize,gc}/   RESULTS.md and raw/ per CONVENTIONS hygiene
bench/dict-study/{lunix,constellation-arm}/   M18b raw summaries (never filetypes/)
docs/reference/on-disk-formats.md     M1.A0: every frozen layout, field by field
docs/reference/attach-matrix.md       M1.A0: expected result per attach combination
docs/reference/gc.md                  M17.A: gc/ body, condemned list, GC settings
docs/plans/v1/reports/                one report per milestone
```

Binaries: `lunixd` (crate `lunixd`), `lunix` (crate `lunix-cli`),
`lunix-fsck` (crate `lunix-fsck`), `lunix-ublk-testd` (crate
`lunix-ublk`, a test daemon over a file-backed engine; M10 only).

## Cross-cutting design rules every milestone obeys

These come from the spec; a milestone that cannot meet one reports it.

1. **Queue threads never block and never run tokio** (ADR-18). Anything
   that waits parks the tag and resumes from a completion. Queue-thread
   interfaces are non-blocking (`try_*`, `submit_*`, `poll`); worker and
   background interfaces may block.
2. **Bounded decode, bounded allocation.** Every decoder checks counts
   against the remaining input and against a stated maximum before
   allocating; decompression is capped by the volume's chunk size, never
   by an `ulen` read from the object; arithmetic on sizes, offsets and
   epochs is checked (DESIGN §15).
3. **Salt rule.** A fresh random 192-bit salt is drawn inside the seal
   function on every call; no API accepts a salt (ADR-11). **Randomness
   comes from the OS** through `getrandom(2)` on every call (extent salt,
   metadata and slot nonces, Argon2id salt, generated root keys), never from
   a userspace DRBG such as `aws_lc_rs::rand::fill` (ADR-11 F3, M2 review
   finding 1). Keys live in `Zeroizing` memory, have no `Debug`, `Clone` or
   `Copy`, and never reach logs.
4. **Redaction.** Secrets (passphrases, key files, KMK, DEK) never appear
   in a log line, an error string, a control-socket response or a report.
5. **Durable order.** An object a commit names is durable (data
   `fdatasync`, rename, directory `fsync`) before the commit is created;
   the journal is reclaimed only after the commit that covers it is
   durable (§4.1, ADR-9). See M11 for the numbered protocol.
6. **Crash points.** Every durable step has a named crash point
   (`crashpoint!("<crate>.<step>")`, defined in `lunix-iface`, compiled to
   nothing unless the `crashpoints` cargo feature is on). With the feature
   on and `LUNIX_CRASH_AT=<name>[:<hit>]` set, the process sends itself
   `SIGKILL` at that hit. The names are registered statically so that
   `lunixd --list-crashpoints` prints them; M15 loops over the list.
7. **Errors:** `thiserror` in libraries, `anyhow` with context in
   binaries; no `unwrap`/`expect`/indexing panics on data read from disk
   or from the control socket.
8. **Comments** explain intent and trade-offs, never narrate code. New
   files carry no license header unless copied from replicix-core (MPL-2.0
   headers are kept on copied files). Reused spike code keeps its
   structure only where the milestone says so.
9. **Both chunk sizes.** Chunk size is a per-volume parameter (16 KiB to
   1 MiB, power of two, ADR-5). Every property and crash test of a
   milestone that sees chunks runs at **16 KiB and 64 KiB**; the test
   parameter is the chunk size, not a constant.
10. **No S3, no tokio on the data path.** Phase 1 has no backend. The
    sealed-object kinds and the commit kinds that only phase 2+ writes
    are defined and round-trip tested in `lunix-format` but have no
    producer in the daemon.
11. **GC safety (M17).** Nothing a GC root can reach is deleted. Every
    delete is condemn → grace → recheck → delete; dedup never targets a
    condemned entry; GC runs on the coordinator between cuts with the
    in-memory map equal to the tip's; a file is unlinked only after the
    commit that stopped referencing it is durable.
12. **Formats change only through M1.A0.** A milestone that needs a byte
    layout the freeze list does not cover asks the lead; it does not decide
    it. The local `epoch` is 0 everywhere in phase 1.
13. **Tamper-resistance rules R1-R4 (normative; DESIGN §6, §7, §8.4, §10.1,
    §15; ADR-11, ADR-31).** The keyed tag of an `integrity` volume binds
    extent bytes only, so these four rules carry the binding; each has a
    gate test in the milestone named.
    - **R1 (M7, M9, M11):** every data read passes the leaf `id_prefix` (or
      the full id) and the volume's chunk size in `OpenExpect` and fails on
      a mismatch; a read without an expectation is a scan and sets
      `verify_id`. Lean volumes (`id_len = 0`) read only as scans.
    - **R2 (M5):** the map recomputes each node's hash against the parent's
      `child_hash` (the commit's `map_root.hash` for the root) and the
      header `chunk_id`, and requires codec 0, for nodes from the local pack
      store too.
    - **R3 (M7; used by M6, M14, M17):** a footer is used only after
      `MetaCodec::open(Footer, "<container name>", bytes)` succeeds, and
      `seg_hash` is recomputed whenever the extent region is read whole
      (fsck, compaction, `catch_up`). A container whose trailer or footer
      fails is absent, never partly used.
    - **R4 (M11, M12, M14):** attach refuses a tip below the local floor
      (`Rollback`; same height, other hash: `Fork`) and opens the lease only
      through `open_lease_fresh(bytes, max(chain epoch, pins.max_epoch))`.


## Common milestone template

Every milestone below states: **Owns** (the only paths it edits),
**Reads** (files to read first), **Depends on**, **Model**/**Review**,
**Steps**, **Gate** (all must be green; thresholds are fixed in advance
and not changed after seeing data, CONVENTIONS rule 4), and **Not to
touch**. Every milestone also ends with the common gate (`tools/ci.sh`
green, rebased, signed, report) from "Done" above, and with the common
report format at the end of this file. "Not to touch" always includes: any
path outside Owns, `docs/explanation/DESIGN.md`, `DECISIONS.md`, and
`lunix-format`/`lunix-iface` (after M1).

---

## M1 — Foundation: layouts, workspace, tools, `lunix-format`, `lunix-iface`, `lunix-testkit`

M1 is five work items in a fixed order, because the frozen byte layouts
are decided **before** any code and reviewed by the maintainer, and
because wave 1 depends on types and traits, not on golden bytes:

| item | what | model | review | starts when |
|---|---|---|---|---|
| M1.A0 | byte tables, `FREEZE.md` draft, attach matrix (docs only) | design-heavy (or the lead) | required, independent, **plus maintainer sign-off** | first |
| M1.A | workspace, tools, `lunix-format` code and goldens, to A0's tables | design-heavy | required, independent (freeze checklist) | A0 drafted (sign-off may still be pending) |
| M1.B1 | `lunix-iface`: traits, `ctl`, `crashpoint!`, contract-test macros | design-heavy | required, independent (needs of M2-M17) | A's type checkpoint (A5) |
| M1.B2a | `lunix-testkit` part 1: `PlainCodec`, `FlatOverlay`, `FlatMap`, `MemSpace`, proptest strategies | routine | lead skim | B1 merged |
| M1.B2b | `lunix-testkit` part 2: `MemJournal`, `MemIndex`, `MemStore`, `MemBlockEngine`, device oracle, contract tests for every fake | design-heavy | required with B1's review | B2a merged (may overlap) |

Maintainer sign-off on A0's layouts may overlap wave 1: wave 1 branches
start from B1 and use `lunix-format`'s types, but **no wave 1 branch is
merged before the sign-off line in `FREEZE.md` is filled**, and a golden
that changes after sign-off is a format change that needs the maintainer.

**Reads (all of M1):** ROADMAP top paragraph; DESIGN §2, §4.1, §5-§8,
§11, §13.3, §15; ADR-4, 7, 8, 9, 11, 12, 19, 25, 26, 28, 30;
`bench/crypto-kat/raw/kat-vectors.json` and
`spikes/crypto-kat/src/{extent,kdf,meta,slots}.rs`;
`spikes/radixmap/src/{leaf,map}.rs` and `spikes/leafenc/src/codec.rs` (the
leaf codec `rb16`); `spikes/ublk-recovery/src/memsink.rs` (record header
idea); replicix-core `crates/chunkfmt` (`CCH1`, codec registry) and
`platform-lite`; `docs/plans/v1/wip/01-p1-review.md`.

### Decisions already taken (do not re-open)

Every row below was decided by the maintainer on 2026-11-12 or adopted
from the independent review. A0 copies this table verbatim into
`docs/reference/on-disk-formats.md` ("Decided before A0") and designs
the byte layouts to fit it. A0, A and B1 do not re-decide any row; a
conflict is reported to the lead, not resolved locally.

| id | decision |
|---|---|
| D1 | `map_fanout` is a descriptor field: 256 or 512 legal, default 512. Interior map nodes are stored raw, through the extent codec (`kind = node`, codec 0 in phase 1; codec 1 zstd later needs no format change). The node header carries `level` and `fanout` only, **no codec byte**. |
| D2 | The stored `CCH1` header keeps its 2-byte codec id and 8-byte `ulen`. The AAD and the key-derivation `info` use the KAT vectors' canonical 1/1/4/4-byte widths. The AAD is a canonical re-encoding, not the header bytes (safe: chunk ≤ 1 MiB, codec ≤ 255). |
| D3 | Index runs are bucket objects `index/<pack:016x>` (§6.1 gains the prefix) with their own run format. **Lag rule:** `index_packs[]` of FULL *n* lists the runs that were durable before step C6 of *n*; they cover segments named by commits ≤ *n−1*. The run written at C8 of *n* is named by FULL *n+1*. |
| D4 | All byte layouts are decided in M1.A0, with maintainer sign-off, before any code. |
| D5 | Genesis commit: `commit_seq` 0, `parent_seq` none, `parent_hash` = BLAKE3 of the exact `volume` object bytes. Every later commit has `commit_seq = parent_seq + 1`. The commit counter is independent of the `write_seq` (FULL carries both `commit_seq` and `cut_seq`; `cut_seq` is a `write_seq`). Field names are `commit_seq`, `parent_seq`, `cut_seq` and `write_seq`; the `lunix-format` types are `CommitSeq` and `WriteSeq`, never a bare `Seq`. `chain_floor` is the `commit_seq` of the oldest retained FULL commit. Phase 1 retention is `last:N` FULL commits, default N = 2 (the tip and its predecessor), so the floor moves with every FULL once N FULLs exist; it is not genesis. Mount and fsck verify the chain from the tip down to `chain_floor`; the floor commit's parent may be absent. |
| D6 | `heat/`, `gc/` and `dicts/` freeze **envelope and kind id only**; the body is `{ver: u16, bytes}` defined by the phase or crate that writes it (`gc/` by M17 in phase 1: one sealed create-only object per GC pass, `gc/<pass:016x>`, body `{ver: 1, bytes}`; `dicts/` by `replicix-zdict`, M18a). |
| D7 | Journal lanes: one lane per ublk queue, a global `write_seq` taken from one global counter by the writer's lane at `try_reserve` (never consumed by a refused reservation), lanes merged at recovery. Local `epoch` is 0 in every journal record and every commit. FLUSH is a **group flush** across lanes (interface in M1.B1). Recovery is floor-based: `recover(hint, floor)` with `floor` = the tip FULL's `cut_seq`. |
| D8 | Local file set: `volume` (the exact descriptor bytes), `volume.json` (local settings only), `families/<id>/keyring`, `state/device` (with `boot_id`), `state/checkpoint`, `state/pins`, the host registry `<state_dir>/volumes.json`; plus, new in this plan, the local mirrors of the GC-root objects `snapshots/`, `readers/`, `holds/` (empty in phase 1) and `gc/` (one sealed evidence object per pass, `gc/<pass:016x>`) and the local-only `state/gc/` (condemned list, the only mutable GC state, atomic replace). |
| D9 | The object name inside every metadata AAD is **bucket-relative** (`commits/<commit_seq:016x>`, `segments/<seg:016x>`, ...), never the local path, so local bytes upload verbatim in phase 2 (ADR-7, §13.3). |
| D10 | EPOCH body includes `authority` (optional). Lease body includes `transferred_to` (optional, best effort, ADR-24 step 6). `snapshots/`, `readers/` and `holds/` bodies name their §6.1 fields in the freeze list (`{seq, created, consistency, label}`, `{seq, deadline}`, `{base_seq}`). |
| D11 | **Plain-extent tag.** Every extent carries the 16-byte tag field. On unencrypted volumes it is BLAKE3-128 (first 16 bytes) over **every byte of the extent before the tag**, that is `CCH1 header ‖ chunk_id ‖ clen ‖ payload`, taken as one contiguous slice (`&extent[..len-16]`), **keyed** by a key derived from the integrity key (purpose `extent tag`) in `integrity` mode, unkeyed in `plain` mode (D16). Local reads and fsck verify it. (Decided by the lead on 2026-11-12, review 2 finding 1: this is the spec's wording, DESIGN §6, ADR-6, ADR-11 and GLOSSARY. It covers `clen`, which sits outside the 16-byte `CCH1` header, at the same cost, and matches the encrypted layout, whose AEAD likewise covers its framing.) On encrypted volumes the field is the AEAD tag as before. |
| D12 | **Journal framing and packing (DESIGN §4.1).** Each record is a 64-byte inline header plus its data, copied into the lane's own aligned buffer. Records are packed back to back, 8-byte aligned, inside one lane batch; only the **batch** is padded with zeros to a 4 KiB multiple and written with one `O_DIRECT` write. A zero magic means end of batch. On a CRC or header failure the reader **resyncs** (D17); recovery stops at the first gap in the merged `write_seq` order. Cost: n 4 KiB writes take ceil(n·4160/4096) blocks; an isolated QD1 4 KiB write costs 8 KiB. |
| D13 | Control protocol: length-prefixed JSON frames (u32 big-endian length, then a JSON object). |
| D14 | `max_io_buf_bytes` is a per-volume attach option (default 1 MiB, minimum 256 KiB). |
| D15 | GC is in phase 1 (M17): roots are the retained commits, the tip FULL plus its DELTAs, and snapshot, reader and hold pins (all pin sets empty in phase 1 but read). Deleting is condemn → grace → recheck → delete, for segments, map packs, index runs and commits below `chain_floor`. `retain_last` (`last:N`, default N = 2: the tip FULL and its predecessor) is a `volume.json` setting; `chain_floor` is the older retained FULL, and commit objects below it are deleted after the grace. |
| D16 | **Three volume modes (maintainer, 2026-11-12).** The descriptor's crypto mode is `encrypted`, `integrity` or `plain`. `integrity`: an HMAC on every commit and on the lease, keyed extent tags, an integrity key in the keyring. `plain` (mode 0): unkeyed BLAKE3 checksums and tags, no key material, no key slots; it detects corruption only and gives no protection against anyone who can write the bucket. Plain is supported everywhere, including volumes with S3 backends; the CLI and docs warn about its limits when it is chosen and when a backend is added to a plain volume. **Default:** `volume create` without `--encrypt` or `--plain` makes an `integrity` volume (flags `--encrypt`, `--integrity`, `--plain`). It auto-creates a key-file slot (kind 2) with a random key at `<dir>/<uuid>/integrity.key` (0600) for a local volume, so attach needs no extra step; `--passphrase` adds a passphrase slot for volumes that move. **Attach:** a plain volume needs no credential; a credential supplied for one is `Downgrade`, as is a pin recording a stronger mode than the descriptor. The descriptor of a plain volume carries an unkeyed BLAKE3 checksum in place of the MAC. (ADR-11 amended; `wip/01-p1-m1a0-review.md`, open question.) |
| D17 | **Spec-level rules from the M1.A0 review.** (1) **Dedup confirmation:** a dedup hit taken from a persisted index run is confirmed against the extent header's full 32-byte `chunk_id` and the extent's tag or AEAD before the map points at it; an unconfirmed hit is a miss; index runs are checksummed, not authenticated (ADR-6, ADR-9, DESIGN §5 step 4). (2) **`created_at`** is a writer clamp, `max(now, parent.created_at)`, not a chain validity rule; mount and fsck never reject on it, fsck warns; `epoch`, `commit_seq` and `chain_floor` stay reader rules (DESIGN §8.2). (3) **Journal resync:** on a CRC or header failure at `p` the reader scans forward from `p + 8` in 8-byte steps to the next header that passes magic, `ver`, `gen`, `off`, CRC and `write_seq` above the last accepted; the lane ends only when none exists; global `write_seq` contiguity keeps the stop semantics for damage above the floor (DESIGN §4.1, ADR-4). (4) **Checkpoint:** lane tails are used only if `last_commit_seq ≤` the tip's `commit_seq`; `fsck --repair --truncate-tip` zeroes both checkpoint slots. **ObjId allocator:** next id = 1 + max(checkpoint `id_high`, highest id named by the chain, highest id present before the orphan sweep) (DESIGN §4.4). (5) **Thin clones:** interior map child entries reserve `lineage_idx` (u8, top byte of the 4-byte `len`, 0 in v1, rejected if non-zero) for ADR-25 (DESIGN §7). |
| D18 | **Keyed chunk ids in `integrity` mode (maintainer, 2026-11-12, after the M2 security review).** `chunk_id = blake3::keyed_hash(addressing_key, plaintext)` in `encrypted` mode (key from the KMK) and in `integrity` mode (key from the integrity key by its own purpose id); `plain` stays unkeyed. It closes the `id_len = 16` collision search (no one without the key can compute ids). Map node hashes are unchanged. M2 implements it; `lunix-format` changes only where the layout doc's worked examples change (the key-purpose table gains one row, item 11; integrity-mode extent goldens carry keyed ids). ADR-6, ADR-11, DESIGN §5 step 3, §6, §7, §15, GLOSSARY. |
| D19 | **Rollback floor (maintainer, 2026-11-12: adopt the review's recommendation; ADR-31).** `state/pins` is version 2 and gains `{max_commit_seq, tip_hash}`. Raise-only; written after every verified attach (the attach's EPOCH commit included) and after every FULL commit is durable (C6), never before. Attach refuses a lower tip as `Rollback` and the same height with another hash as `Fork`; only offline `fsck --repair --truncate-tip` and an explicit operator reset lower it. Phase 5 adds the maximum over mirrors and a raise-only Kubernetes floor; a notary bucket is the documented later upgrade. A host with no floor, and a purely local volume against a writer of its disk, are unprotected, and the docs say so plainly. `state/pins` v2 and D18's purpose id are **amendments to frozen items and need the maintainer's re-sign of `FREEZE.md`** (items 11 and 14). |
| D20 | **Rules R1-R4 are normative** (cross-cutting rule 13). The hostile-keyring cost policy (a per-slot Argon2id memory ceiling, default 256 MiB, and a total budget over the slots tried) and OS randomness through `getrandom(2)` (rule 3) are spec statements too (ADR-11 amendment of 2026-11-12). |

### M1.A0 — byte layouts, FREEZE.md draft, attach matrix

**Owns:** `docs/reference/on-disk-formats.md`,
`docs/reference/attach-matrix.md`, `crates/lunix-format/FREEZE.md`
(draft: the checkboxes and the golden-file names). **Depends on:**
nothing. **Model:** design-heavy, or the lead. **Review:** required,
independent, then maintainer sign-off. No code.

**Steps:**

**Review of A0 and the open question: resolved (2026-11-12).** The
independent review is `wip/01-p1-m1a0-review.md` (verdict: ready with
fixes). Its open question, whether `plain` mode (mode 0) is offered, is
**resolved by the maintainer**: three volume modes, `integrity` as the
default, `plain` supported everywhere with warnings (D16). The review's
majors 1, 6, 8, 9, 10 and 13 are folded in as D17 and the freeze items
below; A0's revision applies them, recomputes the worked examples that
move, and adds the plain-mode rows to the attach matrix. The minors are
fixed in the same commit.

**Binding corrections from review 2.** The tables A0 writes follow
finding 1 and finding 7: (1) the plain-extent tag input is every byte of
the extent before the tag, one contiguous slice, including `clen` (D11;
the freeze-list item 3 and the worked plain-extent example say so); (2)
the `gc/` envelope is one sealed create-only object per pass,
`gc/<pass:016x>`, body `{ver: 1, bytes}`, written once at pass end, and
`state/gc/condemned` is the only mutable GC state (D6, freeze item 15, and
the paths module of item 12). Field names follow D5.

1. Write the decisions table above into `on-disk-formats.md`.
2. For each item 1-15 of the freeze list in M1.A, write the byte-level
   tables: field name, offset, width, encoding, legal range, reserved
   bits (must be zero), maximum counts, and what is authenticated. One
   worked hex example per type (hand-built, not generated) for at least:
   journal record, extent (plain and encrypted), segment trailer, FULL
   commit, `volume` descriptor.
3. Decide the layouts the spec leaves open and list them in a section
   "Decided in A0" with the reason: the journal file header and record
   (magic, fields, CRC coverage, the packing rule of the note below), the
   segment footer and trailer, the commit envelope and the five bodies,
   `volume`, keyring, lease, checkpoint, pins, device record, index run,
   map node header, and the `gc/` and local `state/gc/` envelopes. Every
   variable-length structure states its maximum count.
4. **Journal packing (D12, settled in DESIGN §4.1).** Write the batch
   layout as decided: 8-byte-aligned records inside a batch, zero padding
   to a 4 KiB multiple, zero magic ends a batch, recovery stops at the
   first gap in the merged `write_seq` order, with the D17 resync past a
   CRC or header failure (records carry the file `gen` and their own `off`,
   so a guest payload, a stale record or a relocated record cannot pass).
   State the cost (n 4 KiB writes take
   ceil(n·4160/4096) blocks; an isolated QD1 4 KiB write costs 8 KiB) and
   how a reader finds batch boundaries (A0 decides: for example a batch
   header or the 4 KiB grid plus the zero-magic rule, and says why a
   record cannot be mistaken across a torn batch).
5. Write `attach-matrix.md`: the hand-derived expected result for every
   combination of descriptor mode (`encrypted` | `integrity` | `plain`) ×
   credential supplied or not × descriptor MAC valid/invalid × KMK
   fingerprint right/wrong/absent × local pin right/wrong/absent, from
   ADR-11 F7 and D16 (a plain volume needs no credential; a credential
   for it, or a pin stronger than the descriptor, is `Downgrade`). M2
   loads it as the expected table; M2 does not derive it.
6. `FREEZE.md`: the 15 items with a checkbox each, the golden file names
   they will have, and a last line "Signed off by: ____ on ____" that only
   the lead fills in after the maintainer's review.

**Gate (A0):** every freeze item has a table; every field has an offset
(or an explicit variable-length rule) and a range; the decisions table is
verbatim; the worked examples decode by hand to the stated values (the
reviewer decodes two); `attach-matrix.md` has one row per combination
(3 modes × 2 × 2 × 3 × 3 = 108 rows, or fewer with a stated reason for
each merged row); the independent review has no unresolved blocker; the sign-off
line is filled before any wave 1 branch merges.

**Not to touch:** code, DESIGN.md, DECISIONS.md.

### M1.A — workspace, tools, `lunix-format`

**Owns:** root `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`.gitignore`, `.github/`, `tools/`, `crates/lunix-format` (except
`FREEZE.md`'s text, which A0 wrote and A only ticks), and a skeleton
(`Cargo.toml` with its final dependency list, including `lunix-gc`'s
dev-dependency on `lunix-map`, `src/lib.rs` with a one-line
doc) of **every other crate in the layout**. **Depends on:** A0 drafted.
**Model:** design-heavy. **Review:** required, independent, against the
freeze checklist below.

1. **Workspace.** Root `Cargo.toml` (`resolver = "2"`, members
   `crates/*`), `rust-toolchain.toml` channel `1.98` (as replicix-core),
   edition 2021, `[workspace.dependencies]` with **exact** pins
   (`=x.y.z`) of everything the plan expects: `libublk =0.4.8`,
   `io-uring =0.7.16`, `blake3`, `aws-lc-rs =1.19.0`, `chacha20poly1305`,
   `argon2`, `hmac`, `sha2`, `zeroize`, `lz4_flex`, `zstd 0.13`, `crc32c`,
   `xxhash-rust`, `proptest`, `thiserror`, `anyhow`, `serde`,
   `serde_json`, `clap`, `tokio`, `tracing`, `libc`, `rustix`, `inventory`,
   `hdrhistogram`, `tempfile`, plus
   `replicix-chunkfmt = { git = "https://github.com/replicix/replicix-core", rev = "6fb59d2", features = ["lz4"] }`
   and `replicix-platform-lite` at the same `rev`. Record the resolution
   date in the report. Lock file committed. `[profile.release]
   debug = 1`; a `[profile.test] opt-level = 1` so property tests are
   usable. (M18a may later move the pin to a replicix-core revision that
   carries the codec-3 code; that is a `deps:` commit by the lead.)
2. **Tools** (all shell, shellcheck-clean):
   - `tools/remote.sh` with subcommands `sync` (rsync the worktree, minus
     `.git` and `target`, to `$LUNIX_HOST_BASE/<m>/src`; `--delete` only
     inside that directory), `cargo <args>` (sync, then run cargo there
     with `CARGO_TARGET_DIR` and `-j 4` for build/test/clippy), `run
     <cmd>`, `fmt` (runs `cargo fmt --all` remotely and pulls back only
     `*.rs` files under the milestone's owned paths from
     `tools/ownership.toml`), `root <cmd>` (the lock and leftovers wrapper
     of rule 4; its outer timeout signals the script with `TERM`, never the
     daemon, with a `-k` grace of at least 120 s, and writes the leftovers
     file when the script outlives it: W8), `leftovers`, `df`,
     and `df-guard <path> <GiB>` (rule 3: exits non-zero unless the file
     system holding `<path>` would still have at least
     `$LUNIX_MIN_FREE_GIB` (default 120) GiB free after `<GiB>` more are
     used; every root script calls it before every `mkfs`). It refuses to
     run if `LUNIX_HOST` or `LUNIX_HOST_BASE` is unset or if `<m>` is not
     given (`LUNIX_M` or `-m`). It never prints the host name. It exports
     `LUNIX_SPIKE_DIR=$LUNIX_HOST_BASE/<m>/scratch` for every command, so
     CONVENTIONS' scratch-directory safety rule and this plan's scratch
     directory are the same directory.
   - `tools/ci.sh`: fmt check, clippy `-D warnings`, `cargo test
     --workspace`, `tools/check-ownership.sh`, and the feature-edge check:
     run `cargo tree --workspace -e normal,build -f '{p} {f}'` and fail if
     any line for a crate other than the one that declares the feature as
     its own (`lunix-crypto` for `test-rng` and `tripwire`; `lunix-iface`
     for `crashpoints`) shows `test-rng`, `tripwire` or `crashpoints` in
     its feature list. Features may appear only on `dev-dependencies`
     edges and in the explicit `crashpoints` build of `lunixd`
     (`cargo build -p lunixd --features crashpoints`, which the script
     builds separately and does not run tests with `--all-features`).
     The script's own test feeds a fixture `cargo tree` output with a
     violation and checks that it fails.
   - `tools/ownership.toml` (milestone id → allowed path globs, from the
     "Owns" lines of this plan; ids `m1a0 m1a m1b1 m1b2a m1b2b m2 ... m11
     m12a m12b m13a m13b m14 m15 m16 m17a m17b m18b`) and
     `tools/check-ownership.sh <m>` (changed files against `git merge-base
     HEAD main` ⊆ allowed globs ∪ the always-allowed list of the git
     workflow section). Test it with a synthetic diff in `tools/tests/`.
     `m18a` has no entry: its repository is replicix-core. Two milestones
     may own paths inside one crate only if the globs are disjoint:
     `m17a` lists `crates/lunix-gc/**` minus `crates/lunix-gc/tests/**`,
     `m17b` lists `crates/lunix-gc/tests/**` (and no other `lunix-gc`
     path); `m12a` lists `crates/lunixd/**` minus `tests/stack_*.rs`
     (it creates `src/testsupport.rs` as a stub), `m12b` lists
     `crates/lunixd/tests/stack_*.rs` and `crates/lunixd/src/testsupport.rs`.
     Files that a later item fills are created by the earlier item with
     their final signatures (M12a, M13a, M14 below), so the later item
     edits only those files and never `mod` lines, `Cargo.toml` or
     `main.rs`. The globs of `m14` include `crates/lunix-fsck/src/gc.rs`
     (the stub), those of `m13a` include `src/fsck.rs` and `src/key.rs`.
   - `.github/workflows/ci.yml` runs `tools/ci.sh` on a hosted runner
     (unprivileged tests only).
3. **`lunix-format` rules.** Pure Rust, no I/O, no crypto operations, no
   `unsafe`, `#![forbid(unsafe_code)]`. For each type: `encode(&self,
   &mut Vec<u8>)` or into a slice, `decode(&[u8]) -> Result<Self,
   FormatError>`, the fixed byte layout of A0's tables (little endian
   unless a field says otherwise), a version byte, explicit maximum counts,
   and reserved fields that decode only when zero (so a later phase can use
   them: unknown non-zero reserved bits are an error, not ignored). A
   layout question that A0's tables do not answer goes to the lead; A does
   not decide it.
4. **The freeze list** (everything ROADMAP's top paragraph names, plus this
   plan's additions). Implement each exactly as A0's tables say; the
   items are listed here with the content they must cover. M1 may not
   change a field list the spec gives.
   1. **Primitives:** `ChunkId([u8;32])`, `IdLen` (0, 16, 32), `IdPrefix`,
      `SegId` (`epoch << 40 | counter`, checked), `PackId`, `WriteSeq` (write
      sequence) and `CommitSeq` (commit counter; distinct types, D5),
      `Epoch`, `DekGen(u32)`, `FamilyId([u8;16])`, `VolumeUuid([u8;16])`,
      `Codec` (0 raw, 1 zstd, 2 LZ4, **3 zstd with dictionary**, matching
      replicix-chunkfmt), `ExtentKind` (data 0, map node 1), `Hint {seg,
      off, clen}`, `LineageIdx(u8)`.
   2. **Journal file and record** (§4.1, D7, D12): the 64 MiB ring file
      header (magic, version, volume uuid, lane id, file ordinal,
      generation), and the record `{magic, write_seq, epoch (0), lba, len,
      kind(write|hole), crc32c}` with its 64-byte inline header (A0 adds the
      file `gen` and the record's own file offset `off`, which the resync
      rule of D17 checks), CRC over header and data; a hole record has `len` and no data. The same
      framing is reused for DELTA inline records and the future memfd sink.
   3. **Extent** (§6, ADR-11, D2, D11): `CCH1` header as in
      replicix-chunkfmt, then `chunk_id`, `clen`, and for encrypted volumes
      `dek_gen(4) + salt(24) + nonce(12)`, payload, `tag(16)` (present in
      **every** extent, D11). `clen` follows `chunk_id` and is outside the
      16-byte `CCH1` header. Provide the **AAD builder** (`ver ‖ codec ‖
      level ‖ ulen ‖ kind ‖ dek_gen ‖ family_id ‖ id_len ‖ chunk_id`), the
      **key-derivation `info` builder** (`"lunix extent v2\0" ‖ family_id ‖
      kind ‖ dek_gen ‖ codec ‖ level ‖ clen ‖ id_len ‖ chunk_id ‖ salt`) and
      the **plain tag input**: every byte of the extent before the tag,
      one contiguous slice `&extent[..len-16]` (header, `chunk_id`, `clen`,
      payload; D11). The AAD and
      `info` must reproduce the bytes `aad_hardened`/`info_v2` of
      `bench/crypto-kat/raw/kat-vectors.json` exactly (copy the file to
      `crates/lunix-format/tests/vectors/`); they use the canonical
      widths of D2.
   4. **Segment** (§6): extents, footer (sorted `[chunk_id, offset, clen,
      ulen, codec]`, zstd-compressed, plus segment-level BLAKE3 and `write_seq`
      range; sealed with the `footer` metadata key when encrypted, so the
      footer type has a plain and a sealed form), trailer (footer offset,
      length, magic, version). Map pack: the same container with
      `kind = node` payloads.
   5. **Map node** (§7, ADR-8 as amended, D1): the leaf codec `rb16`
      (segment dictionary of `(lineage_idx: u8, seg)`, runs, bit-packed
      `clen`, id-prefix column, restart point every 16 entries, holes as
      runs, no outer compression); the interior node `{child_hash,
      child_location {pack, off, len}}` where the node hash covers child
      hashes and level only, never locations; the empty-node hash; a node
      header carrying `level` and `fanout` only. Fanout is a **descriptor
      field** (see 8). The interior entry reserves `lineage_idx` (u8) in
      the top byte of the 4-byte `len` (a u24 `len` plus the byte; 0 in v1,
      rejected if non-zero; D17), outside the node hash.
   6. **Commits** (§8, ADR-9, ADR-11, ADR-24, ADR-28, D3, D5, D10): the
      five kinds FULL, DELTA, EPOCH, CONFIG, TRANSFER, each with `epoch`,
      `parent_seq`, `parent_hash` and (unencrypted volumes) `mac`; FULL
      with `cut_seq, size, map_root, map_packs[], new_segments[],
      index_packs[], chain_floor, heat?, created_at` (`created_at` is a
      writer clamp, D17; the decoder never rejects on it); DELTA with inline
      records and `refs: [{kind: segment | journal, id, hash}]`; EPOCH with
      `authority?`; CONFIG with `config_version, authority, backends[{id,
      url, role, state, since_seq}], policy, detached?`; TRANSFER with
      `to, epoch, config_version, reason`. Object envelope for commits:
      magic, version, kind, then the body; a sealed form for encrypted
      volumes. Genesis per D5.
   7. **Sealed-object table** (ADR-11, ADR-30, D6, D9): the object kind ids
      (commit, delta, lease, footer, heat, gc, readers, holds, snapshots,
      dicts, space), the XChaCha20-Poly1305 envelope (24-byte nonce,
      ciphertext, 16-byte tag) and the AAD builder `"lunix meta v1\0" ‖
      volume ‖ kind ‖ bucket-relative object name` (family for the
      keyring). Bodies are frozen for `volume`, keyring, lease, commits,
      `snapshots/` pins, `readers/` pins and `holds/` entries. For
      `heat/`, `gc/`, `gc/space/` and `dicts/` only the envelope and kind
      id are frozen; the body is `{ver: u16, bytes}`.
   8. **`volume` descriptor** (§6.1, ADR-11 F7, ADR-25): format version,
      uuid, family id, `lineage[]` (always empty; entry `{volume_uuid,
      base_seq, backend_hint}`), block size, chunk size, codec policy,
      `id_len`, `map_fanout` (D1), crypto params (encrypted or not, profile
      default|fips) and the volume mode (`encrypted` | `integrity` |
      `plain`, D16), KMK fingerprint, descriptor MAC (label `"lunix
      volume v1\0"`, covers every other field; an unkeyed BLAKE3 checksum
      in `plain` mode), `created_at`.
   9. **Keyring** (`families/<id>/keyring`, ADR-11 F6): `keyring_seq`,
      family id, slots (`slot_id`, kind passphrase-argon2id | key file |
      kms, KDF parameters, salt, wrapped KMK or integrity key), a MAC'd
      body, and the slot AAD `"lunix slot v1\0" ‖ family ‖ slot_id ‖ kind ‖
      KDF params ‖ salt`. Argon2id bounds as constants (read: ≤ 1 GiB,
      t ≤ 10, p ≤ 16; write: ≥ 19 MiB, t ≥ 2).
   10. **Lease** (§8.5): body with plaintext `epoch`, `owner`, `deadline`,
       `released`, `transferred_to?`, `mac`.
   11. **Key-purpose table** (ADR-11): the labels and arguments of `"lunix
       kdf v1\0" ‖ family_id ‖ purpose ‖ args` for the data addressing key,
       the map-node addressing key, one DEK per `dek_gen`, the metadata key
       per object kind, the descriptor MAC key, the keyring MAC key, the
       KMK fingerprint, the integrity (HMAC) key, the **extent tag
       key** (D11) and the **integrity-mode addressing key** (D18: a purpose
       id of its own, derived from the integrity key; the worked examples
       and goldens of integrity-mode extents carry the keyed `chunk_id`).
       This is the only `lunix-format` change D18 causes, and it is an
       amendment to a frozen item (D19).
   12. **Bucket and local paths:** one module with a function per path of
       §6.1 (`commits/<commit_seq:016x>`, `segments/<seg:016x>`, `map/<pack:016x>`,
       `index/<pack:016x>`, `heat/<seq:016x>`, `dicts/<id:08x>`,
       `snapshots/<name>`, `readers/<host>-<id>`, `holds/<child-uuid>`,
       `gc/<pass:016x>`, `sync/<backend-id>/progress`, `head`, `lease`, `volume`,
       `families/<id>/keyring`) and the local layout of §4, including
       `state/commits/<commit_seq:016x>`, `index/`, `snapshots/`, `readers/`,
       `holds/`, `gc/` and `state/gc/`. A function maps a local path to its
       bucket-relative name (D9).
   13. **Index run** (D3; `index_packs[]` in FULL names them): sorted
       `[chunk_id, seg, off, clen]` blocks with a block index and a
       checksum.
   14. **Local-only files:** `volume` (the exact descriptor bytes, so
       `backend add` can upload them verbatim), `volume.json` (local
       settings only: name, directory, space policy, commit cadence,
       `retain_last`, `gc_grace`, `journal_max`), `state/checkpoint` (hint
       only: last `commit_seq`, per-lane journal tail, object id high water
       (`id_high`, D17), stats counters, with a checksum and a double-slot atomic
       layout), `state/device` (device id, `boot_id`, queue count and depth,
       size, flags), `state/pins` (**version 2**, D19: highest `keyring_seq`
       seen, highest epoch seen, KMK fingerprint pin, and the rollback floor
       `max_commit_seq` and `tip_hash`; the decoder also reads version 1,
       which has no floor).
   15. **GC files:** the `gc/<pass:016x>` evidence object, one sealed
       create-only object per GC pass (envelope frozen; body `{ver: 1,
       bytes}` with the pass's JSON lines, defined by M17 in
       `docs/reference/gc.md`, written once at pass end, and once by an
       urgent pass), and the local-only, versioned, non-frozen
       `state/gc/condemned` list (atomic replace; the only mutable GC
       state; losing it is safe: everything is re-marked, and it is never
       uploaded). `gc/space/<commit_seq:016x>` stays reserved.
5. **Golden and property tests** in `crates/lunix-format/tests/`:
   - one checked-in golden file per type and variant under
     `tests/golden/` (binary plus a `.hex` rendering), generated by a
     documented command and compared byte for byte; changing a golden is
     a format change (say so in the file header);
   - `decode(encode(x)) == x` by proptest (10 000 cases per type);
   - **negative tests per decoder:** every prefix truncation of every
     golden fails cleanly; 10 000 random byte mutations never panic;
     a test binary with a counting global allocator asserts that decoding
     any input of length L allocates at most `2 × L + 64 KiB` (no
     allocation from an unchecked count);
   - reserved-field tests (non-zero reserved bits are rejected);
   - the KAT tests of step 3 above, and A0's worked hex examples decoded
     and re-encoded byte for byte.
6. **Type checkpoint (A5).** After steps 1-4's type definitions compile
   (before encode/decode are complete), open the checkpoint commit and tell
   the lead; M1.B1 starts from it.

**Gate (M1.A):**

- `tools/remote.sh cargo build --workspace && tools/ci.sh` green on the
  host; all 17 crates compile; `check-ownership.sh` and the feature-edge
  check pass on their tests.
- Every item 1-15 of the freeze list has a type, a golden file and is
  ticked in `FREEZE.md`, whose last line the lead fills at sign-off.
- KAT: `aad_hardened`, `aad_fips_kdf_zero_nonce` and `info_v2` reproduced
  byte for byte.
- The negative and allocation tests of step 5 pass; proptest round trips
  10 000 cases per type.

### M1.B1 — `lunix-iface`

**Owns:** `crates/lunix-iface`. **Depends on:** A's type checkpoint (A5).
**Model:** design-heavy. **Review:** required, independent, against the
needs of M2-M17 (the reviewer walks each milestone's "Reads" and checks it
can be built against the traits alone).

Design the traits so that every wave 1 milestone can be built and tested
alone. The shapes below are constraints; B1 finalises exact signatures
and documents each trait's contract (ordering, errors, what blocks, which
thread) in rustdoc and in `crates/lunix-iface/README.md`. **Closing the
interface gaps found by the review is the point of this item**: a trait
change after B1 merges costs a rebase of every wave 1 branch.

1. **Block device ↔ engine** (`block`). The engine owns I/O semantics; the
   ublk layer owns tags. `BlockEngine` (shared, `Send + Sync`) creates one
   `QueueHandler` per queue, on that queue's thread. `QueueHandler::
   on_request(&mut self, req: IoRequest, buf: &mut [u8], done: &mut dyn
   Completer)` with `IoRequest {tag, op: Read|Write|Flush|Discard|
   WriteZeroes, fua, offset_bytes, len}`; `poll(&mut self, done)` drives
   completions (called after the wake fd fires); `wake_fd()` is an eventfd
   that workers signal. `Completer::complete(tag, result)` with the
   negative-errno convention. **Threading rule (documented on the trait):**
   `complete` is called only from `on_request` or `poll`, on that queue's
   own thread (libublk 0.4.8 commits only from the queue thread); `poll`
   is the only path for deferred completions. `BlockEngine::size_bytes()`
   and `on_resize`.
2. **Journal** (`journal`). `Journal` creates `JournalLane`s (one per
   queue, non-blocking) and a **flush coordinator**:
   - **Superseded by the merged `lunix-iface` (M1.B1 review, 2026-11-12);
     the iface rustdoc is normative.** `JournalLane::try_reserve(kind,
     blocks) -> Slot | Full | NoSpace`: a refusal consumes no `write_seq`;
     a `Slot` carries a fresh global `write_seq` and is registered in
     flight. `append(slot, lba, data) -> Appended{write_seq, loc}` copies
     the data into the lane's bounded pool and fails only with a fatal
     `Io`. A `Slot` must be appended before the next call on its lane
     (`#[must_use]`; violating it is `Invalid` and poisons the volume).
     `Journal::last_issued()` gives the highest issued `write_seq`.
   - `JournalLane::submit_read(loc, buf, token)` accepts any lane's
     completed append; `poll(&mut Vec<Completion>)`.
   - `Journal::submit_group_flush(upto, done)`: the engine passes
     `upto = last_issued()` read when the group forms; the coordinator
     waits until no reserved or appending slot with `write_seq ≤ upto` is
     in flight on any lane, then `fdatasync`s every lane with unflushed
     appends. A FLUSH covers every write acknowledged before the group
     formed. **Poisoning:** an `append` `Io` error or an `Append`
     completion with an error status fails every pending and later group
     with `upto ≥` the failed seq (`Err(Io)`), and every later
     `try_reserve` or `append` returns `Io`. Lane threads never wait for
     the coordinator.
   - Blocking: `read_at(loc, buf)`, `recover(hint: Option<&Checkpoint>,
     floor: WriteSeq) -> Recovered {records, next_seq, ignored_after_gap:
     Option<WriteSeqRange>}` (all lanes scanned, records with `write_seq > floor`
     merged in `write_seq` order up to the first gap; the hint only says which
     files can be skipped and a lost or stale hint is harmless), and
     `reclaim(upto: &JournalTail)`. `JournalLoc` is `(lane, file ordinal,
     offset)`.
3. **Overlay** (`overlay`): `Overlay` with `insert_write(lba_4k_range, loc,
   write_seq)`, `insert_hole(range, write_seq)`, `lookup(range, &mut Vec<Piece>)`
   (Piece = journal data, hole or none), `cut(c: WriteSeq) -> OverlayCut`
   (MVCC view: entries with `write_seq ≤ c`, grouped by chunk), and
   `release(cut)` (removes only entries with `write_seq ≤ c` that are still the
   visible version).
4. **Map** (`map`): `BlockMap` with `get(ci) -> Option<MapEntry>` (blocking)
   and `try_get_cached(ci) -> Cached::{Hit, Hole, Miss}` (non-blocking),
   `begin_update() -> MapTxn` (`set(ci, entry)`, `set_hole(ci)`,
   `rehint(ci, old: Hint, new: Hint)` which applies only if the current
   entry still has hint `old`, for compaction's `moved` records),
   `seal(&mut dyn PackWrite) -> SealResult {root, new_packs,
   dirty_leaves}`, `grow(new_chunks)`, `root()`, `dirty_leaf_count()` and
   `MapFull` backpressure when the cap is hit; `open(root)`. Nodes are
   read through `NodeSource` and sealed/opened through `ExtentCodec`.
   Read-only traversal of **any** root, for GC and fsck:
   `walk_leaves(root, &mut dyn FnMut(ci, MapEntry))` and
   `walk_nodes(root, &mut dyn FnMut(NodeRef {hash, level, pack, off,
   len}))`; equal subtrees shared between two roots can be skipped by the
   caller by hash (`walk_nodes_skipping(root, &HashSet<NodeHash>)`).
5. **Index** (`index`): `ChunkIndex` with `get_for_dedup(id) ->
   Option<Hint>` (never returns a **condemned** entry), `get(id)` and
   `get_by_prefix(prefix)` (the current copy, condemned or not, for read
   fallback), `insert(id, hint)`, `relocate(id, old, new)` (compaction),
   `mark_condemned(seg)`, `unmark_condemned(seg)`, `drop_segment(seg)`,
   `persist(upto_seg) -> Vec<RunRef>`, `runs() -> Vec<RunRef>`,
   `catch_up(segment_ids_named_by_chain, &mut dyn SegmentFooters)`,
   `rebuild(...)`, `delete_run(RunRef)`.
6. **Store** (`store`): `SegmentStore` (`open_segment() -> SegmentWriter`
   with `append_extent(id, bytes) -> Hint` and `seal(...)`; `read_extent(
   hint, out)`; `list` with sizes, `footers`, `delete(seg)`), `PackWrite`/
   `PackRead` for map packs plus `delete_pack`, `CommitStore` (`put`, `get`,
   `list`, `tip`, `delete(commit_seq)` which refuses at or above the tip's
   `chain_floor`), `CheckpointStore`, `PinSource` (`roots() ->
   Vec<PinRoot>` from the local `snapshots/`, `readers/`, `holds/`
   directories, empty in phase 1), `GcStateStore` (`load`/`store` the local
   condemned list; `put_evidence(pass, body)` for `gc/<pass:016x>`, create-only), `VolumeDir`
   (create/open, `LOCK`).
7. **Space** (`space`): `SpaceGate::reserve(kind, bytes) -> Reserved |
   Parked(waiter) | NoSpace`, `release(kind, bytes)`, `transfer(from_kind,
   to_kind, from_bytes, to_bytes)` (atomic: releases `from_bytes` of one
   kind and charges `to_bytes` of another; also legal with equal kinds, for
   true-ups), `reserve_headroom(kind, bytes)` (draws on the reserved
   headroom; never parks), `SpaceEvents` for read-only on host EIO, and
   `Ladder::register(step, Box<dyn LadderStep>)` with `LadderStep::run(
   want_bytes, urgency) -> Freed`. **Accounting walk-through** (documented
   once in the crate README; M8, M9, M11 and M17 cite it and test it):
   - *admission*: `reserve(JournalReserved, record_bytes)`;
   - *C2* (cut taken): `reserve_headroom(Segments, ulen_of_nonzero_chunks)`
     as an upper bound;
   - *C4* (segments sealed): `transfer(Segments, Segments, bound, actual)`;
   - *C5/C6/C8*: map pack, commit and index run bytes are charged from
     headroom to `Map`/`Commits`/`Index` with `transfer(Headroom, kind,
     bound, actual)`;
   - *C9* (journal reclaimed): `release(JournalReserved, reclaimed)`;
   - *GC delete*: `release(kind, bytes)` for each file removed, after the
     delete is durable;
   - *compaction output*: charged from headroom like a cut's segments.
   The ledger is conservative between C2 and C9 (journal bytes and segment
   bytes both counted) and exact outside that window.
8. **Crypto** (`crypto`): `ExtentCodec` (`chunk_id(kind, data)`, `seal(kind,
   id, EncodingDesc, compressed, out)`, `open(kind, bytes, expect) ->
   EncodingDesc + payload`; both volume kinds carry the 16-byte tag, D11),
   **`MetaCodec { seal(kind, name, body) -> Vec<u8>, open(kind, name,
   bytes) -> Vec<u8> }`** (footers, lease, pins, `gc/`, `heat/`, `dicts/`;
   `name` is the bucket-relative name, D9), `CommitCodec` (= `MetaCodec`
   plus the unencrypted-volume HMAC and chain logic: `seal`/`open` a
   commit given the chain context, `verify_chain`), and an opaque `Keys`
   handle. The testkit's `PlainCodec` implements all three.
9. **Control protocol** (`ctl`, D13): length-prefixed frames (u32 big
   endian length, then a JSON object `{id, method, params}`; responses
   `{id, ok | err{code, message}}`), the request/response enums for `Ping`,
   `VolumeCreate`, `VolumeList`, `VolumeStatus`, `Attach` (with
   `max_io_buf_bytes`, D14), `Detach`, `Resize`, `Space`, `QuiesceAll`,
   `Gc {name, dry_run}` and `GcStatus {name}`, and a `Secret` newtype
   (zeroizing, redacted `Debug`, never serialised into logs). Error codes:
   `Busy`, `NotFound`, `Downgrade`, `WrongKmk`, `NoSpace`, `Invalid`, `Io`,
   `Internal`.
10. **GC hooks** (`gc`): `Collector` with `step(&mut self, view: &GcView,
    budget: GcBudget) -> GcProgress {moved, freed, state, done}`, called by
    the engine's cut/commit coordinator **only between cuts and only when
    the in-memory map root equals the tip's `map_root`** (M11 enforces,
    M17 relies on it); `GcView` bundles read access to the retained
    commits, `PinSource`, `BlockMap` traversal, `ChunkIndex`, `SegmentStore`,
    `PackRead`, `CommitStore`, `GcStateStore`, `SpaceGate`, the `Clock`
    and the volume parameters; `Moved {id, old, new}`; `GcStats`. The
    ladder's compact step is a `LadderStep` backed by the same `Collector`
    (an "urgent pass").
11. **`crashpoint!`** macro and the static registry (`inventory`), the
    `LUNIX_CRASH_AT` reader, feature `crashpoints`; the shared
    `Clock`/`Rng` traits for deterministic tests; and the **contract-test
    macros** `lunix_iface::contract::{journal, overlay, map, index, store,
    space}!(make_impl)`: a suite each trait's fake and real implementation
    both run (non-blocking versus blocking methods, error behaviour,
    ordering, crash-model invariants).
12. Fill every skeleton crate's `Cargo.toml` with its intended internal
    and external dependencies so `Cargo.lock` is complete.

**Gate (M1.B1):** `lunix-iface` compiles with no dependency on any
implementation crate; every trait item has rustdoc stating its thread and
blocking class; the contract macros compile against a trivial impl; the
`ctl` frames round-trip (proptest, 10 000 cases) and reject oversized and
truncated frames; `crashpoint!` registry test (names unique and listed,
feature off compiles to nothing, feature on kills at the `n`th hit in a
child process); the reviewer's walk of M2-M17 "Reads" finds no missing
method (resolved findings listed in the report).

### M1.B2 — `lunix-testkit`

**Owns:** `crates/lunix-testkit`. Dev-dependency only; nothing links it
into a binary. **Review:** B2b is reviewed together with B1.

**B2a** (starts when B1 merges): `PlainCodec` (all three crypto traits;
the plain tag of D11 with a fixed test integrity key), `FlatOverlay` (also
the oracle for M4), `FlatMap` (the oracle for M5), `MemSpace`, and the
proptest strategies for I/O op sequences (read, write, flush, FUA, discard,
write-zeroes; aligned and unaligned to 4 KiB and to the chunk size; sizes
from 4 KiB to 1 MiB).

**B2b**: `MemJournal` (floor-based recovery exactly as `Journal::recover`
specifies, lanes, the group flush, and a crash model that keeps flushed
records and drops or tears unflushed ones at random), `MemIndex` (with
condemn and relocate), `MemStore` (with crash points, `PinSource` and
`GcStateStore`), `MemBlockEngine`, the flat-`Vec<u8>` device oracle, a fake
control server for M13, and a contract test per fake from
`lunix_iface::contract`.

**Gate (M1.B2):**

- Each fake has its own tests and passes its contract suite (a fake that
  differs from its documented contract is a bug).
- The `FlatOverlay`, `FlatMap` and device oracle each pass a
  self-consistency proptest (1 000 sequences of 100 ops).
- An example `MemBlockEngine` driven by a 10 000-op random sequence
  through the `QueueHandler` trait matches the oracle.

**Not to touch (M1):** none of the implementation crates beyond their
skeletons.

---

## M2 — `lunix-crypto`

**Owns:** `crates/lunix-crypto`. **Depends on:** M1.B1 (and M1.A0's
`attach-matrix.md`). **Model:**
design-heavy. **Review:** required, independent (a reviewer with crypto
experience; checklist at the end of the milestone).
**Reads:** ADR-11 in full and its gate outcome;
`docs/explanation/research/2026-11-11-adr-11-crypto-review.md`; DESIGN §5
step 6, §6, §8.2, §8.4 (attach), §15; `spikes/crypto-kat/src/*` (start
from `kdf.rs`, `extent.rs`, `meta.rs`, `slots.rs`, `tripwire.rs`) and
`bench/crypto-kat/RESULTS.md`; replicix-chunkfmt `e2e` (Argon2id, unlock)
for what Lunix does and does not reuse (Lunix uses its own KDF labels and
never `ReplicixDomain`).

**Steps:**

1. **Key hierarchy:** KMK (32 B) → per-purpose keys with keyed BLAKE3
   (`keyed_hash(KMK, "lunix kdf v1\0" ‖ family ‖ purpose ‖ args)`; the
   FIPS-profile HKDF-SHA-256 variant is implemented for key derivation
   because the vectors cover it, but sealing under the FIPS profile returns
   `Unsupported` in phase 1). Types: `Kmk`, `Dek`, `MetaKey`, ... all
   `Zeroizing`, no `Debug`/`Clone`/`Copy`, with `mlock` of the key arena
   (best effort) and `MADV_DONTDUMP`.
2. **`ExtentCodec` implementations:** `PlainExtents` (unencrypted:
   `chunk_id = blake3::keyed_hash(addressing_key, plaintext)` in `integrity`
   mode, the addressing key derived from the integrity key by its own
   purpose (D18), and `blake3(plaintext)` in `plain` mode; every extent carries the 16-byte tag of
   D11, BLAKE3-128 over every byte of the extent before the tag (`CCH1
   header ‖ chunk_id ‖ clen ‖ payload`, one contiguous slice), keyed by a
   key derived from the integrity key in `integrity` mode and unkeyed in
   `plain` mode (D16); no AEAD) and
   `AesGcmExtents` (keyed `chunk_id`, the keyed per-extent key over the
   `info` of M1, AES-256-GCM with the all-zero nonce through `aws-lc-rs`,
   AAD from M1). The salt is drawn from the OS through `getrandom(2)`
   **inside** `seal` on every call, never from a userspace DRBG (`rng::fill_os`
   calls `getrandom`; D20, cross-cutting rule 3); the metadata nonce, slot
   nonce, Argon2id salt and generated root keys use the same source. There
   is no salt setter. `OpenExpect` and `ExtentCodec::open` document that a
   data read without an id expectation is a scan and must set `verify_id`
   (R1); a header `dek_gen` above the keyring's newest is `Auth`, like the
   other checks before the AEAD. A `test-rng` cargo feature (off by
   default; `tools/ci.sh` fails if a non-dev edge enables it) lets tests
   inject an RNG to reproduce the vectors.
3. **`(key, nonce)` tripwire** in debug and fuzz builds (`cfg(debug_assertions)`
   or feature `tripwire`): records every pair, panics on a repeat.
4. **Open path order** (ADR-11 Integrity): header sanity (version, known
   codec, `chunk_id` equals the expected), AEAD tag, `ulen` against the
   volume chunk size (never the header), bounded decompression (cap =
   chunk size), then recompute the chunk id (the key commitment) when the
   caller asks (`verify_id = true`; local reads pass `false` because the
   tag already authenticates the stored bytes, D11; M11's `verify_reads`
   flag passes `true`). On unencrypted volumes the plain tag is checked
   first, before any other field is trusted.
5. **Metadata sealing:** `MetaCodec` (`seal(kind, name, body)`,
   `open(kind, name, bytes)`): XChaCha20-Poly1305 with random 192-bit
   nonces, per-kind metadata keys, AAD from M1 with the **bucket-relative**
   object name (D9). `CommitCodec` (= `MetaCodec` plus chain logic): seal/open for
   encrypted volumes; for unencrypted volumes append/verify the HMAC-SHA-256
   `mac` over the body (including `epoch` and `parent_hash`) under the
   integrity key (`integrity` mode); in `plain` mode `mac` is an unkeyed
   BLAKE3 checksum, with the same fields covered. `verify_chain(from_full, tip)`: `parent_hash` equals the
   BLAKE3 of the parent object's stored bytes, `epoch` never decreases,
   every commit authenticates. Lease: seal/open plus `lease_is_stale(lease,
   max_epoch_seen)`, and `open_lease_fresh(bytes, epoch_floor)` which opens
   and rejects an epoch below the floor; attach opens the lease only through
   it (R4).
6. **Keyring:** create (new KMK or new integrity key, first slot; for an
   `integrity` volume made without `--passphrase`, the first slot is a
   key-file slot whose random key `create` writes to
   `<dir>/<uuid>/integrity.key`, mode 0600, and that attach reads, D16; a
   `plain` volume gets no keyring slots),
   `add_slot`, `remove_slot`, `unwrap(slot credential)`, `rewrap`.
   **Hostile-keyring cost policy:** the format bounds are the read bounds
   below; `Unlock::Passphrase` or `AttachRequest` also carries a policy
   ceiling (`max_m_kib`, default 256 MiB, and a total time and memory
   budget over the slots tried); a slot over the ceiling is skipped and
   reported; the slot recorded at the last attach is tried first
   (`only_slot`); the `argon2` crate's `zeroize` feature wipes the blocks.
   `KeyFile::read` opens with `O_NOFOLLOW | O_NOCTTY | O_NONBLOCK`, checks
   `is_file()`, and logs group or other bits instead of refusing.
   Implemented slot kinds: passphrase (Argon2id, parameters stored per
   slot, bounded on read and floored on write) and key file (32 random
   bytes). The KMS kind is accepted by the decoder and returns
   `Unsupported`. `keyring_seq` rises on every change; `check_not_older(
   seen)`. Slot wraps are XChaCha20-Poly1305 under the key-encryption key
   with the AAD of M1. An unencrypted family has a keyring whose slots wrap
   the integrity key.
7. **Descriptor MAC and attach checks:** `kmk_fingerprint`, `descriptor_mac`
   and `check_attach(descriptor, credential: Option<..>, supplied_fingerprint:
   Option<..>, local_pin: Option<..>) -> Result<AttachOk, AttachError>` with
   `Downgrade`, `WrongKmk`, `BadMac`, `KeyringRollback`, per ADR-11 F7
   (supplied key material ⇒ the volume must be encrypted; an integrity-only
   credential declares an `integrity` volume whose MAC must verify) and
   D16: attaching a `plain` volume needs no credential, and a credential
   supplied for one, or a pin that records a stronger mode than the
   descriptor, is `Downgrade`. The descriptor of a plain volume is checked
   against its unkeyed checksum.
8. **Microbenchmark binary** `crates/lunix-crypto/src/bin/cryptobench.rs`
   (4 KiB, 16 KiB, 64 KiB seal and open; Argon2id at 64 MiB, t = 3).

**Gate:**

- All fields of `kat-vectors.json` reproduced (keys, AAD, bodies, purposes,
  slot AAD, XChaCha vectors, Argon2id vector), for the default and FIPS
  derivations (FIPS seal vectors only through `test-rng`).
- Round trips for 16 KiB, 64 KiB and 1 MiB chunks, every codec 0-2; codec
  3 round-trips the framing and is rejected by the decompressor until
  phase 4.
- **Tamper matrix:** flipping any single byte of an extent (header, id,
  `clen`, salt, nonce, payload, tag) fails `open`, on encrypted volumes and (plain
  tag, D11) on unencrypted ones; one test per AAD field changed
  independently fails; an extent moved to another family, kind or `id_len`
  fails.
- **Keyed ids (D18):** in `integrity` mode `chunk_id` equals
  `keyed_hash(addressing_key, plaintext)` and differs from `blake3(
  plaintext)`; two integrity volumes with different integrity keys give
  different ids for the same chunk; `plain` mode still gives `blake3(
  plaintext)`; the addressing key differs from the tag key, the HMAC key and
  the node key; an extent of one key does not open under another's
  expectation.
- **Randomness (D20):** a test that substitutes the source shows every salt
  and nonce is drawn per call from `rng::fill_os` (a `getrandom` call), and
  that no `aws_lc_rs::rand` import remains outside the AEAD; the 64 KiB and
  4 KiB seal times are re-measured after the change and stay within the
  bounds below.
- **Hostile keyring (D20):** a keyring of 32 slots at the format maximum
  costs no more than the policy budget; a slot over `max_m_kib` is skipped
  and reported; the remembered slot is tried first.
- **Nonce discipline:** sealing the same chunk twice, and with two
  different codecs, yields different salts and different keys (1 000 each);
  1 000 000 seals in the debug tripwire build never repeat a pair; a test
  that replays an RNG makes the tripwire fire.
- **Keyring:** Argon2id bounds enforced both ways (reject m > 1 GiB, t >
  10, p > 16 on read; refuse m < 19 MiB or t < 2 on write); a slot copied
  to another slot id or family fails; a keyring with a lower `keyring_seq`
  than seen is rejected; wrong passphrase and corrupted wrap give the same
  error.
- **Keyring creation and attach (D16):** `volume create` with no mode flag
  yields an `integrity` volume with a key-file slot, and attach with no
  extra argument succeeds by reading `integrity.key`; with `--passphrase`
  the passphrase unwraps; a missing or wrong key file is `WrongKmk`-class,
  not a panic; `integrity.key` is created 0600; a `plain` volume attaches
  with no credential and refuses one.
- **Attach matrix** (`encrypted`/`integrity`/`plain` descriptor × credential
  supplied/not × MAC valid/invalid × fingerprint right/wrong/absent × pin
  right/wrong/absent): the result equals `docs/reference/attach-matrix.md`
  (written in M1.A0), which the test loads as the expected table; M2 does
  not derive it.
- **Chain:** broken `parent_hash`, a decreasing `epoch`, a splice of two
  valid histories, a replayed older lease, a missing or wrong HMAC on each
  of the five commit kinds are all rejected.
- Types: `trybuild`-style compile-fail tests (or `static_assertions`) show
  key types lack `Debug`, `Clone`, `Copy`; a test shows zeroization of a
  dropped key buffer (read through a retained raw pointer in a `unsafe`
  test module, allowed only there).
- Performance, relative to `bench/crypto-kat/RESULTS.md` measured in the
  same session with `spikes/crypto-kat/run.sh`: full seal of 64 KiB ≤ 1.5×
  the spike's "proposed derivation and fresh salt" time (3 975 ns on the
  phase 0 host) and of 4 KiB ≤ 1.5× 920 ns; open within the same bounds.
- No `unsafe` outside the zeroization test and the `mlock` wrapper.

**Reviewer checklist:** the salt is never an input; the AAD matches ADR-11
byte for byte; every metadata object kind has its own key; Argon2id
bounds; downgrade matrix; error types leak nothing; the HMAC covers
`epoch` and `parent_hash`; constant-time comparisons for MACs and tags.

**Not to touch:** other crates; the KAT file.

---

## M3 — `lunix-journal`

**Owns:** `crates/lunix-journal`. **Depends on:** M1.B1. **Model:**
design-heavy. **Review:** required for the recovery scan and torn-tail
handling (independent).
**Reads:** DESIGN §3.1, §4.1, §8.1; ADR-4 and its gate outcome;
`bench/journal-bench/RESULTS.md`; `spikes/journal-bench/src/main.rs`
(group commit, preallocation), `spikes/ublk-recovery/src/memsink.rs`
(record header with CRC); ADR-26 (the record format is shared with the
future RAM sink, so the record codec lives in `lunix-format`, not here).

**Design decisions (settled: D7, D12; record any detail you add in the
README and report):**

- **Lanes.** One lane per ublk queue. A lane owns its ring of files
  (`journal/q<lane>-<ordinal:08x>.j`, 64 MiB each, preallocated and
  physically zero-filled, `O_DIRECT`, `chattr +C` on btrfs). `write_seq` is
  global, taken from one global counter by the lane at `try_reserve`
  (see the M1.B1 contract above; the iface rustdoc is normative); `epoch`
  is 0. FLUSH is a **group flush**: `submit_group_flush(upto =
  last_issued(), done)` waits until no reserved or appending slot with
  `write_seq ≤ upto` is in flight, then `fdatasync`s every lane with
  unflushed appends in parallel; at most one group is in flight; a FLUSH
  covers every write acknowledged before the group formed; any append
  failure poisons the journal as stated above.
- **Ring size** `journal_max` (default 2 GiB per volume, minimum 256 MiB,
  split evenly across lanes). A lane with no free file returns `Full` and
  the engine parks the write until reclaim.
- Records are written with `io_uring` (`io-uring` crate, registered files
  and fixed buffers), batched per submission; the lane is single-threaded
  and never blocks. `append` **copies** the data into a buffer from the
  lane's bounded pool of aligned fixed buffers (`Full` when the pool is
  exhausted). Packing (D12, DESIGN §4.1): each record is a 64-byte inline
  header plus data; records of one batch are packed back to back, 8-byte
  aligned; only the batch is zero-padded to a 4 KiB multiple and written
  with one `O_DIRECT` write; a zero magic ends a batch.

**Steps:**

1. Ring file creation with header, zero fill, directory fsync; ring
   open/validate; ordinal and generation bookkeeping.
2. `JournalLane`: `try_reserve` and `append` (reserve space and a
   `write_seq`, build the record with `lunix-format`, copy, queue the batch
   write), `submit_read` (any lane), `poll`, the poisoning rule and the
   dropped-`Slot` check. A 64 KiB write is one
   record. The flush coordinator (`submit_group_flush`) on its own ring.
3. Blocking `read_at` for destage workers and for recovery.
4. `recover(hint: Option<&Checkpoint>, floor: WriteSeq)`: `floor` is the tip
   FULL's `cut_seq` (trusted, authenticated; 0 at genesis). Scan **every
   file of every lane**, accept records with a valid CRC and a valid file
   header, keep those with `write_seq > floor`, merge lanes by `write_seq`, and stop at
   the first gap in contiguous `write_seq`. **Resync (D17):** on a header
   or CRC failure at offset `p` the lane does not end; scan forward from
   `p + 8` in 8-byte steps to the end of the file for the next header that
   passes magic, `ver`, `gen` (the file's generation), `off` (its own file
   position), CRC and `write_seq` above the last accepted, accept it and
   continue. A lane ends only when no such header exists (a zero magic
   still ends a batch, and the reader continues at the next 4 KiB
   boundary). Global contiguity decides what is kept: a damaged record at
   or below the floor costs nothing, a damaged record above it is a gap.
   `fsck` reports the damaged byte range. The `hint` is used only if its
   `last_commit_seq` is at most the tip's `commit_seq` (D17). Stale records
   of a previous ring pass have `write_seq ≤` the expected one and are discarded
   by the gap rule. Everything after the stop point is **ignored** and
   reported in `Recovered::ignored_after_gap` (the write_seq range of valid
   records beyond the gap); there is no hard error (nothing on disk records
   the last flushed write_seq). The checkpoint hint only says which files can be
   skipped; a lost or stale hint changes nothing but speed. Return the
   records (with their `JournalLoc`) and the next free `write_seq`.
5. `reclaim(tail)`: files wholly below the tail are recycled (re-zeroed in
   the background) or deleted.
6. Crash points: `journal.after_append_submit`, `journal.before_fdatasync`,
   `journal.after_fdatasync`, `journal.group_flush.after_watermark_wait`,
   `journal.before_reclaim_delete`,
   `journal.after_reclaim_delete`.
7. A bench binary `journal-lane-bench` (one lane, closed loop, 4 KiB and
   64 KiB records, group sizes 1, 16, 64).

**Gate:**

- **Model test** (proptest, ≥ 2 000 sequences, 16 KiB and 64 KiB writes
  mixed): random appends across 1-8 lanes, flushes, and a simulated crash
  that, for each lane, keeps every byte up to the last flush point and
  replaces the unflushed tail by a random mix of intact, torn
  (random-length prefix) and missing records. After `recover`: the result
  is exactly a `write_seq`-prefix, contains every record whose `write_seq` ≤ the last
  completed flush, contains no record with a bad CRC, and the next `write_seq`
  is correct.
- **Stale older record in a reused file (M1.B2b review, minor 13).** The
  testkit's `MemJournal` models absence, not stale content, so it cannot
  show this case and M3's own tests must. A real journal reuses
  preallocated ring files; after a crash a *dropped* record's slot holds
  the *older* bytes of the previous pass, which can look like a valid
  record with a lower `write_seq` than its neighbours. The file `gen` (and
  `off`, CRC and the contiguity rule) must make recovery reject it. Records
  at or above `next_seq` that remain after a gap must be invalidated by
  the journal before the first append that reuses their numbers (the
  stale-tail wipe at the first `open_lane` or `reclaim` after `recover`,
  DESIGN §4.1); `recover` after a lane was opened is `Invalid`.
- **Oracle log ordering.** `OracleLog::first_match` needs the log in
  `write_seq` order; with several queues, a harness (M11's and M12's model
  tests) logs at `try_reserve` time, not at submission.
- **Ring wrap with a lost checkpoint** recovers the same prefix (stale
  records of the previous pass are discarded by the floor and the gap
  rule); a zeroed file or a corrupted record mid-ring **above the floor**
  ends recovery at the first gap and the later valid records are ignored
  and reported in `ignored_after_gap`; a corrupted record **at or below
  the floor** changes nothing (resync finds the later records); a
  checkpoint whose `last_commit_seq` is above the tip is ignored (the
  truncate-tip case); a stale or absent checkpoint changes nothing;
  `floor` = a FULL's `cut_seq` ignores everything at or below it.
- Group flush with 1-8 lanes: no lane's `fdatasync` is issued before that
  lane's watermark reaches `upto`; at most one group in flight; the group
  completes only when all lanes have.
- Alignment test: every write is 4 KiB aligned in file offset, length and
  buffer; `O_DIRECT` is in effect (the test SKIPs on tmpfs).
- Reclaim never deletes a file that holds a record above the tail; space
  accounting returns to zero after reclaim.
- **Perf, relative:** in the same session run `spikes/journal-bench`'s best
  group-commit cell for 4 KiB records on the scratch file system at batch
  ≥ 32 (P0.5's group commit; the reference is not a one-block-per-write
  ideal, because batch packing costs ceil(n·4160/4096) blocks and an
  isolated QD1 4 KiB write costs 8 KiB, D12). One
  lane of `journal-lane-bench` must reach ≥ 0.8× its rec/s with p99
  flush latency ≤ 1.25×; four lanes must reach ≥ 1.5× one lane's rec/s
  on the real-disk directory, or the report explains why not (a disk
  that reports write-through flushes in microseconds is expected to
  scale).
- Each named crash point has a test that kills a child process there
  (`crashpoints` feature) and then recovers.

**Reviewer checklist:** the resync rule (what it accepts, that a guest
payload or a stale or relocated record cannot pass `gen` ‖ `off` ‖ CRC);
the merge/stop rule against torn tails and wrap;
the FLUSH guarantee reading (what `ignored_after_gap` reports and why
nothing is a hard error); the group-flush watermark wait; fsync/dir-fsync order at file creation; no blocking call on the
lane thread.

**Not to touch:** the record codec (it is in `lunix-format`).

---

## M4 — `lunix-overlay`

**Owns:** `crates/lunix-overlay`. **Depends on:** M1.B2a. **Model:** routine.
**Reads:** DESIGN §4.2, §8.1 (MVCC), §5 step 1; the `Overlay` trait and
`FlatOverlay` in M1.
**Steps:** an LBA-sharded interval map at 4 KiB granularity (default 64
shards by `lba / chunk` so that a chunk never spans shards), entries
`{range, loc | hole, write_seq}`, newest `write_seq` wins per 4 KiB block; `cut(c)`
returns a frozen view with entries `write_seq ≤ c` grouped by chunk (copy-on-
write of the shard's persistent structure, or versioned entries — choose
and document); `release(cut)`; `lookup` that returns pieces for a read
range. Insert and lookup are called from queue threads (no blocking beyond
a short shard lock; document the lock), `cut`/`release` from the destage
thread.
**Gate:**

- Proptest against `FlatOverlay`: ≥ 10 000 sequences of 200 ops (insert
  write, insert hole, lookup, cut, release, unaligned and chunk-crossing
  ranges) agree on every lookup and every cut's contents, at chunk sizes 16
  KiB and 64 KiB.
- A cut taken at `c` is unaffected by later inserts; `release` removes
  only `write_seq ≤ c` entries that are still visible; a later overwrite of the
  same 4 KiB survives the release.
- Stress: 8 writer threads on disjoint and overlapping ranges plus 2
  readers and a cutter for 10 s; a final comparison with a serial replay.
- Memory: 1 000 000 distinct 4 KiB entries use ≤ 96 B each; runs of
  contiguous locations merge (a 1 GiB sequential write is ≤ 1 000 intervals).
- Perf (informational, quiet host): lookup ≥ 3 M/s single thread on 1 M
  entries.

**Not to touch:** the trait.

---

## M5 — `lunix-map`

**Owns:** `crates/lunix-map`. **Depends on:** M1.B2a. **Model:** routine,
spike-guided. **Reads:** DESIGN §7; ADR-8 and its gate outcome and
amendments; `bench/radixmap/RESULTS.md`; `spikes/radixmap/src/*` (start
from `map.rs`, `leaf.rs`, `store.rs`, `model.rs`) and
`spikes/leafenc/src/codec.rs`. Port, do not copy structure: the spike used
its own node and leaf byte formats; this milestone uses `lunix-format`'s.
**Steps:**

1. `BlockMap` over fanout 512 (and 256, from the descriptor), depth from
   the size, empty-node hash for absent subtrees, entries `hole | {id
   prefix, hint}`; the shape depends only on the LBA.
2. Leaf codec `rb16` via `lunix-format`; point lookup without a full
   decode; a decoded tier for hot leaves; an in-memory node cache bounded
   by `map_cache_max` (default 256 MiB), interior nodes always resident.
   `fastleaf.rs`, the private point-lookup reader for `rb16` leaves that
   meets the 250 ns gate, is accepted (M5 decision 2); an equivalent fast
   path in `lunix_format::node` would allow deleting it. `open` takes the
   commit's `size` (`chunk_count`); the engine passes it.
3. Mutable copy-on-write overlay of dirty leaves keyed by leaf index; a cap
   counted in dirty leaves (default 20 000, configurable) after which
   `MapTxn::set` returns `MapFull` so the engine seals; document the memory
   cost (32 KiB decoded against 11.4 KiB sealed: a decoded leaf is 32 KiB
   at fanout 512 because `Entry` is 64 bytes, so the default cap of 20 000
   dirty leaves is 640 MiB; M5 report, decision 3).
4. `seal`: serialise dirty leaves, recompute hashes up the paths, write
   the new nodes into one pack through `PackWrite`, return the root. The
   node hash covers child hashes and level only, never locations (ADR-8
   amendment 1); nodes are sealed through `ExtentCodec` (`kind = node`,
   keyed by the node hash when encrypted); interior nodes are stored raw
   (codec 0 through the extent codec; the node header has `level` and
   `fanout` only, D1).
   **R2 (cross-cutting rule 13):** every node loaded through `NodeSource`
   (from the local pack store as much as from a fetch) is opened through
   `ExtentCodec` with `kind = node` and must have codec 0; its hash is then
   recomputed (leaf: the whole encoding; interior: header ‖ child hashes)
   and must equal the parent's `child_hash` (the commit's `map_root.hash`
   for the root) **and** the extent header's `chunk_id`. Any mismatch is a
   typed error, and the node is never cached. Node hashes stay unkeyed in
   `integrity` mode (D18).
5. `grow(new_chunks)`: extend depth by wrapping the old root as child 0 of
   new roots; unchanged subtrees keep their hashes.
6. `walk_leaves(root, f)` and `walk_nodes(root, f)` over **any** root (live
   entries for fsck and ratio reports; every node with its pack, offset
   and length for GC and fsck), `walk_nodes_skipping(root, hashes)` that
   skips subtrees whose hash is in the set, `MapTxn::rehint(ci, old, new)`
   (applies only when the entry still has hint `old`; for compaction's
   `moved` records, M17), and `open(root)` with lazy node loading through
   `NodeSource`.
7. Crash point: none (the map writes through `PackWrite`, M7).

**Gate:**

- Proptest against `FlatMap` (≥ 5 000 sequences of 300 ops: set, hole, get,
  seal, reopen from the sealed root, grow), at 16 KiB and 64 KiB chunks and
  fanouts 512 and 256.
- **Root equality:** the same final content built sequentially, in random
  order, and from the flat model gives one root (1 TiB virtual volume,
  1 M random chunks); two stores holding the nodes at different pack
  locations give the same root.
- The empty volume's root is the documented constant; a sparse 10 TiB map
  costs ≤ 1 MiB of nodes.
- **Perf relative to `bench/radixmap/RESULTS.md`** (10 TiB, 64 KiB chunks,
  one core, quiet host; run `spikes/radixmap/run.sh` in the same session):
  point lookup on an encoded leaf ≤ 250 ns median; seal of 1 000 scattered
  dirty leaves ≤ 2× the spike's time (59.7 ms on the phase 0 host); a
  256 MiB leaf cache over 1 M random lookups peaks ≤ 320 MiB RSS; cold open
  plus first lookup reads exactly `depth` nodes.
- **R2 gate (tamper):** with a codec whose tag verifies (the testkit
  `PlainCodec` in `integrity` mode with a key), each of these returns a
  typed error and caches nothing: a valid node extent of the family placed
  where another node is expected (its hash differs from the parent's
  `child_hash`); a node extent whose header `chunk_id` differs from the
  recomputed hash; a node with codec 1 or 2; a data extent presented as a
  node; the root checked against a different `map_root.hash`; the same cases
  served by the local pack store rather than a fetch.
- Corrupt node bytes, a wrong hash, an out-of-range hint, and a truncated
  pack all return errors, never a panic or a wrong entry (mutation test,
  10 000 cases).
- `rehint` applies only when the entry still has the old hint (a newer
  write wins); `walk_leaves`/`walk_nodes` of any root agree with `FlatMap`,
  the pack ids reported are exactly the packs holding those nodes, and
  traversal of two roots with `walk_nodes_skipping` visits only the
  symmetric difference.

**Note for M14 and M17:** `walk_nodes_skipping` skips by node hash, and two
roots can hold the same node content at two locations (different packs), so
skipping can hide the second location. **Pack liveness must use a full
`walk_nodes` of each live root**; the skipping variants are for diffs and
for leaf marking only.

**Not to touch:** the spike directories.

---

## M6 — `lunix-index`

**Owns:** `crates/lunix-index`. **Depends on:** M1.B1. **Model:** routine.
**Reads:** DESIGN §5 step 4, §7 "Chunk index", §13.1; ADR-14; the index
run format in M1.
**Steps:**

1. In-memory table (memtable of recent inserts, `xxh3`-keyed filter for the
   persisted runs) and persisted LSM runs in `index/` with a manifest
   (atomic replace); lookups by full `chunk_id` and by prefix
   (`get_by_prefix`) confirmed against the full id.
2. `insert` only for chunks in **sealed** segments. `persist(upto_seg)`
   writes a run containing the memtable entries whose segment is sealed
   and named by a durable commit; the run is named by the **next** FULL
   (D3), and `runs()` lists runs with their ids; size-tiered compaction of runs (≤ 8 runs
   per level, documented).
3. `catch_up(segment_ids, footers)`: after an unclean stop, scan the
   footers of the segments in `segment_ids` (the ids named by the retained
   commit chain; never "newer than the manifest", which would ingest
   orphans, N2) that the manifest does not cover and insert their entries;
   drop manifest entries pointing at segments the store does not have.
   **Open iface item (M6 report, request 2):** `SegmentFooters::segment_footer`
   is the only existence probe, so `catch_up` decodes the footer of every
   segment the index points into just to learn that it exists. A cheap
   `exists(seg)` on the trait would avoid it. It is not needed for
   correctness (startup cost only) and the trait is not changed yet.
4. **Dedup confirmation (D17).** Index runs are checksummed, not
   authenticated. `get_for_dedup` marks each hit with its source: a hit
   from the in-process table built from verified footers is `Verified`; a
   hit from a persisted run is `Hinted` and the caller (M9) must confirm it
   by reading the extent header at the hint, comparing all 32 bytes of
   `chunk_id` and verifying the tag or AEAD, before the map points at it.
   An unconfirmed hit is a miss. `rebuild(footers)` from scratch (the
   `fsck --rebuild-index` engine).
   GC support (M17): `mark_condemned`/`unmark_condemned` (a persisted
   per-segment flag in the manifest; `get_for_dedup` never returns a
   condemned entry, `get`/`get_by_prefix` still do), `relocate(id, old,
   new)` (inserts the new hint, newest wins; it follows the persist rule of
   step 2: a relocated entry stays in the memtable and is never persisted
   until the segment it names is named by a durable commit), `drop_segment(seg)` (removes
   its entries from lookups at once and from runs at the next run
   compaction), `delete_run`.
5. Memory budget: ≤ 16 B of RAM per indexed chunk for the filter and
   directory (16.7 M chunks per TiB at 64 KiB ⇒ ≤ 268 MiB per TiB of
   unique data); the plan chose this budget (the budget is the plan's, not the spec's;
   it is recorded as a `volume.json` default).
6. Crash points: `index.before_manifest_swap`, `index.after_manifest_swap`,
   `index.after_condemn_flag`.

**Gate:**

- Proptest against a `HashMap` oracle (≥ 5 000 sequences: insert, get,
  prefix get, persist, reopen, compaction), with injected truncation of
  the newest run or manifest at a random byte: reopen falls back to the
  previous manifest and `catch_up` restores every entry of sealed
  segments.
- `rebuild` equals the incrementally built index entry for entry.
- **Hinted hits:** a run entry `X → (seg, off)` naming a valid extent of
  another id is returned as `Hinted`, never `Verified`; a test with a
  forged run shows the hit is flagged.
- Dedup safety: a test shows the index never returns a hint for a segment
  that is not sealed, and `get_for_dedup` never returns an entry of a
  condemned segment from the moment of `mark_condemned` (also after a
  reopen); `relocate`, `drop_segment` and `unmark_condemned` match the
  `HashMap` oracle; a catch_up over a segment that no commit names does
  nothing; a `relocate` entry is absent from every persisted run until the
  new segment is named by a durable commit, and a crash before that leaves
  the old entry current.
- Perf: ≥ 1 M lookups/s on 10 M entries single thread (memtable and run
  hits mixed 50/50); ≤ 16 B/chunk RAM measured with a counting allocator.

---

## M7 — `lunix-store`

**Owns:** `crates/lunix-store`.
**Depends on:** M1.B1. **Model:** routine (careful: this is where the durable
order lives). **Review:** recommended, durable order.
**Reads:** DESIGN §4, §4.3, §6, §13.3; ADR-7, ADR-14; replicix-platform-lite
(`FlockLock`, `FsPrimitives`); Constellation's stale-lock takeover pattern
(ADR-29 of Constellation) as summarised in
`docs/explanation/research/2026-11-11-constellation-reuse.md`.
**Steps:**

1. `VolumeDir::create/open`: the directory layout of M1, `LOCK` via
   `platform-lite` with stale-lock takeover, `volume` and `volume.json`
   atomic writes (tmp, `fdatasync`, rename, directory fsync), `chattr +C`
   on `journal/` when the file system is btrfs, refusal of a directory on
   tmpfs for a volume (unless `LUNIX_ALLOW_TMPFS_FOR_TESTS`).
2. `SegmentWriter`: opens `segments/<seg>.open` (not `.seg`), appends
   extents (buffered writes; large files may use `fallocate`), `seal`:
   footer (plain or sealed through `MetaCodec`, kind `footer`, bucket-relative
   name, D9), trailer,
   `fdatasync`, rename to `<seg:016x>.seg`, directory fsync. Segment ids
   come from the counter (high water from the checkpoint and from a
   directory scan; never reused). Target size 16 MiB; the writer reports
   when full; it can seal early at a cut.
3. `read_extent(hint, expect, out)` (blocking `pread`, plus the
   io_uring-friendly `locate(hint) -> (fd, offset, len)` that the engine
   submits itself), `footer(seg)`, `read_region(seg)`, `list`, `delete`.
   **R1:** `read_extent` takes the `OpenExpect` (leaf `id_prefix` or full id,
   chunk size) and passes it to the codec; there is no overload without
   one. A read with no expectation is a separate `scan_extent`, which sets
   `verify_id`. **R3:** `footer(seg)` returns a footer only after
   `MetaCodec::open(Footer, "segments/<seg:016x>", bytes)` succeeds
   (`map/<pack:016x>` for a pack) and the trailer parses; `read_region`
   (used by fsck, compaction and `catch_up`) recomputes `seg_hash` and
   compares it to the footer's. A container whose trailer or footer fails is
   reported absent (`Err`), never partly used. Open segments from a crashed process
   (`*.open`) are never read.
4. Map packs: `PackWrite`/`PackRead` with the same container and
   `map/<pack:016x>`; index run files are owned by M6 but live in `index/`
   created here.
5. `CommitStore` (`state/commits/<commit_seq:016x>`: write tmp, `fdatasync`,
   rename, directory fsync; `tip()`, `get`, `list`), `CheckpointStore`
   (double slot, checksummed; a bad or stale checkpoint is ignored with a
   warning, since it is only a hint), `state/device` and `state/pins`
   atomic writes. `state/pins` is read and written as `LocalPinsFile`
   (version 2), never through `LocalPins` alone, which drops the rollback
   floor: `lunix-store` switches to `LocalPinsFile` before the floor is
   raised (ADR-31). `VolumeDir::rollback_floor` and `raise_rollback_floor`
   carry the floor (raise-only; an equal `max_commit_seq` with another
   `tip_hash` is refused as well).
6. Orphan sweep at open: delete `*.tmp`, `*.open`, and **every sealed
   segment and map pack whose id is above the tip commit's high-water mark
   (the highest id named by any retained commit) and that no retained
   commit names**, before `index.catch_up` runs (safe: ids are never reused,
   and the journal still holds the data because reclaim is after the commit,
   N2). Sealed files at or below the mark that nothing references are left
   for GC (M17), which treats them as dead objects; the store only lists
   them.
7. GC support: `SegmentStore::delete`, `delete_pack` (index runs are M6's files: `ChunkIndex::delete_run`; the store only creates `index/` and counts its ids),
   `CommitStore::delete(commit_seq)` (refuses at or above the tip's
   `chain_floor`), the local `snapshots/`, `readers/`, `holds/` and `gc/`
   directories (empty in phase 1) behind `PinSource` and `GcStateStore`
   (`state/gc/condemned` atomic write; `put_evidence(pass, body)` writes
   the sealed create-only object `gc/<pass:016x>`: tmp, `fdatasync`,
   rename, directory fsync), listing with sizes. Every delete is `unlink` then directory `fsync`.
8. Crash points: `store.segment.before_fdatasync`, `.after_fdatasync`,
   `.after_rename`, `.after_dirsync`; `store.pack.*` (same four);
   `store.commit.*` (same four); `store.checkpoint.*`;
   `store.delete.before_unlink`, `.after_unlink`, `.after_dirsync`.

**Gate:**

- Round trips for segments of 1, 255 and 4 096 extents (16 KiB and 64 KiB,
  all codecs, plain and encrypted through a ~30-line in-crate test `ExtentCodec`; M7 is
  wave 1a and `PlainCodec` is B2a's, so the crate carries its own fake).
- **Truncation sweep:** a sealed segment truncated at every 4 KiB boundary
  and at 100 random byte offsets fails to open with a typed error and
  never panics; a flipped byte in the footer, trailer or an extent header
  is detected (the footer's BLAKE3 and the trailer magic).
- **Crash sweep:** for each durable-step crash point, kill a child process
  there; reopen; the state is exactly the old one or the new one, never a
  mix; the commit tip is the last commit whose file was renamed and
  directory-synced.
- Stale-lock takeover works after `kill -9` of the holder; a live holder
  blocks a second opener with a typed error.
- `fsync` accounting test: an instrumented file wrapper (test only) proves
  that every seal performs data fsync before rename and directory fsync
  after.
- Never-reused ids: after a crash with orphan segments, the next id is
  above the highest existing file.
- Orphan sweep: a crash between segment seal and commit leaves a sealed
  segment above the high-water mark that the next open deletes before
  `catch_up`; a sealed unreferenced segment below the mark is kept and
  listed.
- Delete sweep: a crash at each delete point leaves either the file or no
  file, the directory entry consistent after reopen; `CommitStore::delete`
  refuses the tip and anything at or above `chain_floor`; the evidence
  object is create-only and atomic (a crash leaves it whole or absent; a
  second `put_evidence` for the same pass is refused).
- Every extent read verifies its 16-byte tag through the codec; a flipped
  payload byte on a plain volume is an error, not data.
- **R1 gate:** on a keyed `integrity` codec, `read_extent` with a prefix
  that is not the extent's fails, for an extent swapped with another valid
  extent of the family, for the same bytes copied to another offset, and for
  an older extent for the same LBA; `scan_extent` of the same bytes
  succeeds only with `verify_id`, which recomputes the id.
- **R3 gate:** a footer copied from another segment (valid for its own
  container name) is rejected; a forged footer with the right trailer and
  checksum fails the MAC on an `integrity` volume; a footer is accepted on a
  `plain` volume only through its unkeyed checksum; extents rearranged inside
  the region with the footer untouched fail `seg_hash` in `read_region`
  though every extent tag is valid; a truncated trailer makes the segment
  absent.

---

## M8 — `lunix-space`

**Owns:** `crates/lunix-space`. **Depends on:** M1.B1. **Model:** routine.
**Reads:** DESIGN §2 "Space policy", §4.3; ADR-14 (ladder); the `SpaceGate`
trait.
**Steps:**

1. Ledger: `used = journal_reserved + segments + cache + map + index`
   (atomic counters per kind, plus the `Headroom` pseudo-kind and the
   atomic `transfer(from_kind, to_kind, from_bytes, to_bytes)`; the
   accounting walk-through of M1.B1 is the specification), limit `min(cache_max, fs_free - free_floor)`
   from `statfs` (refreshed every 1 s and on demand when within 5 % of the
   limit), `free_floor` in bytes or a percentage.
2. Reservation of 2 × segment size plus a commit and map headroom
   (default 256 MiB; configurable) that writes can never take; commits,
   map packs and the checkpoint may.
3. The ladder: callbacks in order (evict clean cache, evict cold synced
   segments, compact, throttle destage), each returning bytes freed or
   "nothing"; in phase 1 `lunixd` registers no-ops for the first, second and fourth
   steps (no cache, nothing synced, no upload) and M17's urgent GC pass as
   **compact**; then `on_full`: `wait` parks the request
   (a `Waiter` handle the engine polls or that signals the queue's wake fd)
   until space frees or `full_timeout` expires, then `NoSpace`; `error`
   returns `NoSpace` immediately.
4. Reads never consult the ledger. A host-FS `EIO` seen by any caller is
   reported with `SpaceEvents::fatal_io()`, which sets the volume read-only.
5. Live reconfiguration of the policy.
**Gate:**

- Model test (proptest, 5 000 sequences with a simulated `fs_free`): `used`
  never exceeds the limit by more than the headroom; waiters wake in FIFO
  order; `full_timeout` fires within 50 ms of its deadline on a fake
  clock; after a million reserve/release pairs the ledger is exactly zero.
- The M1.B1 walk-through (admission, C2 headroom, C4 true-up, C5/C6/C8
  transfers, C9 release, GC delete) replayed as a test leaves the ledger
  exactly equal to the sum of the surviving files; `transfer` is atomic
  under 8 threads (no observer sees the bytes in both or neither kind).
- With `on_full = error`, a write that would cross the limit gets `NoSpace`
  with no state change; with `wait`, it parks and proceeds after a
  release.
- The headroom is never handed to writes but is available to commits.
- A stress test with 8 threads reserving and releasing is race-free
  (`loom` on the core state machine, or a 60 s stress run).

---

## M9 — `lunix-destage`

**Owns:** `crates/lunix-destage`. **Depends on:** M1.B2b (built
against `lunix-testkit`). **Model:** design-heavy. **Review:** required, together
with M11 (the destage and commit integration; the reviewer reads both).
**Reads:** DESIGN §5 in full, §6; ADR-5, 6, 11, 12, 18; ADR-17's
throughput numbers; `bench/ublk-datapath/RESULTS.md` (hash plus LZ4
numbers); `docs/explanation/research/2026-11-11-data-path-hash-compress-encrypt.md`.
**Steps:**

1. **Pipeline function** `destage_cut(cut: OverlayCut, deps) ->
   DestageOutcome` where `deps` bundles `&dyn Journal` (blocking reads),
   `&dyn BlockMap`, `&dyn ChunkIndex`, `&dyn SegmentStore`, `&dyn
   ExtentCodec`, `&dyn SpaceGate`, and the volume parameters. Per chunk, in
   LBA order within a batch:
   1. assemble: fully covered ⇒ overlay data; partial ⇒ read the base
      chunk (map → store; counted as `cold_rmw`) and patch; hole pieces
      patch as zeros; a chunk entirely covered by holes becomes a map hole
      without reading;
   2. zero scan (OR-accumulate over 64 B lanes) ⇒ hole;
   3. `chunk_id` through `ExtentCodec` (keyed with the addressing key in
      `encrypted` and `integrity` mode, D18);
   4. dedup through `ChunkIndex::get_for_dedup` (never a condemned entry)
      and against extents earlier in the same open segment; a `Hinted` hit
      (from a persisted run, M6) is **confirmed first**: read the extent
      header at the hint, compare all 32 bytes of `chunk_id`, verify the
      tag or AEAD through `ExtentCodec`; an unconfirmed hit is a miss and
      the chunk is stored (D17); a confirmed hit sets the map entry and
      stops. **R1:** the base-chunk read of a partial overwrite and the
      confirmation read both pass the expectation (the leaf prefix, or the
      full 32-byte id of the new chunk) through `read_extent`; neither is a
      scan;
   5. compress: policy `auto` = LZ4 probe on the first 4 KiB, raw if the
      saving is under 6 %, else zstd level 1 with a per-worker context;
      `lz4`, `zstd:N`, `none` per the descriptor; under backpressure
      (worker queue deeper than a threshold) degrade to LZ4, then raw;
   6. seal through `ExtentCodec` (a fresh salt per call, by construction);
   7. append to the open segment, which `SegmentStore` rolls at 16 MiB;
      **every segment is sealed before the map transaction commits**, so
      the map never names an unsealed segment;
   8. map update through `MapTxn`; index inserts are **returned** and the
      caller makes them only after the commit naming the segments is
      durable (M11 C8). A failed cut deletes the segments it sealed.
2. **Worker pool:** bounded OS threads (default `max(2, cpus/2)`, configurable), a
   bounded job queue, results returned in order; no tokio. Jobs are
   per-chunk batches of 16 chunks or 1 MiB, whichever is smaller, so small
   chunks do not drown the queue.
3. **Accounting** returned to the caller: bytes in, chunks total, zero
   holes, dedup hits, unique stored, `ulen` and `clen` sums per codec,
   `cold_rmw`, time per stage.
4. Cancellation and errors: a failed chunk fails the cut cleanly (nothing
   partially applied to the map; segments already appended stay
   unreferenced).
5. Crash points: `destage.after_assemble`, `destage.after_segment_append`.
**Gate:**

- Against the testkit fakes with `PlainCodec`: dedup (N identical chunks
  ⇒ 1 extent), zero chunks ⇒ holes and no extent, partial-chunk RMW
  produces exactly the patched content, hole/discard pieces, all four
  codecs, and the map after a cut equals a flat model of the overlay (a
  proptest of ≥ 2 000 random cuts at 16 KiB and 64 KiB).
- **Compression policy:** random data ⇒ stored raw with `clen = ulen` and
  zstd never called (counter); an all-0x41 chunk ⇒ stored compressed; a
  chunk whose first 4 KiB is random and rest zeros is decided by the
  documented probe rule (test fixes the expected decision); `auto` on a
  synthetic text-like corpus stores ≤ 0.6× of the input; degradation
  triggers at the queue threshold and is reported in the accounting.
- Output determinism: same input, same chunk ids and same map; ciphertext
  differs per run (with the `AesGcm` codec, added as a dev-dependency only
  once M2 is merged: this gate item is run by M12 if M2 is not yet merged).
- Throughput, quiet host, no I/O (fakes): ≥ 1.0 GB/s per run with 8
  workers on a text-like 64 KiB corpus (hard floor); the DESIGN §20
  target of 3 GB/s is reported as met or not with the per-stage
  breakdown; zero scan ≥ 20 GB/s per core; BLAKE3 ≥ 4 GB/s per core at
  64 KiB.
- **Dedup confirmation:** a testkit index whose run entry `X → (seg, off)`
  names a valid extent of another id, or an extent whose tag is damaged,
  makes the chunk be stored (a miss), and a later read of `X` returns the
  right data; a correct hinted hit is adopted without a store.
- **R1 gate:** a testkit map whose leaf names chunk A but whose hint points
  at a tag-valid extent of chunk B (keyed `integrity` codec) makes the RMW
  read fail with the id mismatch instead of patching B's bytes into the new
  chunk; the same for a confirmation read. In `integrity` mode two volumes
  with different keys produce different ids for identical data (no dedup
  between them).
- Dedup never targets a condemned segment (testkit index with a condemned
  segment: the chunk is stored again); the outcome lists the segments it
  appended to so the caller can reserve and name them.
- No tokio, no blocking on any `lunix-iface` queue-thread trait.

---

## M10 — `lunix-ublk`

**Owns:** `crates/lunix-ublk`. **Depends on:** M1.B2b (tested with
`MemBlockEngine` and its own file-backed engine). **Model:** design-heavy.
**Review:** required, independent (against the W1-W7 rules and the ADR-16
and ADR-17 text). **Root:** yes; all root runs through `tools/remote.sh
root`, one at a time. **If a command wedges, stop and report; never retry.**
**Reads:** DESIGN §3; ADR-16, 17, 18, 20 and their gate outcomes;
`bench/ublk-recovery/RESULTS.md` (all of it) and `bench/ublk-baseline`,
`bench/ublk-datapath` RESULTS; `spikes/ublk-recovery/src/{main,rawctl}.rs`
and the scripts `run-crash-loop.sh`, `run-features.sh`, `run-resize.sh`,
`rec-lib.sh`, `fsyncer.py`, `heartbeat.py`; `spikes/ublk-baseline/src/main.rs`.
**Steps:**

1. **Raw control ring** (`ctl.rs`): start from `rawctl.rs`. Commands:
   `GET_FEATURES`, `ADD_DEV`, `GET_DEV_INFO`, `GET_PARAMS`, `SET_PARAMS`,
   `START_DEV`, `START_USER_RECOVERY`, `END_USER_RECOVERY`, `QUIESCE_DEV`,
   `TRY_STOP_DEV`, `DEL_DEV`, `UPDATE_SIZE`. Every call has a timeout
   (rule W4) implemented outside the queue threads. `CMD_IOCTL_ENCODE`
   always.
2. **Feature probe** at startup: mandatory flags `CMD_IOCTL_ENCODE`,
   `USER_RECOVERY`, `USER_RECOVERY_REISSUE`, `QUIESCE`, `UPDATE_SIZE`
   (missing ⇒ refuse with a message naming the kernel floor 6.16);
   used-when-present `SAFE_STOP_DEV`, `NO_AUTO_PART_SCAN`; opt-in
   `USER_COPY`; never `NEED_GET_DATA`, zero-copy, `AUTO_BUF_REG`.
3. **Add through the control ring, attach with libublk** (`libublk =0.4.8`)
   as the spike did. Parameters exactly per DESIGN §3: `logical_bs =
   physical_bs = 4096`, `io_opt` = chunk size, `io_min` = 4096, `max_sectors`
   = 2048 (1 MiB), `attrs = VOLATILE_CACHE | FUA` (plus `READ_ONLY` for
   later snapshot mounts), `dev_sectors`, DISCARD parameters
   (`discard_granularity` 4096, large `max_discard_sectors` and
   `max_write_zeroes_sectors`). `nr_hw_queues` default `min(8, cpus/4)`
   (range 1-16), depth 128. `max_io_buf_bytes` is a per-volume attach option
   (default 1 MiB, minimum 256 KiB, D14); `max_sectors` follows it. The
   device record is written between `SET_PARAMS` and `START_DEV`
   (`add_persisting`); a device that was added and never started is DEAD
   and harmless. A volume tag is stored in `ublksrv_flags` at `ADD_DEV`
   (step 5).
4. **Queue threads:** one pinned thread per queue (CPU list from the
   caller), `mlockall`, `prctl(PR_SET_IO_FLUSHER)`, `unshare(CLONE_NEWNS)`
   for the daemon (libublk issue 50), preallocated buffers. Each thread
   builds its `QueueHandler`, translates ublk requests into `IoRequest`,
   and completes through the eventfd-woken `poll`. No `run_target()`.
   Started-request counter per queue. libublk 0.4.8 assumes handlers
   complete at once: after the first `ABORT` it queues no commit and ends
   its loop, and a completion in the same batch as the first `ABORT` keeps
   its in-flight count above zero for ever. The layer keeps its own
   queue-down flag (set by an `ABORT`, or by the control side after
   `QUIESCE_DEV` or `TRY_STOP_DEV`: a queue whose tags are all held gets no
   `ABORT`), commits held requests itself and never blocks in libublk once
   down (ADR-18).
5. **Recovery:** on start, `GET_DEV_INFO` of the recorded device id;
   validate the identity, then `START_USER_RECOVERY` retrying every 100 ms
   on `EBUSY` up to 30 s, re-fetch, `END_USER_RECOVERY`; **no
   `SET_PARAMS`**. ublk has no owner concept; the daemon stores a volume
   tag in `ublksrv_flags` and checks it. A device is recovered only if the
   boot id and volume equal the record's, the state is QUIESCED, or LIVE
   with no living server, the volume tag, queue count, depth, buffer size
   and flags match, and `record.dev_sectors <= dev_sectors <=` the engine's
   size (a grow makes the engine durable, then the kernel, then the
   record; a crash between any two leaves them in this order). The
   recovered device is re-checked after `START_USER_RECOVERY`. A failed
   identity check is an error that leaves the device alone. A LIVE device
   with a living server is `Busy` after the recovery wait, not an identity
   refusal; the caller's own pid counts as not alive (exec-in-place upgrade)
   (M10 re-review R1, fixed in the M10 follow-up).
6. **QUIESCE:** `quiesce_and_exit(timeout)` sends `QUIESCE_DEV`, which
   returns once every queue has an idle command; requests the server
   already holds stay with it. The daemon completes and commits those with
   commands of its own (libublk would drop them), waits until its queue
   threads have ended with nothing in flight, runs the engine's final
   commit, then exits; no `STOP_DEV`, no `DEL_DEV`. The kernel has seen
   every commit, so the successor has nothing to replay. A request the
   daemon cannot commit is left for `REISSUE` (only by `abort()`). On
   timeout (`EBUSY`/`EINTR`) the device stays canceling: new I/O is
   requeued, not served. The caller retries `quiesce` or calls
   `crash_for_recovery()` (abort, recovered by `REISSUE`); it never
   detaches or deletes the device in this state (the device is latched
   `Quiescing`). Never `STOP_DEV` anywhere; never exit with started
   requests except by `abort()` (W1, W2); a fatal error in a queue thread
   calls `abort()`.
7. **Detach:** `try_stop(timeout)`: `TRY_STOP_DEV` (EBUSY ⇒ typed `Busy`,
   the device stays), then drain, then `DEL_DEV` from a helper thread with
   a 60 s watchdog (W5). `TRY_STOP_DEV` answers `EBUSY` while the block
   device has any opener: a mount, an open file descriptor, or udev's
   probe just after the device appeared. Detach of a new device retries
   for a bounded time.
8. **Resize:** `grow(new_bytes)` validates (multiple of 4096, ≥ current,
   ≤ configured maximum) before `UPDATE_SIZE`; shrink and unaligned sizes
   are rejected without touching the kernel (ADR-20). `UPDATE_SIZE` is sent
   only to a device that has a disk (state not DEAD); older drivers
   dereference a null disk.
9. **`lunix-ublk-testd`**: a test daemon with a file-backed `BlockEngine`
   (`O_DIRECT` backing file, WRITE ⇒ `pwrite`, READ ⇒ `pread`, FLUSH ⇒
   `fdatasync`, FUA ⇒ `RWF_DSYNC`, as in the spike) with subcommands `run`,
   `recover`, `quiesce`, `del`, `features`, `grow`, and fault toggles for
   the negative controls (drop writes; disable `REISSUE`). The mounted cells
   are **drop-all** controls (a daemon kill is not a power loss, so a single
   lost write is never read back there); single-write sensitivity comes from
   the raw-device `DeviceOracle` control that drops one acknowledged write.
   Durability against a crash with volatile state belongs to M15. Port the
   spike's harness scripts to `crates/lunix-ublk/tests/root/` for it,
   keeping their watchdogs.
10. **Crash points** for the testd: `ublk.after_fetch`, `ublk.before_commit`.
**Gate** (all on the host, root, serialised; scratch under
`$LUNIX_SPIKE_DIR` (= `$LUNIX_HOST_BASE/m10/scratch`, at most 40 GiB;
`df-guard` before every `mkfs`, backing files recreated between cells, the
whole script under an outer `timeout -k`); leftovers check clean at start
and end):

- Feature table printed and equal to the phase 0 table for the host
  kernel; a missing mandatory flag refuses with the documented message
  (unit-tested with a mocked mask).
- **kill -9 under load** with ext4 mounted and fio verify plus the
  `fsyncer` (acknowledged-fsync check) running: 30 consecutive cycles
  zero fio errors, zero verify failures, `fsck.ext4 -fn` clean, zero bad
  blocks among acknowledged fsyncs; on xfs 10 cycles with `xfs_repair -n`
  clean. Restart delays: immediate (20), 1 s (10), 10 s (3), double kill
  inside the recovery window (3). No stall above 30 s at 1 s delay;
  report p50/p99/max of exec-to-recovered and heartbeat stall (phase 0
  reference: 167 ms and 221 ms p50).
- **QUIESCE handover** ×20 on ext4: clean; the daemon exits without
  `STOP_DEV` (verified by a unit test that no code path builds a `STOP_DEV` command, and by
  the cycles passing).
- `TRY_STOP_DEV` while mounted returns `Busy` and the device and mount
  survive; after `umount` it succeeds and `DEL_DEV` completes.
- **Negative controls fail as designed:** without `REISSUE`, fio sees EIO
  or the fs errors; with the drop-last-write toggle, verify fails.
- `UPDATE_SIZE` 16 → 64 GiB grow under fio on ext4, xfs and btrfs: no
  error, fsck clean. The backing file is sparse and fio is bounded (W3):
  `--io_size` ≤ 8 GiB per run, so the test stays within the ≤ 40 GiB M10
  budget; unaligned and shrink requests rejected by the
  daemon.
- Watchdog test: a deliberately stuck control call (a test hook that never
  returns) is reported at the timeout without killing the daemon.
- Identity test: a stale `state/device` with a different `boot_id`, a wrong
  queue count or a wrong size is refused and the device is untouched.
- `cargo test` unit tests for parameter building, the flag-set rules found
  in phase 0 (`QUIESCE` needs `USER_RECOVERY`; `REISSUE` excludes
  `FAIL_IO`), and the timeout wrapper pass without root.

**Reviewer checklist:** W1-W7, no `STOP_DEV` call site exists, the
`abort()` policy, every control call has a watchdog, buffer lifetime across
recovery, eventfd wake races (a completion arriving while the thread is
blocked in `io_uring_enter`), and that a panic in a queue thread aborts.

**Not to touch:** the spike directories (copy from them).

---

## M11 — `lunix-engine`

**Owns:** `crates/lunix-engine`. **Depends on:** M1.B2b (built and tested
against `lunix-testkit` fakes; real crates are wired in M12). **Model:**
design-heavy. **Review:** required, independent: the commit protocol and
startup recovery, together with M9.
**Reads:** DESIGN §3.1, §4, §4.1, §4.2, §4.3, §8.1, §8.2, §13.3, §14, §19;
ADR-3, 4, 9, 10, 14, 20; the traits of M1.B.
**Steps:**

1. **Write path** (queue thread, non-blocking): reserve journal space
   (`SpaceGate`, charge = `Slot::record_bytes()`), `try_reserve` on the
   queue's lane (which assigns `write_seq`), `append`, update the overlay on
   the `Append` completion, complete the request; `Full`/`NoSpace` parks
   the tag (no `write_seq` consumed) and resumes from a wake-up. A FLUSH
   forms its group with `upto = Journal::last_issued()`. Writes past the volume size or not
   4096-aligned are rejected with `EINVAL`. FUA: as WRITE, then join the
   next flush group.
2. **FLUSH and flush groups:** waiters gather into a group; one
   `submit_flush` per group; while it is in flight the next group
   accumulates; each completion names a `write_seq`. Mode `local`: FLUSH is the
   journal group commit.
3. **DISCARD and WRITE_ZEROES:** a hole record in the journal and an
   overlay hole; partial-chunk holes are handled at destage.
4. **Read path:** overlay pieces first (journal `submit_read`), holes as
   zeros, remaining ranges via `BlockMap::try_get_cached`; a miss, and
   every store read, goes to a bounded **read helper pool** (blocking
   map-node and extent reads, decompression above 16 KiB) and completes
   through the wake fd. A bounded decoded-chunk cache (`read_cache_max`,
   default 128 MiB, CLOCK, keyed by chunk id, engine-internal, not a
   format) serves repeat reads; small decompressions (≤ 16 KiB) run on the
   queue thread. Local-store reads check the extent header, the id prefix
   against the map and the 16-byte tag (AEAD tag, or the plain tag on
   unencrypted volumes, D11), and do not recompute the chunk id; the
   `verify_reads` flag recomputes it (M16 measures its cost). **R1:** every
   such read passes the `OpenExpect` built from the leaf entry (`id_prefix`,
   the volume's chunk size) to `SegmentStore::read_extent`; the codec
   compares the header `chunk_id` with it and a mismatch is an
   `extent_auth_failures` event and an error, never data. The engine has no
   read without an expectation: only fsck and GC scans do, with `verify_id`. A **stale
   hint** (segment gone, or the extent header's id does not start with the
   entry's prefix) falls back to `ChunkIndex::get_by_prefix`, confirmed
   against the full id (DESIGN §7); GC relies on this.
5. **Cut and commit coordinator** (background thread):
   triggers: the commit interval `T` (default 30 s, idle volumes cut
   nothing), `B` dirty bytes (default 1 GiB), journal fill above 50 %, a
   `MapFull` signal, detach, an explicit request, and **pending `moved`
   records from the compactor** (the idle rule does not apply to them: a
   compaction output segment is named by a durable commit only through the
   cut that applies its `rehint`, and an idle volume would otherwise leave
   it unnamed; M17 rule 4). **The local FULL
   commit protocol, in durable order** (each step has a crash point named
   `engine.cN.<what>`):
   - C1 pick `c` = current `write_seq`; wait until every write with `write_seq ≤ c`
     has reached the journal (in-flight only; new writes are not stalled);
   - C2 `overlay.cut(c)`;
   - C3 destage the cut (M9): extents appended to segments, **every
     segment sealed** (footer, trailer, `fdatasync`, rename, directory
     fsync) **before the map transaction commits in memory**, so no reader
     can reach an unsealed segment between C3 and C4; index inserts held
     back. A failed cut deletes the segments it sealed;
   - C4 fix `new_segments` (including the output segments of applied
     `moved` records) and true up the ledger to the real size of the
     segments already sealed in C3 (crash point `engine.c4.named`);
   - C5 `map.seal` into a map pack: `fdatasync`, rename, directory fsync;
   - C6 build the FULL commit (`commit_seq = parent_seq + 1` (D5; not the
     `write_seq`), `epoch` 0, `parent_seq`, `parent_hash`, `cut_seq = c`, `size`,
     `map_root`, `map_packs`, `new_segments`, `index_packs` (the runs that
     were durable **before** this step, covering segments of commits ≤
     `commit_seq − 1`, D3), `chain_floor` (the older of the `retain_last` FULLs
     that remain after this one; it never moves backwards), `created_at` = `max(now, parent.created_at)`,
     the writer clamp of D17), seal or
     MAC it through `CommitCodec`, `CommitStore::put` (tmp, `fdatasync`,
     rename, directory fsync). **This is the durability point of the cut.**
   - C7 write the checkpoint hint (per-lane journal tails after `c`,
     `id_high`, counters, stats) and **raise the rollback floor** in
     `state/pins` to `{commit_seq, hash of the commit just created}` (D19:
     an atomic raise-only write, only after C6 is durable, so the floor is
     never above the durable tip; a crash before it leaves a lower floor,
     which is safe);
   - C8 insert the cut's extents into the index (only now: the commit
     naming their segments is durable, so a failed cut never leaves dedup
     pointing at an orphan) and persist the index run for the sealed
     segments (named by the **next** FULL, D3; a commit never names a run
     that is not yet durable);
   - C9 `journal.reclaim(tail)` and `overlay.release(cut)`; update the
     space ledger by the walk-through of M1.B1.
   - C10 if no cut is pending and the in-memory map root equals the new
     tip's `map_root`, give the `Collector` one budget-limited `step`
     (M17); apply the `moved` records it returns with `MapTxn::rehint` at
     the next cut's C3. Commit objects below `chain_floor` become GC
     candidates only after C6 of the FULL that recorded the new floor.
   A crash before C6 completes leaves the previous commit as the tip; the
   new segments and pack are orphans handled at open. The journal is never
   reclaimed before C6 is durable.
6. **Startup recovery** (`Engine::open`), in this order (DESIGN §4.4): take
   the lock; read and verify `volume`; unlock keys (the caller passes
   `Keys`); `check_attach`; read the tip commit, require a FULL and verify
   the chain from the tip down to `chain_floor` (the floor commit's parent
   may be absent); **no automatic fallback**: a tip that does not
   authenticate is a typed mount error, because falling back to the
   previous commit would reuse the create-only name `commits/<tip>` and lose
   journal records already reclaimed; the repair is offline, `lunix fsck
   --repair --truncate-tip` (M14); **R4 before any change:** compare the
   verified tip with the floor in `state/pins` (all three modes):
   `commit_seq` below `max_commit_seq` is `Rollback { floor, tip }`, the
   same `commit_seq` with another hash is `Fork`, both typed mount errors; a
   first attach (no floor) succeeds, sets the floor and logs that rollback
   of earlier history cannot be detected on this host; where a lease exists
   (a backend volume, phase 2; a local volume has none) open it only
   through `open_lease_fresh(bytes, max(chain epoch, pins.max_epoch))`; the
   checkpoint's `last_commit_seq` is never an anchor; then the **orphan
   sweep** (delete every sealed segment and map pack above the tip's
   high-water mark that no retained commit names, N2; before deleting,
   record the highest id on disk, and set the `ObjId` allocator to 1 + the
   maximum of the checkpoint's `id_high`, the highest id the chain names
   and that on-disk maximum, so a swept id is never reissued, D17). The
   sweep is safe because the tip's root pack id bounds every referenced
   segment: segments of a cut are allocated before its map pack, and
   compaction output is named only by the cut that applies its re-hints, so
   the high-water mark (which includes `map_root.pack`) is never below a
   referenced segment. Then `index.catch_up(segment ids named by the
   chain, ...)`; open the map at `map_root` with the commit's `size` (the
   engine passes `size` to `open`); read the checkpoint hint (discard it if
   its `last_commit_seq` is above the tip's `commit_seq`, D17);
   `Journal::recover(hint, floor = the tip FULL's cut_seq)`; `ignored_after_gap`
   and `damaged` are logged before any lane opens, and `on_journal_damage =
   refuse` returns a typed error before the wipe; rebuild the overlay from
   the records it returns; set the next `write_seq`; raise the floor to the
   tip when it is absent or lower (after a successful open, and whenever an
   EPOCH commit is written); resume a pending GC state from `state/gc/`;
   only then accept I/O. The position of the journal recovery relative to
   the sweep is free; the sweep must precede `catch_up` and the map open,
   and R4 must precede every write. Recovery is idempotent (a crash during
   recovery is another recovery).
7. **Resize (grow only):** validate; write a FULL commit with the new size
   and `map.grow`; then call the `on_resize` hook (the ublk layer issues
   `UPDATE_SIZE`). Shrink returns `Unsupported` in phase 1.
8. **Space integration:** `on_full` outcomes become `ENOSPC` or parked
   tags; a fatal host `EIO` makes the volume read-only (writes get `EROFS`
   semantics via `EIO`, reads continue).
9. **Counters** (`stats`): bytes in/out, ops by kind, flush group sizes,
   chunks total, zero, dedup hits, stored `ulen`/`clen`, `cold_rmw`,
   journal fill, dirty leaves, commits, stage timings, extent auth
   failures; exposed as a snapshot struct; persisted in the checkpoint.
10. Crash points as above, plus `engine.recover.*`.

**Gate (testkit fakes; no root; chunk sizes 16 KiB and 64 KiB):**

- **Model test** (proptest, ≥ 500 sequences of 2 000 ops per chunk size;
  ops: write, read, flush, FUA, discard, write-zeroes, force-cut, grow,
  `crash+reopen`): reads always equal the flat oracle while running; after
  `crash+reopen`, the device equals the oracle as of **some** `write_seq` ≥ the
  last completed FLUSH's `write_seq` with writes applied in `write_seq` order (prefix
  consistency), and no FLUSH-acknowledged write is missing. The fakes' crash
  model drops or tears unflushed journal records and unsynced store files
  at random.
- **Every crash point** `engine.c1..c9` and `engine.recover.*` is hit by at
  least 5 distinct random workloads (a coverage counter fails the test if a
  point is never reached), with the same invariants after reopen.
- Order tests: a fake store that fails if any commit names an object that
  is not durable (C6 after C4/C5), and a fake journal that fails if
  `reclaim` is called before the commit is durable.
- **D17 cases:** a commit written after a clock step backwards has
  `created_at` equal to its parent's, and mount of a chain whose
  `created_at` decreases succeeds (fsck warns); after a crash with
  orphans above the tip, and after `--truncate-tip` followed by an
  orphan sweep, the allocator never hands out an id that was present
  before the sweep; a checkpoint from beyond the tip is ignored and the
  journal records between the new and the old cut are replayed.
- **R1 gate:** over a keyed `integrity` codec and the encrypted one, a
  store fake that returns a different valid extent of the family for a
  hint (swap), the same extent at a stale or wrong offset, and an older
  extent for the same LBA makes the read fail with `extent_auth_failures`
  incremented and no data returned; a stale hint whose id merely does not
  match falls back to the index (and still confirms the full id); no read
  path in the engine compiles without an expectation (API test: the only
  no-expectation call site is behind `scan`).
- **R4 gate (rollback floor, D19):** with a floor `{F, H}` in `state/pins`:
  a chain whose tip is `F-1` or lower is `Rollback`; a chain with tip `F`
  and another hash is `Fork`; tip `F` with hash `H` or above `F` opens and
  raises the floor; a FULL commit raises the floor only after C6 is durable
  (order test: a pins fake that fails if the floor exceeds the durable tip);
  a crash between C6 and the floor write reopens cleanly with the lower
  floor; the floor never decreases through any engine path; a volume with
  no floor attaches, sets one and logs the notice; an old sealed lease
  below `max(chain epoch, pins.max_epoch)` is rejected when a lease fake is
  present. The checkpoint with `last_commit_seq` above the tip is ignored
  and does not change the floor.
- **Named cases** (N1, N2, N4): ring wrap with a **lost** checkpoint; a
  crash between C4 and C6 followed by re-destage of the same data (the
  orphans are swept at open and dedup never adopts them); a commit never
  names an index run that is not yet durable; stale-hint fallback after a
  compaction move (a fake collector that relocates extents); the
  `Collector` is called only between cuts and only with map root = tip
  root (assert in the hook); the ledger equals the bytes of the files in
  the fake store after every commit, exactly outside the C2-C9 window and
  within one commit's headroom inside it.
- FLUSH grouping: 1 000 concurrent flush requests complete with at most
  one flush group in flight (one `fdatasync` per lane with unflushed
  appends) and at most two groups outstanding; every waiter
  completes; a FLUSH never completes before an earlier acknowledged write
  is durable.
- Queue-thread discipline: a test wrapper marks queue-thread entry points
  and panics if a blocking fake method is called from them.
- Read path under a slow store fake (10 ms): throughput of cache-hit reads
  is unaffected; misses complete; no tag is lost or completed twice
  (tag-state assertions).
- DISCARD of a partial chunk, WRITE_ZEROES over holes, grow across a depth
  boundary, and `NoSpace`/`wait` behaviours each have a named test.
- Idle volume creates no commit in 5 × `T` (fake clock).

**Reviewer checklist:** R1 has no read path without an expectation; R4 and
the floor write ordering (never above the durable tip); the C1-C10 order against ADR-9 and §4.1; the
orphan sweep before `catch_up`; floor-based recovery; the GC hook's
preconditions; recovery
idempotence; the prefix-consistency invariant; that FUA and FLUSH are
cuts; no path completes a write before its journal record completes.

---

## M12 — `lunixd` and the in-process stack tests

**Owns:** M12a: `crates/lunixd` except `tests/stack_*.rs` and the body of
`src/testsupport.rs`. M12a adds the `testsupport` cargo feature to
`Cargo.toml`, `pub mod testsupport` behind it in `lib.rs`, and creates
`src/testsupport.rs` with the final signature of the builder
(`not implemented` bodies). M12b:
`crates/lunixd/tests/stack_*.rs` and `crates/lunixd/src/testsupport.rs`
(the in-process volume builder, which M14 and M17.B reuse), editing no
other file. **Depends on:** starts when M11, M3, M4 and M7
have merged and wires the real crates incrementally, with fakes for the
rest (the stack test is parametrised by which crates are real); its gates
need M2, M5, M6, M8, M9, M10, M17.A (and M13a for CLI-driven tests). Two
agents may split it: M12a (control plane and daemon) and M12b (stack tests,
`testsupport`). **Model:** design-heavy. **Stop and report, never retry** a
wedged device in the smoke test.
**Review:** recommended (key flows, state machines). **Root:** only the
device smoke test.
**Reads:** DESIGN §2, §3, §4, §15, §16, §18; ADR-16, 17, 22 (volume id
`lunix1:<uuid>` is phase 6; do not implement); `lunix-iface::ctl`.
**Journal damage at startup (M3 review, lead 2026-11-12):** before the
first `open_lane` of a recovered volume, log `ignored_after_gap` and every
`damaged` range with seq bounds; honour the mount option
`on_journal_damage = wipe | refuse` (default `wipe`; `refuse` keeps the
volume offline for `fsck`). Gate: a test with a damaged record above the
floor shows the log line before the wipe, and `refuse` leaves the journal
untouched.

**Steps:**

1. **Config** (`/etc/lunix/lunixd.toml`, overridable): state dir (default
   `/var/lib/lunix`), control socket path (default `/run/lunix/ctl.sock`,
   mode 0600 root, optional group), metrics address (off by default), log
   level. Registry `<state_dir>/volumes.json` (name → uuid, directory),
   atomic replace, rebuildable by scanning volume directories (D8).
2. **Startup hardening:** `forbid_core_dumps` (platform-lite),
   `PR_SET_DUMPABLE 0`, `mlockall`, `PR_SET_IO_FLUSHER`, own mount
   namespace; the tokio runtime exists only for the control server and
   metrics; queue threads are spawned by `lunix-ublk`.
3. **Control server** over the socket: framing from `lunix-iface::ctl`,
   one task per connection, every mutating call audit-logged (who, what,
   outcome; secrets redacted), bounded frame size (1 MiB), per-call
   timeouts. Handlers: `Ping`, `VolumeCreate`, `VolumeList`, `VolumeStatus`
   (state, size, chunk size, encrypted, the rollback floor, device path, counters, space,
   journal fill, last `commit_seq`/time, dedup and compression ratios),
   `Attach` (with `max_io_buf_bytes`), `Detach`, `Resize`, `Space`,
   `QuiesceAll`, `Gc` and `GcStatus`.
4. **Volume create:** make the directory, `volume` descriptor with its MAC,
   keyring (if encrypted; the unlock secret arrives as a `Secret`),
   `volume.json`, the genesis FULL commit (empty map), registry entry.
5. **Attach:** `check_attach` (including `Downgrade`/`WrongKmk`), open the
   engine (recovery; its R4 check returns `Rollback` or `Fork` against the
   floor in `state/pins`, which the handler maps to distinct control error
   codes and the CLI to distinct messages, and a first attach on a host with
   no floor returns an informational notice that history before it cannot be
   checked for rollback), create or recover the device through `lunix-ublk`
   (recover when `state/device` matches, `boot_id` equal and the device is
   QUIESCED/recovering; otherwise add a new device), create the
   `/dev/lunix/<name>` symlink (udev rules are phase 8) and write
   `state/device`.
6. **Detach:** `TRY_STOP_DEV` (`Busy` ⇒ `Err(Busy)`, nothing else
   changes), final cut and commit, `DEL_DEV`, remove the symlink and
   `state/device`, release the lock. `--force` is not implemented in phase
   1.
7. **Signals:** `SIGTERM`/`QuiesceAll` ⇒ quiesce every device, then exit
   with no `STOP_DEV`; on quiesce timeout keep serving and log. `SIGUSR2`
   is the same as `QuiesceAll` (used by the harness). `SIGKILL` is the
   recovery path.
8. **On start:** for each registry volume whose `state/device` exists,
   recover it (step 5) before accepting control calls; log the stall.
9. **Metrics (deferrable: M12 may drop this step if the schedule needs it;
   ROADMAP phase 1 does not list it):** `/metrics` Prometheus text on the configured address
   (hand-rolled, no web framework): per-volume IOPS and latency
   histograms by op, journal fill, dirty bytes, destage and dedup and
   compression ratios, cache hit ratio, `cold_rmw_total`, commit count,
   space figures.
10. **`lunixd --list-crashpoints`** prints the registry; the `crashpoints`
    feature builds a binary that honours `LUNIX_CRASH_AT`.
11. **In-process stack tests** (`crates/lunixd/tests/stack_*.rs`, no root):
    real crates wired together over the real file system on the scratch
    directory with a `MemBlockEngine`-style driver that calls the
    `QueueHandler` directly. Includes the encrypted and unencrypted
    volumes and both chunk sizes. M12b exports the volume builder as
    `lunixd::testsupport`.
12. **GC wiring:** register `lunix-gc`'s `Collector` with the engine's
    coordinator and its urgent pass as the space ladder's compact step;
    implement the `Gc` and `GcStatus` handlers (`--dry-run` uses the same
    code path without deleting); read `retain_last`, `gc_grace`, `gc_rate`
    from `volume.json`.

**Gate:**

- **Stack model test:** the M11 model test again, with the real journal,
  overlay, map, index, store, space, destage and crypto crates, ≥ 100
  sequences of 2 000 ops per chunk size and per encryption setting; the
  crash is a drop of the engine plus a byte-level corruption of each lane
  past its last flush point and deletion of files not synced (the store's
  test hook); every `engine.*`, `store.*`, `journal.*`, `index.*`, `destage.*` and
  `gc.*` crash point is exercised by a child-process `SIGKILL` run at
  least 3 times (first hit, a middle hit, the last hit of a workload), with
  `lunix-fsck` (M14, if merged; otherwise the store-level self-check) clean
  after each. This runs at both chunk sizes and both encryption settings
  and is green **before M15 starts**.
- **Control:** every request type against a running `lunixd` (spawned as a
  child on a temp socket) returns the documented result; `Detach` of a busy
  volume returns `Busy`; secrets never appear in the audit log or in any
  response (grep test with a canary passphrase).
- **Downgrade:** attaching an encrypted volume without a credential is
  allowed to fail, attaching an `integrity` volume made by default (key
  file beside it) succeeds with no extra argument, attaching a `plain`
  volume succeeds with no credential and returns `Downgrade` when one is
  supplied, attaching an unencrypted volume with an encryption credential
  returns `Downgrade`, and a wrong fingerprint returns `WrongKmk`, over the
  socket.
- **Rollback (R4):** over the socket, a volume directory whose commit
  chain is replaced by an older valid prefix returns `Rollback`, a chain
  forked at the floor height returns `Fork`, and the honest volume attaches;
  `VolumeStatus` shows the floor; the first attach on a fresh state
  directory succeeds with the notice. This holds in all three modes. The
  plain statement is in the CLI help: a purely local volume cannot be
  protected from someone who can write its local disk.
- Device smoke (root, run through `tools/remote.sh root`, serialised, ≤ 8 GiB of host disk): create, attach, `mkfs.ext4`, mount,
  write 1 GiB with fio verify, `umount`, detach, re-attach, verify, `lunix
  fsck` clean; one `kill -9` and recovery under the same load.
- Idle daemon CPU ≤ 1 % of one core; resident memory with one idle 1 TiB
  volume at **4 queues** (the host default `min(8, cpus/4)`) ≤ mlocked
  queue buffers + 150 MiB (4 × 128 × 1 MiB = 512 MiB of buffers, so
  ≤ 662 MiB); the report states the buffer figure separately, and the
  measurement at 8 queues for information.

---

## M12 inputs from the M9-M11 reviews

Collected on 2026-11-13 from the M9, M10 and M11 reports and their review
and re-review sections. Items marked *decided* are settled in the review or
by the lead; the others are open for M12 or the maintainer. Nothing here
changes a milestone gate on its own; M12 lists in its report what it took
and what it left.

**From the M9 re-review (the destage adapter, R1-R7):**

- **R1 (decided: classify once).** The adapter copies `DestageError::class`
  into the engine's `Retry` with an exhaustive `match` (an
  `impl From<ErrorClass> for Retry`), never calls `Retry::classify` on
  `error` again (M9 samples `dirty_leaf_count()` after the failure,
  `SimpleDestager` before the call; two copies of the table drift), and a
  test covers every `ErrorKind`.
- **R2 (open).** A failed delete of a failed cut's segments is silent in
  M9's error arm (`if let Ok(n) = deps.segments.delete(..)`), and
  `Partial::segments` does not say which ids were not deleted. Return the
  undeleted segment ids (or count them) so the adapter logs them and keeps
  the ledger charge; the file sits outside the ledger until GC otherwise.
- **R3 (decided).** `freed_bytes` has no consumer and its rustdoc misleads:
  nothing was charged beyond the C2 bound, which the coordinator releases
  whole on failure. Treat it as informational; the adapter must not release
  it a second time.
- **R4 (open).** `AfterMapSeal` means "seal, then retry the same cut" in M9
  and `CutFail::Smaller` (a lower `write_seq`) in the engine. If the map is
  at its cap, `begin_update` fails whatever the cut size, so "smaller"
  cannot converge. Harmless today (every commit's C5 seals the map and a
  commit that fails after C3 stops the engine, so a cut never starts with
  dirty leaves); say so in one sentence of the adapter's rustdoc, or make
  the coordinator seal on this class.
- **R5, R6, R8 (nits).** The lean-volume test does not pin `verify_id`;
  `dedup_rejected` now also counts a `Verified` hit with an absurd `clen`
  (fix its doc in `stats.rs`); the store trait still says `delete` is "GC
  only" although M9 and the engine's stand-in call it for their own
  unnamed segments (widen the sentence in `lunix-iface`).
- **R7 (open, maintainer).** `DestageCtx` has no `pressure` carrier. The
  adapter derives it from `Journal::fill()` per call. **The thresholds
  (fill to `Lz4` or `Raw`) are undecided**; with `Pressure::None` only cuts
  above 4 GiB degrade.
- **The adapter mapping (M11 report, request 3).** M12 adapts M9's
  `destage_cut(&OverlayCut, &DestageDeps)` to the engine's `Destager`:
  the cut is borrowed (the engine keeps it for `Overlay::release`);
  `DestagedSegment.sealed` maps to `DestageOutcome::segments` (all sealed
  before the map transaction commits); `DestagedSegment.entries` flattened
  map to `index_inserts` (inserted only after C6 is durable); `DestageStats`
  map to the counters in the order hash, dedup, compress, seal, append;
  `extent_auth_failures` is carried (M9 does its own base reads; reads
  through `ctx.reader` are counted by the engine); `DestageCtx::reader` and
  `::writes` go unused. M9's "cut too large" (`CutTooLarge`) and
  `AfterMapSeal` both come through as the retry-smaller path; M9's per-chunk
  pre-probe of the map is kept (the engine's `SimpleDestager` has none).

**From the M10 review and re-review (the ublk layer):**

- **What M12 must call.** `harden_process` first; `UblkDevice::add_persisting`
  for the first start (with the record writer, run between `SET_PARAMS` and
  `START_DEV`); `UblkDevice::recover` for later starts (on
  `UblkError::Identity` add a new device); `record()` after every grow;
  `quiesce_and_exit` for an upgrade; `detach` for a stop (retry on `Busy`
  for a bounded time). Handle `UblkError::Device { dev_id, .. }` (record
  the id; another process deletes the device with `delete_by_id(.., true)`
  once the daemon is gone) and `warnings()`. libublk prints `device reload
  json failed` once per attach; it is harmless (a line for the operator
  notes). Persisting the record after an engine-initiated grow through the
  resize hook is the daemon's job. Handlers are dropped only after a drain.
- **R4 (open).** A crash between `persist` and `START_DEV` leaves a DEAD
  device nothing deletes. The next start gets `Identity { State { 0 } }` and
  adds a new device, as documented; the old one stays until reboot. It is
  the layer's own device (the tag matches): either `recover` reports a
  distinct "never started" refusal and deletes it, or M12 calls
  `delete_by_id(id, .., false)` on it.
- **From the M10 re-review, fixed in the M10 follow-up except R4:** R1, a LIVE device
  with a living server is reported as an identity refusal instead of `Busy`
  after the recovery wait (fix before M12 uses `recover`); R2, the result of
  a drain commit is never looked at; R3, the resize hook outlives the
  device (`ResizeBridge` must refuse unless the lifecycle is `Serving`);
  R5, `quiesce()` does not latch `Quiescing` when `QUIESCE_DEV` hits its
  watchdog. The drop-one-FUA control cannot trip by construction (R6); the
  M10 gate line "with the drop-last-write toggle, verify fails" holds for
  the mounted cells as "drop-all", and a raw-device `DeviceOracle` negative
  control is the sensitive one. Durability of FUA/FLUSH against volatile
  state belongs to M15.

**From the M11 re-review (the engine):**

- **Minor 7 (open).** `bound_cut` ignores `map.dirty_leaf_count()`, which
  after a failed cut holds the re-hinted leaves; a persistent `MapFull` can
  then stall commits (unreachable with defaults). Fix: bound by
  `cap - dirty_leaf_count()` (at least 1), and run one empty cut when
  `Smaller` arrives at `c <= prev + 1` with dirty leaves present.
- **Minor 8 (open).** `EngineConfig::from_volume_json` hard-codes
  `map_fanout: 512`; M12 must set it from the descriptor (better: a
  constructor argument next to `chunk_size` and `id_len`).
- **Minor 9 (document).** `Queue::drop` poisons the volume when numbers are
  still in flight; M10 and M12 document and keep "drain before drop".
- **Journal-reclaim after open.** `fill()` overstates the ring after a crash
  between C6 and C9 until the first commit; M12 may call `commit_now(false)`
  once after open for an exact ledger.
- **Journal damage logging.** The engine logs one `warn!` with the `Debug`
  of the ranges (bounds included); M12's gate test accepts it or asks for
  one line per range.
- **The floor store.** The engine keeps its own `RollbackFloorStore`; M12
  provides a blanket implementation over `VolumeDir::rollback_floor` and
  `raise_rollback_floor` so the engine never depends on the iface change.
- **`Moved` carries the chunk number (accepted for later).** `Moved` should
  carry `ci: ChunkNo` so the engine calls `rehint(ci, old, new)` directly;
  today it walks the tip's leaves at every cut that applies moves. If the
  lead prefers not to touch `lunix-iface`, M17 can hand the engine
  `Vec<(ChunkNo, Moved)>` through `GcProgress`.
- **`GcControl` wiring.** Not implemented in the engine. M12 implements
  `run_pass`/`run_urgent` as requests the coordinator services at C10 (or
  from `tick` when idle) under the same preconditions as `gc_step`: a small
  `Engine::run_gc(req)` that takes the coordinator mutex, because an urgent
  pass must also run without a dirty overlay.
- **R1 over the encrypted codec.** The testkit has no encrypted codec, so
  the R1 gate cases (swap, older extent, damaged tag, wrong offset, stale
  hint) run in M12 over `lunix-crypto`, together with the lease part of R4
  where a lease exists.

---

## M13 — `lunix-cli`

Two work items so the wave 1 part never links crates that do not exist yet.

### M13a — socket commands (wave 1a)

**Owns:** `crates/lunix-cli` (all of it, including stubs for `src/fsck.rs`
and `src/key.rs`: `pub fn run_fsck(args) -> ExitCode` and `pub fn
run_key(args) -> ExitCode`, final signatures, bodies print "built in M13b"
and exit 2, wired in `main.rs` and the `clap` tree here).
**Depends on:** M1.B1 (the fake control server is in M1.B2b; M13a tests
against it once it lands, and against a throw-away in-test server before).
**Model:** routine. **Reads:** DESIGN §16 (CLI sketch); `lunix-iface::ctl`.
**Steps:** the `lunix` binary with `clap`:
`volume create NAME --size 200G --dir DIR [--chunk 64K] [--encrypt |
--integrity | --plain] [--passphrase] [--compress auto|lz4|zstd:N|none]
[--id-len 16|32] [--key-file F]` (D16: with no mode flag the volume is
`integrity`, with a key-file slot at `<dir>/<uuid>/integrity.key`, 0600, so
`attach` needs nothing extra; `--passphrase` adds a passphrase slot for a
volume that moves; the three mode flags are mutually exclusive; `--plain`
prints a warning that plain mode detects corruption only and protects
against nobody who can write the bucket),
`volume list`, `attach NAME [--queues N --depth D --cpus L
--max-io-buf 1M]`, `detach NAME`, `status [NAME] [--json]`, `resize NAME
SIZE`, `space NAME --floor X --on-full wait|error --full-timeout T
--cache-max X`, `gc NAME [--dry-run]` (control socket; prints the pass
summary or the dry-run report). The `fsck` and `key` subcommands exist as
stubs that print "built in M13b" and exit 2. Sizes parse `K/M/G/T` as
powers of 1024. Passphrases are read from a TTY prompt, `--passphrase-fd N`
or `--key-file`; never from argv or the environment. Exit codes: 0 ok, 1
generic error, 2 usage, 3 busy, 4 not found, 5 no space, 6 downgrade or
wrong key. `status` prints human text and `--json`.
**Gate:**

- Every socket subcommand round-trips against the fake control server
  (request fields, error mapping, exit codes).
- Snapshot test of `--help` for every subcommand.
- A secret canary passed through `--passphrase-fd` never appears in the
  process arguments, the environment, any log or any output (test reads
  `/proc/self/cmdline` and captured output).
- **Modes (D16):** `volume create` with no flag creates an `integrity`
  volume and the key file is 0600; `--encrypt`, `--integrity` and
  `--plain` together or in pairs are a usage error (exit 2); `--plain`
  prints the warning (snapshot-tested text) on stderr; `--help` documents
  the default. The warning for adding a backend to a plain volume is
  tested against the fake server (phase 2 wires the command; the text
  lives in the CLI now).
- Size parser: accepts `64K`, `200G`, `1T`, rejects `0`, `1.5G`, negatives
  and overflow; property test against `u64` arithmetic.

### M13b — `fsck` and `key` (wave 2)

**Owns:** `crates/lunix-cli/src/fsck.rs`, `crates/lunix-cli/src/key.rs`
(only the bodies of M13a's `run_fsck` and `run_key`; no edit to `main.rs`
or `Cargo.toml`). **Depends on:** M13a, M2, M7, M14. **Model:** routine.
**Steps:** `fsck NAME [--deep] [--rebuild-index] [--repair [--gc] [--truncate-tip]] [--json]`
(offline, links `lunix-fsck`; refuses if the volume is attached), and
offline key-slot commands `key ls|add-slot|rm-slot NAME` (operate on the
local keyring with the volume detached, through `lunix-crypto`; a `plain`
volume has no slots and the commands say so).
**Gate:** both subcommands run against a volume directory built by the
M7/M2 libraries (no daemon): clean volume exit 0, a corrupted volume exit
2, `--repair --gc` on a volume with condemned files finishes the pass; a
secret canary never leaks from `key add-slot`; `--help` snapshots for the
two subcommands; exit codes as in M13a.

---

## M14 — `lunix-fsck`

**Owns:** `crates/lunix-fsck` (all of it). M14 creates `src/gc.rs` with
the final signature `pub fn check_gc(...) -> Vec<Finding>` (empty body),
declares `mod gc;` and calls it from `lib.rs`; M17.B then edits only
`src/gc.rs`. **Depends on:**
M2, M5, M6, M7; its stack-built-volume gate reuses `lunixd::testsupport`
once M12b lands (until then M14 uses its own builder but must not be the
first integration). **Model:**
routine. **Review:** recommended. **Reads:** DESIGN §4, §6, §7, §11
(rebuild), §19; ADR-9, 11; `lunix-iface` store/map traits.
**Steps:** a library and the `lunix-fsck` binary; offline, takes the volume
`LOCK`. Checks, each with a stable code:
1. `volume` parse and descriptor MAC; keyring parse (and with credentials,
   unwrap); `volume.json` and `state/*` parse (the checkpoint is a hint:
   disagreement is a warning). **Rollback floor (D19):** `state/pins`
   parses as version 1 or 2; a tip below `max_commit_seq` is the error
   `rollback`, and a commit at the floor height with another hash is the
   error `fork`; a pins floor above the tip is never repaired silently.
2. **Commit chain** from the tip down to `chain_floor` (the floor
   commit's parent may be absent; the chain runs from genesis only while
   the floor is the genesis `commit_seq`): `commit_seq` contiguous
   (`commit_seq = parent_seq + 1`, D5), `parent_hash`, non-decreasing `epoch`, HMAC or AEAD of every
   commit, the recorded `chain_floor` equals the older of the `retain_last`
   retained FULLs, no commit file at or above the floor missing; commit
   files below the floor are legal only while condemned (pending delete).
3. **Map walk** from the tip's `map_root` (**R2**: every node is opened
   with `kind = node`, codec 0 required, its hash recomputed and compared
   to the parent's `child_hash`, or to the commit's `map_root.hash` for the
   root, **and** to the header `chunk_id`): every node's hash equals the
   recorded hash, every pack exists with the recorded length, depth and
   fanout match the size, every entry of the tip root and of each other retained root
   resolves to an existing extent: through its hint (inside an existing
   sealed segment, id prefix matching the extent header at that offset) or,
   for a stale hint, through the index by prefix confirmed against the full
   id; an entry that does neither is an error. The same check runs again
   after `index/` is deleted and rebuilt from the footers (the delete-time
   index invariant of M17 rule 5).
4. **Segments:** trailer, footer opened through `MetaCodec::open` (**R3**;
   a footer that does not authenticate makes the container absent, not
   partly used), `seg_hash` recomputed over the whole region, footer entries
   against a scan of the extents (offsets, `clen`, `ulen`, codec); `--deep` also opens every
   extent (plain or AEAD tag), decompresses within the chunk-size cap and recomputes the
   chunk id (a scan: `verify_id`, R1); unsealed (`*.open`) and `*.tmp` files; sealed segments or
   packs that no retained root references and the condemned list does not
   name (warning `orphan` when the volume was closed cleanly, otherwise
   `crash leftover`).
5. **Index:** compare runs with the footers (missing, stale, extra
   entries); `--rebuild-index` rewrites it from the footers.
6. **Journal:** scan like recovery with the tip's `cut_seq` as the floor; a
   torn tail is `ok (legal)`; with the resync rule (D17) a damaged record
   is reported with its byte range (an error if it is above the floor and
   so ends recovery, a warning if at or below it); valid records after the
   first gap are a **warning** with their seq range (`ignored_after_gap`,
   N4), not an error. Also warn when `created_at` decreases along the chain
   (D17), and on a `plain` volume print a notice that no check can detect
   a writer who recomputed the checksums.
7. **Ratios and space:** exact dedup ratio, compression ratio, zero/hole
   share and per-category bytes recounted from the map and footers, for
   `lunix status` and M15.
8. `--repair` does only safe things: delete `*.tmp`/`*.open`, delete
   orphans when `--delete-orphans` is also given, rebuild the index,
   rewrite the checkpoint. `--repair --truncate-tip` is the only answer to
   a tip that does not authenticate (mount never falls back): it moves the
   damaged commit object aside (never deletes it) and makes the previous
   commit the tip; it is offline, refuses when the previous commit does
   not authenticate or is absent, **zeroes both checkpoint slots** (the
   checkpoint describes the old tip, and its lane tails would make recovery
   skip records between the new and the old cut, D17), and reports the
   journal records that the older floor can no longer cover, and **lowers the
   rollback floor to the new tip** (the only automatic lowering, offline and
   reported). Records between the new tip's cut and the damaged tip's cut
   that C9 had already reclaimed are gone, so `--truncate-tip` loses those
   writes; this is expected for an offline repair and the how-to says so. `--repair --reset-floor` is the explicit operator reset (for
   example a DR steal from a lagging mirror, phase 5): it sets the floor to
   the current verified tip, prints the old and new values, and is never
   implied by another flag.
Output is human text or `--json`; exit 0 clean, 1 warnings only, 2 errors.
**Gate:**

- Clean on volumes built by the real crates (a test builder that uses
  `lunix-map`, `lunix-store`, `lunix-index`, `lunix-crypto` directly: 3
  commits, 2 000 chunks, dedup and holes, plain and encrypted, both chunk
  sizes) with exit 0 and ratios equal to the builder's known values.
- **Corruption matrix:** a one-byte flip (and a truncation) in each file
  class — `volume`, keyring, a commit, a map pack node, a segment extent, a
  footer, a trailer, an index run, the checkpoint, a journal record
  mid-ring — gives exit 2 with the code documented for that class, except that a flip
  in the checkpoint, a torn journal tail, and a corrupt journal record that
  ends the scan early give 0 or 1 as documented (records beyond the first
  gap are a warning). 
- 2 000 random single-byte flips over all files of a volume: fsck never
  panics, never hangs (60 s watchdog per run) and never reports clean when
  the flip is in covered bytes (`--deep` makes every byte of a stored
  extent covered; the report lists the uncovered padding explicitly).
- `--rebuild-index` after deleting `index/` restores the index to equality
  with the original (via `ChunkIndex` iteration), and every retained-root
  entry still resolves by hint or index afterwards.
- `--repair --truncate-tip` on a volume whose tip is corrupt makes the
  previous commit the tip, keeps the damaged object aside, zeroes the
  checkpoint (a test with a checkpoint whose `last_commit_seq` is above the
  new tip shows every journal record above the new cut is replayed), and
  the volume authenticates at store level (M12b's stack test then mounts
  it); without the flag the mount returns the typed error.
- **Tamper (R1-R4) on `integrity` and `encrypted` volumes:** fsck exits 2
  with a documented code for each of: two valid extents swapped in a
  segment (tags valid; caught by the footer and, with `--deep`, by the id),
  extents rearranged with the footer untouched (`seg_hash`), a footer from
  another segment (R3), a valid node extent in the wrong place, a node with
  codec 1, and a node whose hash differs from the parent's `child_hash` (R2),
  a chain rolled back below the floor (`rollback`) and a fork at the floor
  height (`fork`); `--repair --truncate-tip` lowers the floor to the new tip
  and `--repair --reset-floor` sets it to the verified tip (open iface item for
  M14: `VolumeDir` has no lowering path, since `raise_rollback_floor` refuses a
  lower seq and `store_local_pins` keeps the floor; M14 proposes an offline-only
  `reset_rollback_floor` to the lead before using it); `state/pins`
  corrupted in each byte is detected or harmless; a version 1 `pins` file
  reads without a floor.
- Journal resync: a one-byte flip in a record at or below the floor gives a
  warning and exit 1; the same flip above the floor gives an error with the
  byte range; clean volumes of all three modes (`encrypted`, `integrity`,
  `plain`) exit 0.
- Memory: fsck of a 1 TiB sparse volume with 1 M mapped chunks stays under
  500 MiB.

---

## M17 — Local garbage collection and compaction

**Owns:** M17.A: `crates/lunix-gc` except `tests/`, `docs/reference/gc.md`.
M17.B: `crates/lunix-gc/tests/**` (listed under `m17b` only in
`ownership.toml`), `crates/lunix-fsck/src/gc.rs` (the stub M14 created),
`crates/lunixd/tests/stack_gc*.rs`, `bench/phase1/gc/`.
**Depends on:** M17.A: M1.B2b (fakes); M5 (`lunix-map`, wave 1b) as a
**dev-dependency**, because pack liveness needs real pack ids, `walk_nodes`
and `rehint` and `FlatMap` has no packs (`FlatMap` stays the content
oracle); M6 and M7 only as the traits they implement. M17.B: M5, M6, M7, M11, M14, M17.A and M12b's `testsupport`.
**Model:** design-heavy. **Review:** required, independent, for both
parts (one reviewer reads both; the checklist is at the end).
**Reads:** DESIGN §7 ("Hints", "Chunk index"), §8.7, §11, §4.3; ADR-25,
ADR-30 (GC rule for `dicts/`), ADR-9; M1.B1's `gc` hooks, `PinSource`,
`GcStateStore`, ledger walk-through; Constellation's GC notes in
`docs/explanation/research/2026-11-11-constellation-reuse.md` if they
help (nothing is reused as code).

GC is **required in phase 1**: a local volume that cannot reclaim fills its
disk under any churn. The scope is everything DESIGN §11 describes that a
local-only volume can exercise; backends, leases, mirrors and `moved`
forwarding across backends are phase 2+. Differences from the spec text
that apply to a local-only volume are listed under "Decisions taken" and
in Spec gaps (N6-N9).

### Rules

1. **Roots.** A pass marks from: the `retain_last` retained FULL commits
   (DESIGN §11 and §8.7: `last:N`, default N = 2, the tip and its
   predecessor, so an offline `fsck --repair --truncate-tip` can drop a
   damaged tip; mount never falls back by itself; DELTA commits do not
   exist in local-only mode, but a tip FULL's DELTAs would be roots);
   snapshot, reader and hold pins from `PinSource` (all empty in phase 1,
   but read, so no GC version can delete a future pin's base); and the
   trained dictionaries referenced by live extents (none in phase 1;
   codec-3 extents are counted when they appear). `chain_floor` is the
   older retained FULL's `commit_seq`.
2. **When it runs.** On the engine's coordinator thread, between cuts, and
   only when the in-memory map root equals the tip's `map_root` (M11
   guarantees it; the hook asserts it). Mark, condemn and compaction steps
   are small and resumable (`GcBudget`), so a pass never stalls a cut for
   more than its budget (default 50 ms per step).
3. **Liveness is by chunk id, per segment.** Mark walks every root
   (`walk_leaves`, skipping subtrees shared with an already-walked root by
   hash) and collects the live chunk ids. A leaf entry resolves to an
   extent: through its hint if the segment exists and the extent header's
   id starts with the entry's id prefix, else through the index by prefix
   (stale hints from compaction). Per-segment live bytes are the sum of
   `clen` of the extents the **index names as the current copy** of a live
   id; an older duplicate copy is dead. Volumes with `id_len = 0` have no
   prefix to resolve with: they use hint-only liveness and compaction is
   refused (`Unsupported`); whole dead segments are still deleted.
4. **Compaction.** Candidates are segments with live fraction `u < 0.5`
   (configurable), ranked by LFS cost-benefit `(1-u)·age/(1+u)`, with a
   minimum age of one commit interval. The compactor reads live extents,
   groups them by LBA locality (map order), **re-seals** them through
   `ExtentCodec` (same compression, a fresh salt drawn inside `seal`, as
   ADR-11 requires; the chunk id is unchanged) into new segments through
   the normal `SegmentStore`, and
   returns `Moved {id, old, new}` records. The engine applies them to the
   map with `MapTxn::rehint` at the next cut and the compactor updates the
   index with `relocate`, **only after** the new segment is sealed.
   `relocate` entries follow M6's persist rule: they stay in the memtable
   and are never persisted until the new segment is named by a durable
   commit. A new segment is named by the next FULL's `new_segments[]` like
   any other. **Pending `moved` records are a cut trigger** (M11 step 5):
   the idle rule (an idle volume cuts nothing) does not apply to them, so
   the output segment is named within one commit interval.
   Compaction is rate-limited (default 64 MiB/s, `gc_rate`) and its scratch
   is bounded (one output segment open at a time).
5. **Delete: condemn → grace → recheck → delete**, for segments, map
   packs, index runs and commit objects below `chain_floor`:
   - *condemn*: an object with no live reference **and whose id is not
     above the tip's high-water mark** (the highest id any retained commit
     names) is added to the condemned list (`state/gc/condemned`, with the condemn time); a segment is
     marked in the index (`mark_condemned`), after which `get_for_dedup`
     never returns its entries; a one-line evidence record is added to the
     pass's evidence lines, which are written at pass end as one sealed
     create-only object `gc/<pass:016x>` (rule 11);
   - *grace*: `gc_grace` (a `volume.json` setting, default 60 s, tests use
     the fake clock). DESIGN §11: a local-only volume has no lease, mirror
     or reader pin, so the grace covers only in-flight cuts, destage and
     reads, and is never shorter than that; the setting has a floor of
     30 s (10 s plus twice `read_timeout`, default 10 s, a `volume.json`
     setting), below which it is refused.
     The ladder's *urgent* pass (rule 8) uses the floor, not zero, and the
     recheck is unchanged;
   - *recheck*: re-run mark from the current roots; delete only objects
     that are still unreferenced and still condemned (an object that gained
     a reference is un-condemned);
   - *delete*: unlink, directory `fsync`, `release` in the ledger; segments
     then `drop_segment` in the index; the deletion is fenced on the local
     epoch (0), so the check is a no-op that phase 2 turns into the lease
     fence.
   - *delete-time index invariant*: a segment that stale hints still point
     into may be deleted only if the index durably names the current copy
     of every live id those hints resolve to, or that copy's segment is
     named by the chain (so `catch_up` restores the entry after a crash).
     This holds by construction (`relocate` is never persisted before the
     new segment is named, D3); M17.A tests it and M14 checks it.
6. **Order with commits.** A segment, pack or run named by a retained
   commit is live by the roots rule. Commit objects below `chain_floor`
   are condemned only after the FULL that **recorded** the new
   `chain_floor` is durable (C6), so a crash never leaves a tip whose floor
   is missing. The floor commit's parent may be absent; mount and fsck
   verify the chain from the tip down to `chain_floor` only. An index run
   is live while the current manifest or any retained FULL's `index_packs[]`
   names it.
7. **Orphans.** Sealed segments and packs that nothing references (crash
   between C4 and C6, abandoned compaction output) have zero live bytes and
   are condemned like any dead object, **except** that GC never condemns a
   segment or pack whose id is above the tip's high-water mark: inside a
   running process such a file is the open segment, sealed-but-uncommitted
   output or compaction output, never garbage; after a crash the open-time
   sweep of M7/M11 deletes it (and runs first).
8. **Space ladder.** `lunixd` registers `Collector`'s urgent pass as the
   ladder's *compact* step: it runs mark, condemns, and (floor grace, recheck)
   deletes dead objects whose floor grace has elapsed, then compacts if still short; it returns the bytes
   freed. A write that still finds no space follows `on_full`.
9. **Map packs** are deleted whole only. A pack that holds one live node is
   kept. Map-pack compaction is **not** in phase 1; M17.B measures the dead
   share of surviving packs and reports it (Spec gaps N8).
10. **Crash points** (names registered statically): `gc.mark.end`,
    `gc.condemn.after_list_write`, `gc.compact.after_segment_seal`,
    `gc.compact.after_relocate`, `gc.delete.before_unlink`,
    `gc.delete.after_unlink`, `gc.delete.after_dirsync`,
    `gc.commit_delete.before_unlink`, `gc.evidence.after_put`. Every
    state between two crash points is recoverable: the condemned list is a
    hint (lost or stale ⇒ re-mark), deletes are idempotent, a missing file
    that the list names is skipped.
11. **Formats.** `docs/reference/gc.md` (M17.A) defines the `gc/` evidence
    body: one sealed create-only object per pass, `gc/<pass:016x>`, body
    `{ver: 1, bytes}` with the pass's JSON lines (time, action, kind, id,
    bytes, reason), written once at pass end (and once by an urgent pass);
    a crash before it loses only that pass's evidence. The only mutable GC
    state is the local `state/gc/condemned` (versioned, atomic replace,
    never uploaded). Phase 2 uploads the evidence objects verbatim.
    `gc.md` also defines the GC settings with defaults (`volume.json`):
    `read_timeout` 10 s, `gc_grace` 60 s (floor 30 s), `gc_rate` 64 MiB/s,
    live-fraction threshold 0.5, step budget 50 ms, `retain_last` 2.
    Nothing here changes a frozen envelope (D6).
12. **Mark staleness (why the design is safe; `gc.md` records it and M17.A
    tests it).** Mark, condemn and recheck are separate budgeted steps with
    cuts between them, so the mark that a condemn uses may predate a FULL
    that gained a reference (a dedup hit on a not-yet-condemned segment).
    (i) A pass pins its root set `R` (commit seqs) when mark starts and
    walks exactly `R`. (ii) Condemn excludes any object named by a
    **currently** retained commit's `new_segments[]`, `map_packs[]` or
    `index_packs[]` (rule 6, made operational). (iii) After condemn no new
    reference to the object can arise (dedup excludes condemned entries,
    compaction output is new, `rehint` only targets compaction output), so
    a recheck that walks a root set pinned at **its** start finds every
    reference: any root created later derives from a pinned root and can add
    no reference to a condemned object. (iv) If the root set changes during
    a recheck walk, the walk is still sufficient for the same reason.

### M17.A — `lunix-gc` against fakes (wave 1c)

**Steps:** `Collector` implementation of the M1.B1 hook; mark with shared-
subtree skipping; resolution through hint or index; per-segment, per-pack
and per-run liveness; cost-benefit ranking; the compactor; condemn, grace,
recheck and delete state machine over `GcStateStore`; the urgent pass; the
per-pass evidence object; `GcStats` (live bytes, dead bytes, bytes freed, segments
compacted, write amplification of compaction); `lunix gc --dry-run` data
(what a pass would condemn and compact, and the bytes), without changing
anything; `docs/reference/gc.md`.

**Gate (M17.A; testkit fakes, no root, chunk sizes 16 KiB and 64 KiB,
`id_len` 16 and 32, plus an `id_len = 0` case):**

- **GC oracle.** Drive a random workload (writes, overwrites, discards,
  commits, retention changes, pins added and removed) through the fakes;
  maintain an exact model that computes the set of objects any root can
  reach. After every pass: no object reachable from any root has been
  deleted (checked by reading every root's every block through
  `FlatMap`'s oracle); every unreferenced object is deleted after grace;
  at quiescence the set of existing objects equals the model's reachable
  set plus objects still inside their grace. ≥ 2 000 sequences of 300 ops.
- **Dedup race.** A write that dedups against a segment while it is being
  condemned never targets it: `get_for_dedup` returns nothing from the
  moment of condemn, and a test interleaves destage cuts with every step
  of a pass. A recheck that finds a new reference un-condemns. **Case:** a
  cut with a dedup hit on a segment between mark and condemn (the mark is
  stale, rule 12); the recheck un-condemns it and nothing is deleted.
- **In-flight output.** A compaction output segment sealed but not yet
  named by a commit (an idle volume with pending `moved` records) is never
  condemned (id above the high-water mark); the pending records force a cut
  and the output is named; a crash after `relocate` but before that cut
  deletes the output at open and leaves the old segment named and indexed.
  `relocate` entries are absent from persisted runs until named.
- **Delete-time index invariant.** Deleting a segment with stale hints
  never leaves a live id without a durable index entry or a chain-named
  copy; the test deletes `index/` and rebuilds it after each deletion.
- **Stale hints.** After compaction a retained root's stale hint still
  reads (through index fallback) before and after the old segment is
  deleted.
- **Crash sweep.** Every GC crash point is hit by at least 5 workloads in a
  child process; after reopen, the model's invariants hold, nothing
  reachable is missing, and a second pass finishes the work.
- **Liveness arithmetic.** Per-segment live bytes equal a recount from the
  model for 1 000 random volumes; cost-benefit order matches a reference
  implementation; no candidate at or above the threshold is compacted.
- **Ledger.** After any pass the ledger equals the bytes of the files that
  exist (fake store), exactly.
- **Mark staleness.** The four points of rule 12 each have a test with
  interleaved cuts (root set pinned at mark start; condemn skips objects
  named by a currently retained commit; recheck finds the late reference).
- Compaction output extents decode to the same plaintext and chunk id as
  their source and carry a new salt on encrypted volumes (the tripwire
  build runs the oracle); the index relocation is applied only after the new
  segment is sealed (order test with a fake store).
- A pass over a 1 M-chunk fake volume stays within its step budgets (no
  step longer than twice the budget on the fake clock) and uses ≤ 64 MiB
  beyond the live-id set; the live-id set is a bitmap or a sorted vector,
  documented.

### M17.B — GC on the real stack (wave 2)

**Steps:** `crates/lunix-fsck/src/gc.rs`: the GC-specific fsck checks;
`lunixd`'s wiring of the real `Collector` and the ladder step is M12a's, and
M17.B only tests it through `lunixd::testsupport`.

fsck integration (this is where M14 and M17 meet; M14 already verifies the
chain from the tip down to `chain_floor` and resolves hints by hint or
index):

- the condemned list is parsed; files it names that still exist are
  reported `ok (gc pending)`, files it names that are gone are `ok`;
- a missing file that **no** retained root resolves to is legal; a missing
  file that a retained root needs (after hint and index fallback) is an
  error;
- `--repair` can finish a pending pass offline (`--gc`): mark, condemn with
  floor grace, recheck, delete; the dry-run output is the same code path;
- per-segment liveness recounted from the maps equals `lunix-gc`'s;
  mismatches are errors;
- a crash at any `gc.*` point leaves a volume that `lunix fsck` accepts
  (exit 0, or 1 with only `gc pending` notes).

**Gate (M17.B; real crates over the scratch file system, no root):**

- **Churn oracle on the stack.** Overwrite a 2 GiB region 10 times (and a
  random-overwrite mix at both chunk sizes, encrypted and unencrypted)
  with commits every few seconds on the fake clock and `retain_last = 2`:
  after the final pass and grace, the store (segments + packs + index runs
  + commits) is ≤ 1.3 × live data (compressed, map root's reachable bytes)
  plus 4 × the largest commit's map-pack bytes plus the open segment; the
  report prints the actual figures, the dead share of surviving map packs,
  and compaction write amplification (bytes rewritten per byte freed).
- **Reads under GC.** A concurrent reader verifies every read against the
  flat oracle through the whole churn run, including reads of the previous
  retained FULL's blocks (via a test that mounts that root read-only
  through the map) before and after compaction.
- **Crash loop.** Every `gc.*` crash point plus a random-kill loop (150
  kills, uniformly during churn and GC passes) on the stack; after each:
  reopen, reads equal the oracle as of a prefix-consistent state,
  `lunix fsck --deep` clean, a final pass converges.
- **Space ladder.** With `on_full = wait` and a small `cache_max`, a
  workload that would hit ENOSPC without GC completes because the urgent
  pass frees space; with GC disabled (test switch) the same workload gets
  `NoSpace`.
- **fsck corruption matrix** additions: a deleted needed segment, a deleted
  needed map pack, a corrupted condemned list (ignored), a condemned list
  naming a live object (un-condemned by recheck, never deleted).
- Statistics line in `bench/phase1/gc/RESULTS.md` per CONVENTIONS hygiene.

**Reviewer checklist (both parts):** no code path deletes without
condemn, grace and recheck except the documented urgent pass (which still
rechecks); `get_for_dedup` excludes condemned entries from the moment of
condemn; roots include pins even when empty; the coordinator is the only
caller and the map root equals the tip's at every mark; chain-floor ordering
(floor recorded durable before commits below it are condemned); no object
above the tip's high-water mark is condemned; pending `moved` records force
a cut; `relocate` is never persisted before the segment is named; mark
staleness (rule 12) and the delete-time index invariant (rule 5); compaction
re-seals with a fresh salt; relocate happens after seal; stale-hint fallback is
exercised; the ledger release follows durable deletion; idempotence of every
state after a crash; `id_len = 0` behaviour; the urgent pass cannot starve
cuts.

**Not to touch:** the frozen crates; `lunix-engine`'s commit protocol (the
hook is M11's).

---

## M15 — Phase 1 exit tests: fio verify, xfstests, crash loops, ratios

**Owns:** `tests/exit/`, `bench/phase1/exit/`. **Depends on:** M12a, M12b,
M13a, M13b, M14, M17.B. **Model:** routine (the lead triages failures).
**Root:** yes, serialised. **Stop and report, never retry** a wedged
device. **Reads:** CONVENTIONS "Safety rules" and "Benchmark hygiene";
`spikes/ublk-recovery/{run-crash-loop.sh,rec-lib.sh,fsyncer.py,heartbeat.py,stall.py}`;
DESIGN §21. **Integration rule:** a defect found in another crate is reported with a
failing test (kept in M15's own tree, `#[ignore]`d with the defect id until
fixed); M15 does **not** edit other crates, so `check-ownership.sh` stays
strict. Never weaken a test.
**Steps and gates** (all on the host; `lunixd` built with `--features
crashpoints` for the sweep, plain release for everything else; exit
volumes **32 GiB**, xfstests TEST and SCRATCH **16 GiB each**; `df-guard`
with the script's budget before every `mkfs`; the volume is **deleted and
recreated between cells**; the whole of M15 peaks at ≤ 100 GiB of host
disk; `journal_max` 512 MiB; every script under an outer `timeout -k` with
its wall-clock cap stated; GC (M17) is on with the default retention, and
the report states the space used per cell):

1. **fio verify.** fio `--size` is at most 12 GiB per job group, so a volume
   never exceeds its 32 GiB. Raw device and ext4 and xfs files, `--verify=crc32c`
   with `--verify_state_save=1`, `--do_verify=1`, `--direct=1`, mixes of
   block sizes via `bssplit` (4k-1M), queue depths 1/32/128, jobs 1/8,
   plus `trim` and `write_zeroes` (`blkdiscard -z`) mixed in, each 10 min
   (`--runtime`), chunk sizes 16 KiB and 64 KiB, encrypted and unencrypted.
   Gate: zero errors, zero verify failures, then detach/attach and
   `--verify_only`: zero failures; `lunix fsck --deep` clean.
2. **xfstests** (the lead provides it; build under the host dir; the
   `local.config` is committed under `tests/exit/xfstests/`: `TEST_DEV` and
   `SCRATCH_DEV` are the Lunix block devices, mounts under
   `$LUNIX_SPIKE_DIR/mnt` with `nosuid,nodev`, and no mount tree outside the
   scratch directory). TEST and SCRATCH are Lunix volumes; run `-g quick` on ext4 and xfs at 64 KiB,
   and at 16 KiB. Also run the same groups on a reference (`loop` over a
   file on the same disk, or `brd`) in the same session. Gate: every test
   that passes on the reference passes on Lunix; each test that fails on
   both is listed with its failing reason; each test that fails only on
   Lunix is a defect (report with `results/` excerpts). "passes" in
   ROADMAP is read this way (Spec gaps 17).
3. **kill -9 crash loop.** (a) Random-time: ext4 100 cycles and xfs 50
   cycles, fio verify (`--verify_backlog`) plus `fsyncer`, kills at random
   5-20 s, restart delays 0/1/10 s and double kills; (b) **at every
   step:** for every crash point printed by `lunixd --list-crashpoints`,
   run a workload with `LUNIX_CRASH_AT=<point>:<hit>` for hit ∈ {1, middle,
   last reached} (three runs per point, ext4; xfs for the `engine.c*`
   points; this includes every `gc.*` point; each run is capped at 10 min
   wall clock, and the point list times 3 runs must fit the stated total
   budget, about 20 h), the supervisor restarts in recovery mode; (c) QUIESCE
   handover ×20 via `SIGUSR2`. After every cycle: fio zero errors and zero
   verify failures, `fsyncer` zero bad acknowledged blocks, guest `fsck`
   clean after each phase, `lunix fsck` clean (exit 0, or 1 with only
   orphans) after each detach, and the stall times (p50/p99/max). Both
   negative controls from M10 still fail on the full stack. Stretch (run if
   `dm-flakey` is available, mark "not run" otherwise): power-cut emulation
   under a `dm-flakey` backing device for the local directory, 20 cycles
   with the unflushed region dropped.
4. **Ratios.** `lunix status --json` and `lunix fsck` exact counters after:
   fio with `--buffer_compress_percentage` 0/50/90 and
   `--dedupe_percentage` 0/50/90 at `bs` = chunk size on raw; extraction
   of a Linux source tarball on ext4; a copy of a Debian/Ubuntu root file
   system; random data. Gate: for fio at `bs` = chunk size the measured
   dedup ratio is within 10 % of `1/(1-p)` and the compression ratio within
   15 % of the compressibility knob's expectation (the report shows the
   table whichever side the data falls on); random data stores raw (stored
   bytes ≤ 1.01× logical); results in tables with both chunk sizes.
5. **Report** `bench/phase1/exit/RESULTS.md` per CONVENTIONS hygiene
   (host header, versions, exact fio job files committed next to results,
   raw JSON under `raw/`), the leftovers check output at the end, and the
   ratio tables.

**Gate (M15):** items 1-3 pass as stated; item 4's table is complete and
the thresholds met or explained; the store size after each cell is within
M17.B's bound for its churn; leftovers clean; no device in a stuck
state at the end.

---

## M16 — Performance gate and the ADR-5 chunk-size benchmark

**Owns:** `tests/perf/`, `bench/phase1/perf/`, `bench/phase1/chunksize/`.
**Depends on:** M12a, M13a, M17.B (start after M15's first green fio
verify).
**Model:** routine measurement, with the analysis reviewed by the lead.
**Root:** yes, serialised, **quiet host only**. **Stop and report, never
retry** a wedged device. **Reads:** DESIGN §20;
`bench/BASELINE.md` (the ceilings); CONVENTIONS "Benchmark hygiene" in
full; ADR-5 and its gate outcome; `bench/tracesim/RESULTS.md`.

**Part A — §20 targets** (release build, `lunixd` at **4 and at 8 queues**
(Spec gaps 13): every cell is reported at both with the CPU split, and
judged at the better of the two with the oversubscription stated; queue threads and fio pinned to disjoint
CPUs; `performance` governor if possible; 3 repetitions per cell, median
and min/max, noisy cells flagged; fio command lines committed):

| workload | target (DESIGN §20) | cell |
|---|---|---|
| 4K random read, cache hits | ≥ 500 K IOPS, p99 ≤ 150 µs | 4k randread, QD 128, 8 jobs, working set 4 GiB written first |
| 4K random write, local | ≥ 300 K IOPS (journal-bound) | 4k randwrite, QD 128, 8 jobs, `--runtime=60` |
| sequential write ingest | ≥ 3 GB/s with `auto` on 8 workers | 1M write, QD 32, 8 jobs, compressible 50 % data |
| sequential read, hits | ≥ 5 GB/s | 1M read after the write |

The cold-read and sync-mode rows need S3 and are out of scope. Also run the
4k randread and 1M read cells once with `verify_reads = true` and report
the cost (Spec gaps 10), so the maintainer can decide the default. Every
cell runs under an outer `timeout -k` of 1.5 × its runtime. Gate: each
row meets its target, **or** the report explains the shortfall with
evidence: the ceiling from BASELINE for that cell, daemon CPU per I/O,
`perf stat`/flame graph of the saturated thread, the journal device's own
raw `fio` limit measured in the same session, and the fraction of the gap
each factor explains. No threshold is changed. Also record the idle-to-
loaded memory of the daemon and the destage backlog at the end of each
write cell.

**Part B — ADR-5 benchmark.** Volumes at 16 KiB and 64 KiB (same size,
same `--id-len 16`, `auto` compression, unencrypted), each on ext4 and,
for pgbench, also xfs. Volumes are 48 GiB virtual (pgbench at scale 1 000
holds about 15 GB plus WAL with a 2× margin) and are **recreated between
cells**; `df-guard` before every `mkfs`; peak host disk ≤ 100 GiB;
`journal_max` 1 GiB; GC on with the default retention. Stored bytes are
taken from `lunix fsck` recounts (live data) and also as the on-disk store
size after a final GC pass and its grace. Workloads (host-installed from official sources;
the lead lists what is missing):

1. **fio:** random write mixes with `bssplit` 4k/8k/16k/64k on a large
   file at queue depth 32, 30 min, with `--dedupe_percentage=30` and
   `--buffer_compress_percentage=50`; and a sequential-then-random
   overwrite pattern.
2. **pgbench:** PostgreSQL (version recorded), scale 1 000 (about 15 GB),
   `-c 16 -j 4 -T 1800`, default `full_page_writes`, 8 KiB pages;
   TPS and latency percentiles as well.
3. **VM images:** three raw Ubuntu or Debian cloud images (two adjacent
   releases and one other distribution; the lead supplies them) written
   to the volume, then `apt-get update && upgrade` and a package install
   of at least 200 packages in a chroot on a loop-mounted copy, to model
   image churn.

For each workload and chunk size record: logical bytes written (journal
bytes), dedup ratio, compression ratio, stored data bytes, **map bytes**
(pack bytes per TiB mapped and per GiB written), map seal write
amplification, **daemon CPU seconds per GiB written** (`utime+stime` of the
process, queue threads and workers), **partial-chunk RMW per 1 000
writes** (`cold_rmw`), throughput and latency percentiles, and the §20
Part A cells at both sizes. Three repetitions of the shortest cells; the
long cells once, with the noise caveat stated.

**Advisory rule, fixed in advance** (the maintainer decides; this only
makes the evidence readable): recommend 16 KiB iff, over the VM-image and
pgbench workloads, stored bytes per logical GiB written (data plus map)
are ≥ 15 % lower than at 64 KiB, **and** daemon CPU per GiB is ≤ 1.3×,
**and** the §20 Part A rows that pass at 64 KiB still pass at 16 KiB.
Otherwise recommend 64 KiB. Report which clauses hold, with the numbers.
M16 writes the recommended text as a proposed "Gate outcome" block in its
`RESULTS.md`; the lead appends it to ADR-5 after the maintainer decides.
**Gate (M16):** both parts complete with the tables above and the raw JSON
committed; every shortfall is explained or absent; the recommendation
follows the rule.

---


## M18 — Compression dictionaries on real data (M-DICT study)

Two work items: **M18a** adds the generic pieces to `replicix-core`
(codec id 3, the dictionary blob, a trainer/evaluator crate) so that
Constellation can use them; **M18b** is the Lunix study harness, the data
collection and the note. M18 does not need a running Lunix volume: it
captures from loop images with an offline chunker, and runs **one**
validation cell through a Lunix volume after M12a exists.

**Why in phase 1:** it feeds ADR-30's adoption thresholds (5 % on a class,
1 % volume-wide) and the built-in dictionary set, and the ADR-5 decision.
ROADMAP and ADR-30 move the benchmark to phase 1 (Spec gaps 18, resolved
by the spec update); M18 does not edit them.

**Existing study (read-only for M18).** A separate file-type dictionary
study for Constellation is already running. Its tooling is
`spikes/dict-filetypes/` (`projects.tsv`: the corpora list; `taxonomy.py`:
the extension map and type families; `fetch.py`: sampling caps, licence
detection, `sources.lock.tsv`, manifest conventions) and it writes to
`bench/dict-study/filetypes/`. M18b reads those files at the start, **reuses
its corpora list and its conventions where they overlap** (project list,
pinned refs, sampling caps, manifest and lock-file format, extension
classes), does not edit them, and **never writes into
`bench/dict-study/filetypes/`**. M18b's outputs go to
`bench/dict-study/lunix/` and `bench/dict-study/constellation-arm/`. M18b
records the commit of `spikes/dict-filetypes/` it used in `PROTOCOL.md` and
in the note, and lists any convention drift from the running study (N13).

---

### M18a — replicix-core: codec id 3, dictionary blob, `replicix-zdict`

**Owns:** the replicix-core checkout at
`/home/bra/cvs/replicix/replicix-core` (its own git repository): in
`crates/chunkfmt` the codec-3 encode/decode, and a new crate
`crates/zdict` (`replicix-zdict`). **Depends on:** M1.B1 only for the
workspace conventions; otherwise independent (wave 1a). **Model:**
design-heavy. **Review:** required, independent (the evaluator is
statistics code: a reviewer with statistics experience reads it with
M18b's checklist). **Root:** no.

**Rules specific to replicix-core work (override the git workflow where
they differ):**

1. Work in a worktree of the replicix-core repository on a branch
   `p1/m18a-zdict`, same commit rules (signed, no trailers, never push).
2. **Constellation depends on this repository.** Changes to
   `replicix-chunkfmt` must be additive and source-compatible
   (`CODEC_ZSTD_DICT = 3` already exists as a constant; the decoder for it
   is new). Run replicix-core's own `cargo test --workspace` and, in the
   Constellation checkout, `cargo check --workspace` with a `[patch]` to the
   branch, run with the `zstd-dict` feature **off and on** (read-only use
   of Constellation; no change there).
3. **Before the branch is merged, the lead announces the change to the
   Constellation maintainers' session** (what is added, which public items,
   the version, the test vectors). M18a produces the announcement text as
   its report's first section; the merge and the announcement are the
   lead's. After the merge, the lead bumps Lunix's `replicix-chunkfmt`
   `rev` in a `deps:` commit.

**Steps:**

1. **Codec 3 in `replicix-chunkfmt`.** Encode and decode of zstd with a
   dictionary under the existing `CCH1` framing, behind a new cargo feature
   `zstd-dict` (default off, like `lz4`): the new `Codec::ZstdDict` variant
   is `#[cfg(feature = "zstd-dict")]`, exactly as `Lz4` is
   `#[cfg(feature = "lz4")]`, because `Codec` is not `#[non_exhaustive]` and
   adding a variant unconditionally would break exhaustive matches in
   Constellation. The dictionary is supplied by the
   caller as a prepared handle (`DictId`, `EncoderDict`, `DecoderDict`
   types), the dictionary id is read from the zstd frame header's `dictID`
   (ADR-30: top bit set = built-in), decompression stays bounded by the
   caller's ceiling (never by `ulen` from the object), and an unknown id
   returns a typed `MissingDictionary(id)` error rather than a panic.
2. **Dictionary blob format** in `replicix-zdict`: `{magic, ver, dictID,
   class, trained_on_hash, bytes}` with encode/decode, a version byte,
   bounded decode (size ≤ 1 MiB, allocation checked), reserved fields zero.
   This is the body inside Lunix's `dicts/<id:08x>` envelope (D6) and is
   defined here, not in `lunix-format`.
3. **Trainer and evaluator** in `replicix-zdict`, generic over a **corpus
   container** (a simple file format: records of `{block_id u64, class u8,
   bytes}`, plus a manifest with input SHA-256; reader and writer):
   - `train(samples, params) -> Dictionary` (ZDICT fastCover; `d`, `k`,
     `steps`, size, seed are parameters and are recorded in the blob);
   - `split_blocked(corpus, k, seed) -> Folds` (k-fold over **blocks**: all
     chunks with the same `block_id` stay in one fold; an assertion makes
     train/test overlap impossible);
   - `evaluate(folds, candidates, levels) -> Report`: per candidate and
     fold, per-chunk stored bytes with and without the dictionary on the
     held-out fold only;
   - `bootstrap(report, resamples = 10_000, seed)`: block bootstrap with
     percentile and BCa 95 % intervals on the volume-wide (or corpus-wide)
     relative reduction, plus the Wilcoxon signed-rank statistic and the
     rank-biserial correlation per chunk-level comparison, plus Holm
     adjustment over a declared family;
   - the **adoption rule** as a function: `adopt(report, class_threshold =
     5 %, global_threshold = 1 %)` returns "meets" only when the **lower**
     95 % bound clears the threshold;
   - everything seeded from one master seed; no global state.
4. **Test vectors** (pinned goldens in `crates/chunkfmt/tests/` and
   `crates/zdict/tests/`): a small fixed dictionary and three encoded
   codec-3 chunks with their decoded bytes; a blob golden; the first
   dictionary produced by M18b (added by a follow-up commit by the lead
   once M18b has one).

**Gate (M18a):**

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace` green in replicix-core, with and
  without `--features zstd-dict`; Constellation still checks with the
  branch patched in, with the feature on as well as off.
- Codec 3 round trip at 4, 16, 64 KiB and 1 MiB chunks; a chunk without
  its dictionary returns `MissingDictionary`; decompression never exceeds
  the ceiling (fuzz-style test, 10 000 mutated frames, counting
  allocator).
- Blob: decode(encode(x)) by proptest; truncation and mutation never
  panic; allocation ≤ 2 × input + 64 KiB.
- **Statistics tests:** (i) on synthetic data with a known true gain, the
  95 % interval covers it in ≥ 93 % of 500 repeated simulations (block
  bootstrap); (ii) a deliberately leaking split (the same block in train
  and test) trips the assertion in `split_blocked`, and the evaluator
  shows in-sample inflation when bypassed in a test-only path; (iii) Holm
  and BCa match reference values computed independently (values in the
  test, computed by a Python script that is committed next to them);
  (iv) the adoption function refuses a candidate whose point estimate
  clears the threshold but whose lower bound does not; (v) the whole
  pipeline is deterministic: the same seed gives byte-identical reports.
- The report's first section is the announcement text for the
  Constellation maintainers.

**Not to touch:** Constellation; `replicix-objcas`, `platform-lite`,
`csi-common`; Lunix.

---

### M18b — the Lunix study harness and the note

**Owns:** `tests/dictstudy/` (collection and analysis scripts, the offline
chunker, `PROTOCOL.md`), `bench/dict-study/lunix/` and
`bench/dict-study/constellation-arm/` (raw summaries), and the research
note `docs/explanation/research/<date>-zstd-dictionaries-on-block-devices.md`
(with its `.pdf` render when `pandoc` exists on the host; SVG figures under
`docs/explanation/research/figures/dict-study/`). No product crate changes.
**Depends on:** M18a for the trainer/evaluator (stages 1-2 may start with
the corpus and capture tooling before M18a merges); stage 3 needs M12a, M13a and M14 (its
validation uses `lunix-fsck`'s walk). **Model:** design-heavy for the method and the analysis; a routine
implementer may collect data. **Review:** required, independent, and
statistical: a reviewer with statistics experience reads the method, the
analysis code and the note before merge, against the checklist at the end.
**Root:** yes for `mkfs`, loop devices and mounts only (no ublk), through
`tools/remote.sh root`, serialised on the host lock; the compression
analysis itself is unprivileged. **Reads:** ADR-30 and ADR-12; DESIGN §5
step 5; `docs/explanation/research/2026-11-11-data-path-hash-compress-encrypt.md`;
`spikes/dict-filetypes/*` (see above); CONVENTIONS "Benchmark hygiene" and
"Research notes". **Stop and report, never retry** a wedged device or mount.

**Downloads and installs (host only, official sources, versions
recorded; the lead approves the list):** the corpora of
`spikes/dict-filetypes/projects.tsv` (fetched by its `fetch.py`
conventions), the latest stable Linux kernel tarball (kernel.org),
PostgreSQL and `pgbench`, MySQL (or MariaDB, stated) with `sysbench`, an
Ubuntu cloud image, several common container images (pulled with `skopeo`
and unpacked with `umoci`, or with rootless `podman --root` under the
scratch directory; installing Docker or containerd on the shared host is a
system change and is not done without the lead's explicit approval) (for example `ubuntu`, `debian`, `alpine`, `python`, `node`,
`postgres`, `nginx`, `golang`), the Silesia corpus, ZFS (`zfsutils`, kernel
module or DKMS), `f2fs-tools`, `btrfs-progs`, `zstd`, `brotli`, `xz`,
`lz4`. bcachefs, NTFS3 and exFAT are included only if the kernel or DKMS
provides them cheaply; otherwise they are listed in the note as "not run:
<reason>".

#### Arm 1 — block devices (the Lunix question)

**Capture, decoupled from Lunix.** The measured population is "the unique
non-zero chunks of the device image after a flush". A loop-backed sparse
image of the same file system, chunked offline, gives the identical set, so
the study does not wait for the daemon:

- One sparse image per file system (`truncate -s 64G`), file systems each
  created with their own compression off: ext4, xfs, btrfs, ZFS (a pool on
  the loop device, `compression=off`, `ashift` and `recordsize` recorded),
  F2FS. **One file system at a time**: the harness runs the workloads on it,
  `sync`, `fstrim` where supported, `umount`, detaches the loop device,
  exports, deletes the image, and only then starts the next file system.
  `tools/remote.sh df-guard` runs before every `mkfs`. Peak use per file
  system ≤ 80 GiB (image data ≤ 40 GiB, export ≤ 25 GiB, working files);
  the harness aborts a workload at 70 % of the image's data budget and
  records it as truncated.
- A small offline chunker (`tests/dictstudy/chunker/`, a standalone cargo
  project, not a workspace member) reads the image, drops all-zero chunks
  (counted as holes), deduplicates by BLAKE3, and writes the unique chunks
  into the corpus container of M18a at 4, 16 and 64 KiB (one pass per size,
  sequentially, deleting each export after its analysis; only summaries and
  the export's SHA-256 are kept). Each chunk gets a `block_id` (the 1 MiB
  extent it came from; 16 MiB in the sensitivity run) and a class tag.
- The class tag comes from an independent labeller: the file system's own
  metadata map (`dumpe2fs`, `xfs_db`, `btrfs inspect-internal`, `zdb`,
  `dump.f2fs`) where available, else the ADR-30 classifier; the labeller's
  agreement on a 1 % hand-checked sample is reported.
- **Share accounting.** Besides the unique chunks the harness records, for
  every run: the zero (hole) chunk count and bytes, the chunks the LZ4
  probe sends to raw (4 KiB probe at 6 %) and their bytes, and the dedup
  ratio, so every result can be stated as **volume-wide stored bytes with
  holes and raw extents included** (see the primary metric).
- **One Lunix validation cell (stage 3, after M12a).** The ext4 image's
  content is written through a real Lunix volume (16 KiB chunks); the
  stored unique non-zero set read back with `lunix-fsck`'s walk must equal
  the offline set at 16 KiB (same chunk-id set, same dedup count). A
  mismatch is a finding that blocks the claim "the offline population is
  the stored population"; it does not block the rest of the study.

**Workloads, each on each file system:**

1. *Metadata-heavy:* millions of empty files and directories in a
   realistic tree (generator with a fixed seed: depth distribution and
   fan-out modelled on a `find` of a Linux source tree and a Debian
   `/usr`), plus a small-files variant (1-4 KiB files).
2. *Reference source corpus:* the latest stable Linux kernel source
   extracted; optionally also built with `defconfig` (objects) as a
   binary-heavy variant.
3. *Databases:* PostgreSQL filled with pgbench (scale stated, about 10-20
   GB); MySQL/InnoDB filled with sysbench OLTP (similar size).
4. *More real-world cases where they fit:* an installed Ubuntu cloud VM
   image; a container image store (several common images pulled with
   `skopeo`/`umoci` or rootless `podman --root` into a store on the file
   system); the Silesia corpus as a
   published reference; an incompressible control (random data plus
   already-compressed media) to verify the LZ4 probe's raw path.

**Pruned measurement matrix (the compute budget).** The review found the
full cross product to be roughly 20 000 dictionary trainings and days of
CPU. The matrix is:

| tier | what | where |
|---|---|---|
| P (primary) | chunk size 16 KiB; codecs none, LZ4 (and the probe's decisions), zstd 1, 3, 6, 9; dictionary 64 KiB trained on a 10 MB sample: one per volume, and per class with 3 classes; 5 folds (10 for small corpora); cross-file-system transfer (train on one file system's data, apply to every other's) | every run |
| S (sweeps) | dictionary size 16, 32, 64, 112 KiB × sample size 1, 10, 100 MB; class counts 2, 3, 4 | ext4 and one database run only |
| C (chunk size) | 4 KiB and 64 KiB views (offline re-chunking for 4 KiB; documented loss of stored alignment) at dictionary 64 KiB, sample 10 MB, per volume and 3 classes | ext4, the pgbench run and the kernel-source run only |
| H (heavy codecs) | zstd 19, xz and brotli as reference points | a **1 GB block-stratified random sample per run**, not the whole export |
| T (timing) | compression and decompression throughput per core (≥ 5 repetitions, pinned, warm) | a 1 GB in-memory sample per file system, not per run |
| R (sensitivity) | the primary comparison repeated with 16 MiB blocks instead of 1 MiB blocks as the resampling and fold unit; reports whether the intervals widen | ext4 and the pgbench run |

`d`, `k` and `steps` of fastCover are tuned **once** on a pilot fold of the
ext4 kernel-source run (training folds only), recorded in `PROTOCOL.md`
and then fixed for every run. `PROTOCOL.md` states the expected CPU: about
1 400 trainings at ≤ 60 s, ≈ 30 CPU-hours of full-export codec passes and
≈ 10 CPU-hours of timing, in total at most 150 CPU-hours at ≤ 6 cores
(host shared with up to four other milestones); the report states the
actuals.

**Primary metric (pre-registered).** *Volume-wide stored bytes*: the sum
over all non-hole chunks of the deduplicated population of the bytes the
policy stores (raw chunks count `ulen`), with holes counted as zero and
reported alongside as a share. The hole share and the raw share are
reported next to every result because ADR-30 says they cap any gain, and
the ADR-30 thresholds are tested on this metric, not on the compressible
subset alone (the compressible-only figure is secondary).

#### Arm 2 — Constellation (whole files and 4 MiB chunks)

A second, separate question for Constellation, run on the **files** of the
same corpora (not the block image; file-system metadata is irrelevant
here):

- Objects: every file under 4 MiB is one object; a larger file is split
  into 4 MiB chunks, each an object.
- Classes: by file extension, using the extension map of
  `spikes/dict-filetypes/taxonomy.py`; the top 20 extensions by bytes are
  classes, everything else is "other".
- Dictionary chosen **by file extension** (the dictionary of the object's
  class); sizes 32, 64 and 112 KiB; compared with zstd 1/3/6/9 without a
  dictionary and with one corpus-wide dictionary.
- Evaluated per extension and corpus-wide, on held-out folds blocked by
  file (all chunks of a file in one fold; for corpora made of many versions
  of the same project, by project).
- Corpora: the project list of `projects.tsv` and the non-image corpora
  (kernel source, Silesia, container image layers extracted to files).
- **Own hypothesis family, own primary metric** (corpus-wide stored bytes
  over the arm's objects, including those stored raw) and own Holm family.

**Shared between the arms:** the container format and the bootstrap
implementation (M18a), the seeds, the single analysis command, the
reviewer checklist and the note. Everything else (population, classes,
hypotheses, primary metric, Holm family) is separate.

#### Method (the defensible part)

1. **Pre-registration.** Before collecting, commit
   `tests/dictstudy/PROTOCOL.md` with, **per arm**, the hypotheses, the
   primary metric, the analysis plan, the stopping rule and the exclusion
   rules. Examples: arm 1: H1 a volume-wide dictionary beats plain zstd-3
   by ≥ 5 % on ≥ 3 file systems at 16 KiB; H2 per-class dictionaries beat a
   single one on database pages; H3 cross-file-system dictionaries lose less
   than 50 % of the in-domain gain; H4 the 16 MiB-block run's intervals do
   not change any H1-H3 conclusion. Arm 2: C1 an extension-keyed dictionary
   beats zstd-3 by ≥ 5 % on ≥ 5 of the top 10 extensions; C2 it beats
   zstd-3 by ≥ 1 % corpus-wide (lower bound); C3 extension-keyed
   dictionaries beat one corpus-wide dictionary. Deviations from the
   protocol are listed in the note, not hidden.
2. **Train/test discipline.** Dictionaries are trained on one sample and
   evaluated only on a disjoint held-out sample, with k-fold
   cross-validation over blocks (M18a's `split_blocked`; the blocking is
   by 1 MiB extent for arm 1 and by file for arm 2). In-sample figures are
   reported only to show the size of the leakage.
3. **Statistics.** Paired comparisons on the same chunks. Wilcoxon
   signed-rank on chunk-level differences for the per-chunk question; a
   **block bootstrap** (10 000 resamples) for the volume-wide (arm 1) or
   corpus-wide (arm 2) figure, with percentile and BCa intervals; effect
   sizes (relative reduction in stored bytes with its interval, the
   rank-biserial correlation), never p-values alone; Holm correction within
   each arm's pre-registered family. A candidate "meets" a threshold only
   if the **lower** 95 % bound clears it. All of this is M18a's code.
4. **Fixed seeds and hashes;** every random choice takes a seed from one
   recorded master seed; every input export has a recorded SHA-256; the
   analysis is one command that regenerates every table and figure from
   the raw summaries, with pinned versions of Python and libraries (a lock
   file) and of `zstd`/`ZDICT` (and of the `zstd` crate).
5. **Timing.** n ≥ 5 repetitions per cell, pinned CPUs, warm-up, the
   `performance` governor if possible, median and spread (min, max, MAD);
   the host's other load recorded; cells with max/min > 1.2 flagged.
   Throughput is measured on in-memory chunks (no I/O).
6. **Per-class breakdown** (file-system metadata against file data against
   database pages, and the classifier's classes; arm 2 per extension),
   reported as its own tables because aggregate gains hide which class
   pays.
7. **Threats to validity** are written out: a single host and kernel; file
   system versions and mkfs defaults; the generator's realism for the
   metadata workloads; the representativeness of the corpora; labeller
   error; the offline 4 KiB re-chunking and the loop-image capture (checked
   by the Lunix validation cell); pgbench/sysbench data being synthetic;
   ZDICT's randomness and parameter choices (tuned once on a pilot fold);
   the bootstrap's assumption on block independence (checked by the
   16 MiB run); multiple comparisons; dictionary maintenance cost not
   measured; the pruned matrix (sweeps on two runs only).

**Output.**

- The research note, in paper form: abstract, introduction, related work
  (zstd dictionaries and ZDICT/fastCover, the Brotli built-in dictionary,
  compression in ZFS, btrfs, F2FS and VDO-style block deduplication, and
  database page compression; cite each by name and year, and mark any
  claim that was not read by the author as UNVERIFIED), method, setup,
  results for both arms (tables, and figures as SVG: ratio against codec
  per file system, gain against dictionary size and against sample size,
  cross-file-system transfer matrix, per-class gains with intervals,
  per-extension gains, throughput against ratio, the 1 MiB against 16 MiB
  interval widths), discussion (what ADR-30's thresholds and the built-in
  dictionary set should be; what the 4/16/64 KiB results imply for ADR-5;
  what the Constellation arm implies for a Constellation dictionary set),
  threats to validity, conclusion, and a reproducibility appendix (exact
  commands, versions, seeds, hashes). A concluding section "Implications
  for ADR-30 and ADR-5" lists proposed changes as proposals; M18b does not
  edit the ADRs.
- Raw summaries in `bench/dict-study/lunix/` and
  `bench/dict-study/constellation-arm/` (CSV/JSON per run, the analysis
  code reference, the environment lock); scratch exports are deleted at the
  end, their hashes kept. The first produced dictionary is handed to the
  lead for M18a's pinned goldens.
- A PDF render (`pandoc` with a pinned template) if pandoc is available on
  the host; otherwise Markdown only, and the note says so.

**Gate (M18b):**

- `PROTOCOL.md` is committed before the first data file; the note lists
  every deviation.
- Every file system in the list either has complete results for all tiers
  that apply to it or an explicit "not run: reason" row (bcachefs, NTFS3,
  exFAT may be absent; ext4, xfs, btrfs, ZFS and F2FS must run, or the
  report says which tool or kernel module was missing and the milestone is
  reported partial).
- No run exceeded its disk budget; `df-guard` was called before every
  `mkfs`; one file system existed at a time; leftovers clean; scratch
  deleted; nothing was written under `bench/dict-study/filetypes/` (a
  `git status` check in the report).
- The Lunix validation cell was run after M12a, or the note says it was not
  and weakens the claim accordingly.
- Re-running the analysis command from the raw summaries reproduces every
  table and figure byte for byte (figures: identical SVG).
- Train/test disjointness is enforced by code (M18a's assertion) and by a
  test that deliberately leaks and shows the in-sample inflation.
- Statistical review passed: the reviewer's findings are resolved or listed
  in the note under "Reviewer findings".

**Reviewer checklist (statistical):** the hypotheses were fixed before the
data, per arm; leakage between folds is impossible by construction; the
unit of resampling matches the dependence structure (blocks, not chunks;
the 16 MiB run is reported); intervals, not just p-values; multiplicity
handled within each arm's family; the dictionary-training parameters were
tuned on training folds only and fixed; the primary metric includes holes
and raw extents; the labeller's error is quantified; the offline population
is validated against Lunix; throughput claims are single-core and warm; the
note's claims stay inside the data; UNVERIFIED marks are honest; the
existing file-type study's directory was not touched.

---

## Spec gaps for the lead

Status as of 2026-11-12 after the independent review
(`wip/01-p1-review.md`) and the maintainer's decisions. "Resolved" means a
decision is recorded in "Decisions already taken" (D-numbers, in M1) and
the spec text has been updated to match (DESIGN.md, DECISIONS.md,
GLOSSARY.md, ROADMAP.md, including the edits of review 2); if the spec
still disagrees when you read it, this plan's rule governs and your report
notes the discrepancy. Review 2 (`wip/01-p1-review-2.md`) resolved N6-N13
below.

| # | gap | status | resolution (where applied) |
|---|---|---|---|
| 1 | control protocol | resolved (D13) | length-prefixed JSON frames; `ctl` in M1.B1 |
| 2 | fanout, interior compression | resolved (D1) | `map_fanout` 256/512 default 512; interior nodes raw via the extent codec; no node codec byte |
| 3 | `CCH1` widths against the KAT vectors | resolved (D2) | stored header keeps CCH1 widths; AAD and `info` use canonical widths |
| 4 | index pack path | resolved (D3) | `index/<pack:016x>`; a FULL names runs durable before its C6 |
| 5 | byte layouts | resolved as a process (D4) | decided in M1.A0 with maintainer sign-off before code; genesis `parent_hash` = BLAKE3 of `volume`, genesis seq 0 (D5) |
| 6 | `heat/`, `gc/`, `gc/space/` bodies | resolved (D6) | envelope and kind id only; `{ver: u16, bytes}`; `dicts/` joins the rule; `gc/` body is defined in phase 1 by `docs/reference/gc.md`: one sealed object per pass, `gc/<pass:016x>`, `{ver: 1, bytes}` (review 2 finding 7) |
| 7 | no GC in phase 1 | **resolved by decision 5 (D15)**: GC is in phase 1 | M17 (local GC and compaction); the 16 B/chunk index budget, 2 GiB `journal_max` and 256 MiB headroom stay as plan defaults in `volume.json`, not the spec |
| 8 | journal lanes | resolved (D7) | lanes, global seq, group flush, floor-based recovery, `epoch` 0 |
| 9 | local file set | resolved (D8) | exact descriptor in `volume`, `volume.json`, keyring path, `state/device` with `boot_id`, registry; GC dirs added |
| 10 | local reads skip id recompute | resolved (D11) | every extent has a tag that local reads verify; `verify_reads` recomputes the id; M16 measures its cost so the maintainer can flip the default |
| 11 | "journal deduplicates by seq" | wording | reissued requests are re-admitted with a new seq; the reissue is idempotent at the block level |
| 12 | CONVENTIONS conflicts | open (lead) | add a "Phase 1 execution" section to CONVENTIONS that points at this plan's rules (commits in worktrees, per-milestone `CARGO_TARGET_DIR`, remote host); this plan overrides until then |
| 13 | queue count against host | resolved | M16 runs Part A at 4 and 8 queues and reports both with the CPU split; judged at the better of the two with the oversubscription stated; the memory gate is measured at 4 queues (M12) |
| 14 | power-loss emulation | kept as a stretch in M15 | DESIGN §21 puts `dm-flakey` runs in phase 8 CI |
| 15 | mlocked buffers | resolved (D14) | `max_io_buf_bytes` attach option; M12's memory gate is "mlocked buffers + 150 MiB" at 4 queues |
| 16 | `on_hang` watchdog | out of phase 1 | noted for ROADMAP phase 8 |
| 17 | "xfstests passes" | resolved | no test fails that passes on a `loop` reference in the same session; the `-g quick` exclusion list is committed and justified per test |
| 18 | ROADMAP/ADR-30/ADR-5 text | resolved by the spec update | the dictionary benchmark moves to phase 1 (M18); the trainer and codec 3 live in replicix-core (M18a); ADR-5 is fed by M16 and M18b |

New gaps found by the review and the decisions:

| # | gap | status |
|---|---|---|
| N1 | `index_packs` lag (what a FULL names) | resolved (D3); M11 order test |
| N2 | orphan segments adopted by dedup after a crash | resolved: open-time sweep of unnamed sealed files above the tip's high-water mark runs before `catch_up`; `catch_up` takes the segment ids named by the chain; M17 reclaims the rest (M6, M7, M11) |
| N3 | plain-extent integrity | resolved (D11) |
| N4 | recovery floor; `MidRingCorruption` | resolved: `recover(hint, floor)`; no hard error; records beyond the first gap are ignored and reported (`ignored_after_gap`); fsck warns |
| N5 | commit seq against write seq; `chain_floor` | resolved (D5): genesis `commit_seq` 0, `commit_seq = parent_seq + 1`; floor is the older retained FULL (default N = 2), so it moves |
| N6 | GC defaults: `gc_grace` 60 s with a floor of 30 s (10 s + 2 × `read_timeout`, `read_timeout` default 10 s in `volume.json`), `gc_rate` 64 MiB/s, live-fraction threshold 0.5, step budget 50 ms, `retain_last` 2 | **resolved** (review 2): adopted with `read_timeout` defined (the old 30 s read timeout would have put the floor above the grace default); revisit after M17.B (M17 rule 11) |
| N7 | liveness by chunk id with index fallback for stale hints; `id_len = 0` volumes use hint-only liveness and refuse compaction | **resolved** (review 2): accepted, with the delete-time index invariant (M17 rule 5; M14 checks it, including after `index/` is rebuilt; DESIGN §11 states it) |
| N8 | map packs are deleted whole only; a pack with one live node survives, so dead share can accumulate | **resolved** (review 2): whole-pack deletion accepted; trigger fixed: if M17.B measures the dead share of surviving packs above 50 % of pack bytes, open a follow-up (re-emit the live nodes of packs under 25 % live at the next seal), otherwise defer to phase 4 |
| N9 | `gc/` evidence body and the local `state/gc/condemned` list are defined in phase 1 by `docs/reference/gc.md` | **resolved** (review 2 finding 7): `gc/` is one sealed create-only object per pass, `gc/<pass:016x>`, body `{ver: 1, bytes}`, written at pass end, so phase 2 uploads it verbatim by construction; the condemned list is local-only and never uploaded |
| N10 | compaction re-seals with a fresh salt, so every compacted byte costs a decrypt/encrypt (or a copy on plain volumes) | informational; M17.B reports write amplification |
| N11 | journal batch packing cost: n 4 KiB writes take ceil(n·4160/4096) blocks and an isolated QD1 4 KiB write costs 8 KiB | **resolved** in DESIGN §4.1 (D12); the stale ADR-4 sentence ("in one record padded to a 4 KiB multiple") is deleted (review 2 finding 4); M3's perf gate compares with the spike's group commit at batch ≥ 32 |
| N12 | the extent tag covers every byte before the tag (`CCH1 header ‖ chunk_id ‖ clen ‖ payload`, one contiguous slice) | **resolved** by the lead on 2026-11-12 (review 2 finding 1): D11, M2 step 2 and freeze item 3 say so; DESIGN §6 and GLOSSARY already did |
| N13 | the file-type dictionary study (`spikes/dict-filetypes/`, `bench/dict-study/filetypes/`) is still running; M18b reuses its conventions | **resolved as a process** (review 2): M18b records the commit of `spikes/dict-filetypes/` it used and lists any convention drift in the note |
| N14 | which volume modes exist; the default; whether `plain` is offered (M1.A0 review, open question) | **resolved** (maintainer, 2026-11-12, D16): `encrypted`, `integrity` (default, key-file slot beside a local volume) and `plain` (supported everywhere, with warnings); ADR-11 amended |
| N15 | index runs are unauthenticated, so a forged dedup hit could drop a write (review finding 9) | **resolved** (D17): hits from a run are confirmed against the extent's full `chunk_id` and tag before the map points at them (M6, M9; ADR-6, ADR-9) |
| N16 | `created_at` as a chain rule (review finding 6) | **resolved** (D17): a writer clamp, never a reader rejection (DESIGN §8.2; M11) |
| N17 | journal CRC failure below the floor hides later records (review finding 8) | **resolved** (D17): reader resync in 8-byte steps (DESIGN §4.1, ADR-4; M3, M14) |
| N18 | checkpoint tails after `--truncate-tip`; ObjId reuse after the orphan sweep (review finding 10) | **resolved** (D17): tails used only if `last_commit_seq ≤` tip, truncate-tip zeroes the checkpoint, allocator rule (DESIGN §4.4; M3, M11, M14) |
| N19 | thin clones cannot share interior map nodes (review finding 13) | **resolved** (D17): `lineage_idx` reserved in interior child entries (DESIGN §7, ADR-25, ROADMAP freeze list; M1.A freeze item 5) |
| N20 | integrity-mode safety rests on rules outside `lunix-crypto` (M2 review finding 2) | **resolved** (D20, cross-cutting rule 13): R1-R4 are normative in DESIGN §6, §7, §8.4, §10.1, §15 and ADR-11, and have gates in M5, M7, M9, M11, M12 and M14 |
| N21 | the `id_len = 16` collision residual in integrity mode (M2 review finding 8) | **resolved** (D18, maintainer): keyed chunk ids in `integrity` mode; `plain` keeps the residual and promises nothing against a bucket writer |
| N22 | the rollback anchor (DESIGN §22.1, M2 review attack 8) | **resolved** (D19, ADR-31): local floor in `state/pins` v2; phase 5 adds the mirror maximum and a Kubernetes floor; `state/pins` v2 and D18's purpose id need the maintainer's re-sign of `FREEZE.md` |
| N23 | salt and nonce source, hostile keyring cost, key-file open flags (M2 review findings 1, 3, 4) | **resolved** as spec statements (D20, cross-cutting rule 3, ADR-11 amendment, DESIGN §15); the code fixes are M2's, tracked in `wip/01-p1-m2-review.md` (findings 1-7) |

## Out of scope

- Any S3 or backend code, `doctor`, leases (the lease and CONFIG/TRANSFER
  bodies are format only), the keeper and RAM sink, eviction, readahead,
  retention other than `last:N` and pins (the pin sets are read, never
  written), map-pack compaction, snapshots, shrink, mirrors, CSI,
  `lunix upgrade`, packaging and udev rules.
- FIPS sealing (the profile is accepted in descriptors and refused at
  seal time), KMS slots (accepted by the decoder, refused by the
  implementation), the external cryptographer review (recommended before
  release, not a phase 1 gate).
- `USER_COPY` and `BATCH_IO` data paths beyond what `lunix-ublk` exposes
  as opt-in flags (the default is copy, tested; opt-in modes are
  smoke-tested only).
- Fuzz harnesses (phase 8); the decoders' negative and allocation tests
  stand in for them until then.

## Report format

Follow CONVENTIONS "Reporting back" and, in addition, each milestone's
report (`docs/plans/v1/reports/p1-<m>.md`, also the final message) has:

1. **Header:** milestone, branch, final commit id, base commit id, date,
   the host's kernel and CPU count (no host name), tool versions, whether
   root was used.
2. **Gates:** a table of every gate item: the command that reproduces it
   (through `tools/remote.sh`), the rule restated, the measured value,
   pass or fail. Performance rows carry the reference value measured in
   the same session.
3. **Decisions taken** where this plan was silent or ambiguous (the lead
   confirms them), each with the reason.
4. **Requests to the lead** (changes wanted in `lunix-format`,
   `lunix-iface` or dependencies, with the exact diff), and defects found
   in other crates (with a failing test).
5. **Spec gaps found** (new ones beyond the list above).
6. **Skipped or not run**, each with the reason, and the leftovers-check
   output for any root session.
7. **Deferred** items, one line of justification each.
