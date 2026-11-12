# Conventions for plan-executing models

You are executing one implementation plan for **Lunix**, a Rust ublk
block-device daemon (`lunixd`) that stores deduplicated, compressed,
optionally encrypted chunks on a local file system and on S3 backends. The
repo root is `/home/bra/cvs/replicix/lunix`; this file is
`docs/plans/v1/CONVENTIONS.md`. Read it fully before starting any plan.
Every plan file assumes you did.

## Ground rules

1. **Do NOT `git commit`, `git push`, or rewrite history.** Leave all
   changes in the working tree. A coordinator reviews and commits.
2. **Do not start a plan on a dirty tree** unless the plan says the
   in-tree changes are its subject. If `git status` shows unrelated
   changes, stop and report.
3. Work until **all gates the plan names are green**. Do not report
   success with failing tests or skipped-because-broken checks. If you are
   stuck after several distinct attempts, stop and write an honest report
   of what works, what does not, and your hypotheses.
4. Never weaken an existing test, scenario or gate threshold to make it
   pass unless the plan explicitly authorizes it. Fixing a test's bug is
   fine; loosening its assertion is not. Spike decision rules are fixed in
   advance in the plan: do not change a threshold after seeing the data.
   If you think a rule was wrong, report the data and your argument and
   leave the rule as written.
5. `docs/explanation/DESIGN.md` is the spec. Do not edit it to match an
   implementation detail or an accident. If you find a genuine
   contradiction between the spec and reality, implement what the plan says
   and record the contradiction in your report. A change to a design
   decision is made by the maintainer, recorded as an ADR in
   `docs/explanation/DECISIONS.md` (see "Recording decisions"), and
   DESIGN.md is then updated in the same commit that makes it true,
   linking the ADR.
6. Do not hardcode credentials. S3 credentials come from the environment or
   an AWS profile only; a step that needs a provider without credentials is
   skipped and reported as skipped.

## Orientation (read in this order, skim where obvious)

- `docs/explanation/GOALS.md`, then `docs/explanation/DESIGN.md` — the
  spec. Each plan names its sections.
- `docs/explanation/DECISIONS.md` — the ADRs. All are *proposed* until the
  maintainer or a gate accepts them.
- `docs/explanation/GLOSSARY.md` — terms (chunk, extent, segment, cut,
  commit kinds).
- `docs/plans/v1/ROADMAP.md` — phases and exit criteria.
- `docs/explanation/research/` — dated notes; the plan names the ones that
  matter. Notes are evidence, not spec, and several contain values marked
  UNVERIFIED.
- Constellation (`/home/bra/cvs/replicix/constellation`, read-only for
  Lunix plans unless the plan says otherwise) is the source of the reusable
  S3/CAS/chunk-format code; see
  `docs/explanation/research/2026-11-11-constellation-reuse.md`.

## Build & test commands

The Rust workspace is created in phase 0/1. Until a plan creates it,
spikes are standalone cargo projects under `spikes/<name>/`. Once a
workspace exists at the repo root:

```bash
unset CARGO_TARGET_DIR                  # first thing in every shell
cargo build --workspace
cargo build --release --workspace       # benchmarks always use release
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`unset CARGO_TARGET_DIR` matters because some sandboxes inject it and
scripts then cannot find binaries under `target/`. Standalone spike
projects run the same four commands inside their own directory. Pin the
toolchain in `rust-toolchain.toml` (copy Constellation's channel) and pin
exact versions (`=x.y.z`) of `libublk`, `io-uring` and `object_store` in
spikes, with a comment saying why.

Tests that need root, a loaded `ublk_drv`, or S3 are `#[ignore]` or live in
scripts under `tests/` or `spikes/*/run.sh`; they must detect a missing
prerequisite and SKIP loudly (exit code 77 in scripts) instead of failing.
A SKIP is acceptable on a host that lacks the prerequisite and never when
the plan says that prerequisite is mandatory.

## Safety rules for ublk work on a dev host

ublk spikes create block devices and load a kernel module. Prefer a
disposable VM with a recent kernel (6.19+ or 7.0); on the dev box (Linux 7.0, 32 CPUs) follow these rules
exactly.

1. **Privileges.** Needs root (or CAP_SYS_ADMIN + `/dev/ublk-control`
   access) and `modprobe ublk_drv`. If `sudo` is unavailable
   non-interactively, stop and ask. Do not change system-wide sysctls
   beyond what the step names, and revert any you change
   (`kernel.io_uring_disabled`, CPU governor) before finishing.
2. **Never point a test at a real disk.** Backing stores are regular files
   on a scratch file system, `null_blk`, `zloop`/loop devices over such
   files, or `brd`. Never write to `/dev/sd*`, `/dev/nvme*`, `/dev/dm-*`
   or any device you did not create in this session. Every script
   hardcodes a scratch directory (default `/var/tmp/lunix-spike`, or the
   directory in `$LUNIX_SPIKE_DIR`) and refuses to `mkfs`, `mount` or
   `dd` anything whose path does not resolve under it or to a device
   listed in the script's own `created-devices` file.
3. **Mounts.** Mount only under `$LUNIX_SPIKE_DIR/mnt/<name>`, always with
   `nosuid,nodev`. Unmount before deleting a device.
4. **Cleanup is part of every script.** Use `trap cleanup EXIT INT TERM`:
   stop fio, `umount`, then delete the ublk device (the spike's own
   `del` subcommand, `lunix detach` once it exists, or ublksrv's
   `ublk del -n <id>` if installed), `losetup -d`, `rmmod null_blk` only if the script
   loaded it. Record created devices in `created-devices` as they appear.
