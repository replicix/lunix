# M15 prep report: phase 1 exit tests (fio verify, xfstests, crash loops, ratios)

- Milestone M15, **preparation only**: branch `p1/m15`, base `321e9dc`, 2026-11-16. The gates were not run (no Lunix
  device, no root run of Lunix). Host: kernel 7.0.0, 16 CPUs, 30 GiB, fio 3.41, e2fsprogs 1.47.2, xfsprogs 6.18.0, root
  file system ext4; this work used CPUs 12-15 only.
- Everything below is in `tests/exit/` and `bench/phase1/exit/` (the two paths M15 owns). Nothing else was touched.

## What is ready

| item | file | state |
|---|---|---|
| shared harness (config, daemon start/stop/recover, `lunix` CLI wrappers, fsck verdict, guest fsck, fio helpers, teardown per W1-W9) | `tests/exit/lib.sh`, `tests/exit/crashlib.sh` | written, shellcheck clean, run end to end against a **fake** `lunix`/`lunixd` (see "Checked how") |
| 1. fio verify matrix | `tests/exit/fio-verify.sh`, `bench/phase1/exit/fio/verify-{raw,fs}.fio`, `tests/exit/trimcheck.py` | job files and `trimcheck.py` run **for real against a loop device** (raw and ext4); the script runs against the fake |
| 2. xfstests | `tests/exit/xfstests.sh`, `tests/exit/xfstests/{local.config.in,exclude.*,expected-fail.*}`, `bench/phase1/exit/xfstests-compare.py` | built xfstests on the host; script and compare tool run **for real on loop devices** (reference) |
| 3a/3c. kill -9 loops and QUIESCE handover (SIGUSR2) | `tests/exit/crash-random.sh` | against the fake |
| 3b. every crash point, three hits | `tests/exit/crashpoints.sh` | against the fake |
| 3 negative controls | `tests/exit/negctl.sh` | M10 controls via `run-crash-loop.sh`; full-stack controls against the fake |
| 4. ratios | `tests/exit/ratios.sh`, `bench/phase1/exit/fio/ratio.fio`, `bench/phase1/exit/chunkstat.py` | against the fake; the data generator was checked offline |
| 5. result format and summary | `results.jsonl` per script, `bench/phase1/exit/summarize.py` | run on a fixture; host header from `hostinfo` in `lib.sh` |
| self-tests | `tests/exit/selftest.sh` (no root), `selftest-root.sh` (loop device), `selftest-fake-root.sh` + `fakes/fake-lunix.sh` | all three ran green on the host |

### Design in one paragraph per script

- **`lib.sh`** follows the M12a harness but drives the `lunix` CLI (M13a, merged) instead of raw `lunixd ctl` JSON, so the CLI
  is exercised too. A cell is `cell_begin` (daemon stopped, state and volume directories removed, daemon started, volume
  created with `--chunk`, `--id-len 16`, `--compress auto`, attached) and `cell_end` (detach, `lunix fsck --deep`,
  store size). "Clean" for fsck is exit 0, or exit 1 whose warnings are all in `ORPHAN_CODES` (`orphan crash_leftover
  leftover`); any error or other warning fails. Every device the daemon announces is recorded in `created-devices`; teardown
  never deletes a device without `lunix-ublk-testd del --force` and only when its daemon is gone (W1a), and never signals a
  stopping daemon.
- **`fio-verify.sh`**: cell = (target raw/ext4/xfs, chunk 16 K/64 K, mode integrity/encrypted) on a 32 GiB volume. Six
  profiles (QD 1/32/128 x 1 and 8 jobs), each a 4 GiB job group (plan: at most 12 GiB) with `bssplit` 4k-1M, crc32c
  verify, `verify_state_save`, a `psync` companion job that mixes trim into its own region (`trim_percentage`,
  `trim_verify_zero`) and `trimcheck.py` doing BLKDISCARD and BLKZEROOUT (`blkdiscard -z`) or `fallocate` punch/zero-range
  rounds with an O_DIRECT read-back, all for `RUNTIME` = 600 s. Then umount, guest fsck, detach, deep fsck, attach,
  `fio --verify_only` of each profile from its saved state (checked on a loop device that it reads one pass and writes
  nothing), detach, deep fsck again.