5. **Leftover detection.** Run `bash spikes/common/ublk-leftovers.sh`
   (create it in P0.1) at the start and end of every session. It lists
   `/dev/ublkb*` and `/dev/ublkc*`, `/sys/class/ublk-char/*`, loop devices
   backed by files under the scratch dir, mounts under the scratch dir, and
   running `fio`/spike processes, and exits non-zero if anything exists
   that is not in the current run's `created-devices`. A device stuck in
   `QUIESCED` after your daemon died is a leftover: delete it (the `del`
   subcommand with the device id, which must also work on devices in
   recovering state) and
   confirm `/sys/class/ublk-char` is empty.
6. **Hangs.** A killed daemon with I/O in flight leaves processes in D
   state until the device is deleted or recovered. Always run fio with
   `--timeout`/`--runtime` and a watchdog (`timeout -k`), and run the
   recovery spikes with the `FAIL_IO`-style watchdog the step describes.
   If the box becomes unresponsive, that is a finding; report it and do not
   retry destructively.
7. **No memory-reclaim self-deadlock.** Never put the daemon's own
   backing files, its log, or swap on the ublk device it serves. Spikes
   `mlockall` and call `prctl(PR_SET_IO_FLUSHER)` when the step says so.
8. **Resource limits.** Keep scratch usage below the size the plan states
   (check `df`), and delete large sparse files at the end.

## Benchmark hygiene

Every number that feeds a decision comes with its context.

- Record in each `RESULTS.md`: CPU model and core count, kernel
  (`uname -r`), distro, memory, storage devices and file systems used,
  governor (`/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor`),
  SMT/turbo state, `io_uring_disabled`, `libublk`/`io-uring`/`fio`
  versions, git revision of the spike (or "uncommitted"), and the date.
- Release builds only; `RUSTFLAGS="-C target-cpu=native"` only if stated.
- Set the `performance` governor if you can and restore it afterwards; if
  you cannot, say so next to the numbers.
- Pin: fio jobs and the daemon queue threads on disjoint CPU sets
  (`taskset`/`--cpus_allowed`), same NUMA node where applicable. State the
  sets.
- The exact fio command line (or job file, committed next to the results)
  accompanies every table. Use `--direct=1`, `--ioengine=io_uring`,
  `--time_based --ramp_time=5 --runtime=30` unless the step differs, and
  `--output-format=json` saved under `bench/<name>/raw/`.
- Repeat each cell at least 3 times; report median and min/max. Discard no
  run silently; list noisy cells (max/min > 1.2).
- CPU cost: record daemon CPU seconds (`/proc/<pid>/stat` utime+stime) per
  I/O or as CPU% per core alongside IOPS.
- Compare like with like: same fio job, same queue depth, same device size.
  A kernel-reference cell (`null_blk`, loop) is run in the same session.
- Raw JSON and scripts are committed; `RESULTS.md` holds the tables and the
  decision made from them.

## Recording decisions

Gates in a plan end in a decision. Record it in
`docs/explanation/DECISIONS.md`:

- Append a dated **Gate outcome** block under the ADR the gate belongs to
  (do not rewrite the original text): `Status: accepted | amended |
  rejected (proposed → …)`, the evidence (links to `bench/<name>/RESULTS.md`
  and notes under `docs/explanation/research/`), the rule from the plan that
  was applied and which side of it the data fell on.
- If the outcome is "amended", add the new decision text as an
  `ADR-N amendment` block and list the DESIGN.md sections that must change.
  Do not edit DESIGN.md yourself unless the plan says so; the maintainer
  confirms first. Gate outcomes that merely confirm an ADR are marked
  *accepted (gate)*.
- ADRs marked "needs the maintainer's decision" (ADR-19) are never changed
  to accepted by an executing model; follow the plan's conditional branch.
- New findings that need a decision but have no ADR get a new numbered ADR
  marked *proposed*.
- Research notes (`docs/explanation/research/YYYY-MM-DD-<slug>.md`) have a
  question, method, results, sources and an "Open questions / UNVERIFIED"
  section. Mark anything you did not measure or read yourself.

## Code style

- **Rust guidelines:** every agent that writes, reviews or refactors Rust
  loads the `rust-skills` skill first (installed for all Claude accounts,
  from github.com/leonardomso/rust-skills at commit `fd2a861`) and follows
  its rules. Where a rule conflicts with this file, the plan or the spec,
  those win. Reviews cite rule ids (for example `err-no-unwrap-prod`).
- Rust 2021, stable toolchain. Errors: `thiserror` in libraries, `anyhow`
  with context in binaries. Comments explain intent and trade-offs, never
  narrate code.
- Queue threads never block and never run tokio (ADR-18). Spike code
  that models the final architecture follows the same rule; throwaway
  measurement code may take shortcuts but says so in its module comment.
- Files copied from Constellation keep their MPL-2.0 headers; new files in
  shared crates carry the same header.
- Spike code is allowed to be rough but must build warning-free,
  `cargo fmt`-clean, and reproduce its results from a single documented
  command in the spike's `README` or `run.sh`.

## Reporting back

End with a concise report: files added and changed; the exact commands that
reproduce each headline result; the result tables (or the path and the one
line summary); for each gate the rule, the measured values, and the
outcome; design decisions where the plan was ambiguous; steps skipped with
the reason (missing privilege, credentials, kernel feature); leftovers
check output at the end of the session; anything deliberately deferred with
a one-line justification each.