- **`xfstests.sh`**: `TARGET=lunix|loop`, `FS`, `CHUNK`; two volumes (or two loop devices) of 16 GiB, `local.config`
  generated from the committed template (mounts under `$LUNIX_SPIKE_DIR/mnt`, `nosuid,nodev`, results and `TMPDIR` under the
  run directory), `./check -g quick -x dangerous -E <exclude>` under an outer cap. `xfstests-compare.py` classifies per file
  system: passes on both, fails on both (reason from the reference's `.out.bad`), **fails only on Lunix (defect)**, passes on
  the reference but did not run or pass on Lunix, and checks both runs completed. Exit status 1 on any defect or incomplete run.
- **`crash-random.sh`**: `MODE=kill` runs the phases `immediate`, `delay1s`, `delay10s`, `double` (a second kill inside the
  recovery window), ext4 100 cycles and xfs 50 (`PHASES` rescales), kills at random 5-20 s; `MODE=quiesce` is the 20-cycle
  SIGUSR2 handover (the daemon is never killed; a daemon that does not exit in 60 s is a wedge). Load: fio verify with
  backlog + two `fsyncer.py` + the 1 ms `heartbeat.py` (the M10 helpers, used in place). After every cycle the load must
  still be alive; per phase: fio errors, guest fsck, acknowledged-fsync verify, `lunix fsck --deep` after the detach;
  stall times from `stall.py` plus the daemon's own `stall_ms` log values (p50/p99/max in `summarize.py`).
- **`crashpoints.sh`**: reads `lunixd --list-crashpoints` of the crashpoints build (`LUNIXD_CP`), a counting run
  (`LUNIX_CRASH_COUNT`) gives executions per point, then per point hits {1, (count+1)/2, count} on ext4 (and xfs for
  `engine.c*`). A crash during create/attach/mkfs is handled (the supervisor restarts the daemon in recovery mode and the step
  is retried). Each run: 120 s of the churn workload (fio on 256 MiB x 4 so blocks are overwritten, `lunix gc` every 15 s,
  `commit_interval_ms` 2000), 30 s after the crash, cap 600 s; a run that never reaches its hit is `unreached` (a `last`
  run is repeated once at 90 % of the count). Total budget `BUDGET_S` (default 72000 s); slices with `POINTS_FROM/TO`.
- **`negctl.sh`**: N1 reruns the two M10 controls (`CONTROL=noreissue`, `CONTROL=fault`) through `run-crash-loop.sh`; N2
  corrupts one 4 KiB block of a verified region of a raw Lunix volume and requires `verify_only` to fail; N3 raises the
  acknowledged index of `fsyncer.py` and requires the verify to fail. See open question 5.
- **`ratios.sh`**: per chunk size, fio `bs` = chunk, compress 0/50/90 x dedupe 0/50/90 (4 GiB each, always
  `refill_buffers`: without it fio repeats a few buffers and the data is not what the knobs say, which was checked offline),
  a random-data cell, Linux tarball extraction on ext4 (`TARBALL=`), a root file system copy (`ROOTFS_SRC`, default `/usr`,
  read-only `rsync -aHx`, capped at 12 GiB). Per cell `lunix status --json` (attached and detached), the deep fsck recount and
  `chunkstat.py` (the dedup and zlib compression of the data actually on the device, the yardstick beside the nominal
  1/(1-p)). `summarize.py` applies the gate: dedup within 10 % of 1/(1-p), compression within 15 % of 1/(1-c), random data
  stored at most 1.01x; the table is printed whichever side the data falls on.

## Checked how

| check | result |
|---|---|
| `bash -n` and `shellcheck -S warning` (shellcheck 0.11 here) on every script | clean |
| `tests/exit/selftest.sh` on the host (syntax, `fio --parse-only` of all job files with the exported variables, `summarize.py` on a fixture) | PASS |
| `selftest-root.sh` on a loop device (tmpfs-backed): verify job with trim companion, `verify_only` from the saved state (reads one pass, writes no sector), `trimcheck.py` raw (6 700 rounds) and file (6 400 rounds), ext4, guest fsck | PASS |
| xfstests built on the host, mini run `generic/001 002 003 005 006 007 011 013` on loop for ext4 and xfs through `xfstests.sh`; `xfstests-compare.py` on the two result directories | ran; `generic/003` fails on the ext4 loop reference (a real "fails on both" example); compare output as designed |
| `selftest-fake-root.sh`: fio-verify (2 targets x 2 profiles), crash-random (kill, double, SIGUSR2), crash points (counting run, three hits), ratios, negctl N2/N3 against `fakes/fake-lunix.sh` (loop devices instead of ublk) | see the last section; script bugs found and fixed this way: `$((..))` with leading zeros, `fio` label with `/`, an `expect` that never returned failure, a fio TERM seen as an error, an fsyncer run starved of space by a too small fake volume |

What this proves is the control flow of the scripts. It proves nothing about Lunix, and the fake cannot show a hang of the
real device (a daemon killed with I/O in flight leaves requests held by the kernel; the scripts' waits and `wedged` calls
for that path were written from the M12a scripts and are untested).

## What each gate run needs

| need | detail |
|---|---|
| binaries (built by the lead) | `lunixd` release (`LUNIXD`), `lunix` CLI (`LUNIX`), `lunix-ublk-testd` (`TESTD`, only for `del --force` and the M10 controls), `lunixd` built with `--features crashpoints` in a separate target dir (`LUNIXD_CP`); optional `lunix-fsck` (`LUNIX_FSCK`) until `lunix fsck` (M13b) is merged |
| host tools present | fio 3.41, jq, python3, e2fsprogs, xfsprogs, rsync, losetup, `perf` not needed here |
| xfstests | built in-tree in the M15 directory on the host (the `xfstests` subdirectory) (git.kernel.org `xfstests-dev`, commit `d07dca72` of 2026-10-15, `make`, not installed); `XFSTESTS_DIR=<abs>` |
| packages installed on the host for the xfstests build | autoconf automake libtool libtool-bin libaio-dev libacl1-dev libattr1-dev uuid-dev xfslibs-dev libcap-dev attr acl quota gettext libgdbm-dev libgdbm-compat-dev indent liburing-dev xfsdump (`apt-get install --no-install-recommends`; some were already present). No users were created: tests that need the `fsgqa` users report "not run" on both targets |
| inputs the lead provides | `TARBALL` (a Linux source tarball), optionally `ROOTFS_SRC` (default `/usr` of the host) |
| root runs | all through `tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs> LUNIXD=... LUNIX=... TESTD=... bash tests/exit/<script>`; each script prints its cap and the `LUNIX_ROOT_TIMEOUT` it needs (cap + 600 s) |
| CPUs | daemon `DAEMON_CPUS` (default 12-13), fio `FIO_CPUS` (14-15), `QUEUES` (default 2); on a free host use 4 queues, `journal_max` 512 MiB is the default of the harness |

### Expected runtime and disk (estimates, none measured on Lunix)

| run | command | runtime | peak disk |
|---|---|---|---|
| fio verify, full matrix (12 cells x 6 profiles) | `fio-verify.sh` (slice with `CHUNKS`, `MODES`, `TARGETS`) | about 14 h (6 x 12 min + fscks per cell); four slices of about 3.5 h | one 32 GiB volume at a time; the random fio data does not compress, so the store reaches about 27 GiB plus 0.5 GiB journal; df-guard asks 40 before each mkfs |
| xfstests | 4 Lunix runs + 2 loop runs, `-g quick` | 1-3 h each, about 12 h | two 16 GiB volumes per run, store below 20 GiB |
| random kill loop | `crash-random.sh` FS=ext4 / FS=xfs | ext4 about 45 min, xfs about 25 min | 32 GiB volume, store below 15 GiB |
| QUIESCE x20 | `MODE=quiesce` | about 10 min | same |
| crash points | `crashpoints.sh` | 5-10 min per run; N points x 3 runs (+ xfs for `engine.c*`): about 20 h at 80 points; slices | 32 GiB volume per run, store small (a few GiB) |
| negative controls | `negctl.sh` | about 15 min | 8 GiB volume + the M10 controls' 4 GiB images |
| ratios | `ratios.sh` | about 2.5 h | one 32 GiB volume, store below 10 GiB |

The plan's ceiling for M15 is 100 GiB of host disk at any time; the harness never has more than one volume or one xfstests
pair, so the peak is about 45 GiB.

## Open questions and findings for the lead

1. **Host disk blocks every gate run today.** `df` shows 121 GiB free and `LUNIX_MIN_FREE_GIB` is 120, so `df-guard` refuses
   every `mkfs`, including the 2 GiB the self-test asked for (the self-tests therefore ran on a tmpfs directory with
   `LUNIX_MIN_FREE_GIB=4`, which cannot be done for the gates). A gate run needs at least 160 GiB free (45 GiB peak + 120);
   free space or lower the floor in the brief (the plan says the two move together).
2. **`lunixd` never calls `lunix_iface::crashpoint::dump_counts`** (only `lunix-ublk-testd` does, `main.rs:600`). Without it
   `LUNIX_CRASH_COUNT` produces nothing, so "middle" and "last reached" cannot be taken from a counting run. `crashpoints.sh`
   falls back to fixed hits (`HITS_FALLBACK="1 25 250"`) and says so in the report; a one-line call on the daemon's quiesce
   exit path (in M12a's tree) restores the plan's definition. M15 may not edit `lunixd`.
3. **The plan says "delete the volume between cells" but the CLI has no `volume delete`.** The harness stops the daemon
   and removes the state and volume directories. If the maintainer wants a CLI path, that is a request for M13a.
4. **Gaps in other crates the scripts rely on, to verify when the stack is merged:** `lunix gc` (the daemon answers "no
   collector" until M17.B is wired; `crashpoints.sh` calls it in a best-effort loop and `chunksize` in M16 needs it), `lunix
   fsck NAME --dir --deep --json` (M13b; `LUNIX_FSCK` is the stand-in), the key `verify_reads` in `volume.json` (used by M16),
   the log line "volume recovered at start ... recovered=true stall_ms=" (taken from `daemon.rs`), `volume list --json`
   fields `name`, `dir`, `device` (taken from `VolumeSummary`).
5. **Negative controls on the full stack.** The M10 controls (no REISSUE, drop all writes) are switches of the test daemon;
   `lunixd` has none. `negctl.sh` reruns the M10 ones through `lunix-ublk-testd` and adds two harness-sensitivity controls on
   the full stack (corrupted verified block, inflated fsyncer acknowledgement). A true full-stack control that loses
   acknowledged writes needs a file-armed fault hook in `lunix-ublk` like `LUNIX_UBLK_FAULT_HOLD` (for example "drop the
   next FUA write"); request for the owner of `lunix-ublk`.
6. **`-g quick` expectations.** `expected-fail.*` and `exclude.*` are empty on purpose: the plan's definition of the gate
   reads "fails on both" from a reference run of the same session, and the reference run on the gate disk is the first thing
   the gate run does (`TARGET=loop`). Only after that run should a test that hangs or damages the host be excluded, with its
   reason. A real `-g quick` reference run was not possible now (open question 1). On loop, `generic/003` failed in the mini run.
7. **Encrypted and unencrypted.** "Unencrypted" is `--integrity` (the default). `--plain` is not covered; add it to `MODES` if
   wanted (the scripts accept `plain`).
8. **Queue count.** `QUEUES` defaults to 2 here because the test host leaves 4 CPUs; the plan allows up to 4 queues for M15
   (`journal_max` 512 MiB). On a free host set `QUEUES=4`, `DAEMON_CPUS` and `FIO_CPUS` accordingly.
9. **Root file system copy.** The default `ROOTFS_SRC=/usr` of the test host is read-only and size-capped; if the lead prefers
   a debootstrap or a distribution rootfs tarball, set `ROOTFS_SRC` to a directory (a tarball must be unpacked outside the
   volume first).
10. **`ublk-leftovers.sh` and other agents.** It reports any `fio` whose command line mentions `ublk`, `loop` or the scratch
    directory; the self-tests on loop devices passed it, but a run that starts while another agent's `fio` mentions `loop` in
    its command line would be refused.
11. **Duplicated library.** `tests/perf/lib.sh` (M16) is a copy of `tests/exit/lib.sh`; the two milestones own separate paths.
    After both merge the lead may move the common part to one place.

## Self-test results of the fake run

`selftest-fake-root.sh` against the fake `lunix` and `lunixd` on a tmpfs scratch directory, final state of the head commit:
fio verify (raw and ext4, profiles 8:1 and 8:2, 8 s), random kill loop (immediate and double phases), QUIESCE handover (3
cycles by SIGUSR2), crash points (counting run, then first, middle and last hit of two points, `engine.commit_x` correctly
reported `unreached` because the fake counts it as zero), ratios (four fio cells and the random cell), negative controls N2 and N3: **all PASS**, leftovers check clean after
each. (Reruns of single scripts are `ONLY="points crash" bash tests/exit/selftest-fake-root.sh`.)

## Commits

See `git log p1/m15`; both prep branches are pushed and not merged. `tools/check-ownership.sh` reports only that `m15` may
not change `docs/plans/v1/reports/p1-m15-prep.md` (the table allows `p1-m15.md`); the file name is the one the brief asked for.
