# Plan 00 — Phase 0: spikes, simulations, prototypes and gates

Read `docs/plans/v1/CONVENTIONS.md` first (ground rules, ublk safety
rules, benchmark hygiene, decision recording, report format). Spec
context: `docs/explanation/DESIGN.md` §3 (ublk device), §4 (local store),
§7 (block map), §9 (S3 client, `doctor`), §10 (cache), §20 (performance
targets); `docs/explanation/DECISIONS.md` ADR-2, 4, 5, 8, 11, 14, 17, 18,
19; `docs/plans/v1/ROADMAP.md` phase 0.

Research notes this plan draws on (evidence, not spec; values marked
UNVERIFIED there are what this plan verifies):

- `docs/explanation/research/2026-11-11-ublk-io-uring-rust.md`, §6
  (pitfalls), §9 "Recommendations" item 9 (the spikes) and "Open questions".
- `docs/explanation/research/2026-11-11-data-path-hash-compress-encrypt.md`
  (§1 chunk size, §5 crypto, nonce management).
- `docs/explanation/research/2026-11-11-cache-consistency-s3-protocol.md`
  (cache policy section, simulation open question).
- `docs/explanation/research/2026-11-11-constellation-reuse.md`
  (appendix table: crate extraction).

**Depends on:** nothing. This plan produces no product code; it resolves the
ADR gates *before* any format freezes (ROADMAP: from phase 1 on the journal
record, segment/`CCH1` framing, map node, commit kinds, bucket layout and
key hierarchy are final).

## Goal

Replace the proposed status of the gated ADRs with measured outcomes, and
leave a reproducible baseline:

| gate | ADR | milestone |
|---|---|---|
| libublk is an adequate base; framework overhead known | 18 | P0.1 |
| copy vs `USER_COPY` vs `BATCH_IO` | 17 | P0.2 |
| `USER_RECOVERY`+`REISSUE`+`QUIESCE` keeps a mounted fs consistent; feature table | 17 | P0.3 |
| `UPDATE_SIZE`; FLUSH/FUA visibility | 17, 20 | P0.4 |
| journal commit strategy (group `fdatasync` vs linked `WRITE→FSYNC`) | 4 | P0.5 |
| chunk size 64 KiB; cache policy S3-FIFO | 5, 14 | P0.6 |
| radix Merkle map cost | 8 | P0.7 |
| crypto design review | 11 | P0.8 |
| `replicix-core` extraction (conditional) | 19 | P0.9 |
| conditional-write support and latency per provider | 2 | P0.10 |
| memfd write buffer survives daemon restart via a keeper; lost keeper fails safe | 26 | P0.12 |
| record outcomes, publish baseline | all | P0.11 |

## Status (2026-11-12)

Phase 0 is closed out except for the items under "Remaining". The
outcomes are in `docs/explanation/DECISIONS.md` (the dated Gate outcome
blocks) and the numbers in `bench/BASELINE.md`. The plan stays in `wip/`
until the coordinator reviews it.

| milestone | status | reason |
|---|---|---|
| P0.1 ublk baseline | done | ADR-18 accepted (gate); the loop comparison is inconclusive (disk-bound) |
| P0.2 copy, `USER_COPY`, `BATCH_IO` | done | reduced matrices; rules not met, copy stays the default |
| P0.3 recovery and feature table | done | ADR-16 and ADR-17 accepted (gate) with amendments; `BATCH_IO` with QUIESCE failed |
| P0.4 `UPDATE_SIZE`, FLUSH/FUA | done | ADR-20 accepted (gate); the daemon validates sizes |
| P0.5 journal | done | ADR-4 accepted (gate); group commit; one run per cell |
| P0.6 trace simulation | not run | no trace could be obtained without a licence acceptance; simulators built and tested |
| P0.7 radix map | done | ADR-8 accepted (gate) with amendments |
| P0.8 crypto review | partial | review done; six format-changing findings; ADR-11 stays proposed |
| P0.9 `replicix-core` | done outside this plan | commit `caa47b4`; Constellation `6e25bad6`; ADR-19 accepted |
| P0.10 S3 probe | partial | AWS S3 Standard, OVH and MinIO done; R2, GCS, B2 and others have no credentials |
| P0.11 close-out | done | this status, `bench/BASELINE.md` and the ADR blocks |
| P0.12 keeper-held RAM buffer | done | ADR-26 accepted (gate) for the keeper; the systemd FD-store variant is not run |

**Remaining:**
- **P0.6 traces.** A person accepts the SNIA licence and downloads MSR
  (and Tencent CBS, or Alibaba through its survey) onto the test host,
  then runs `spikes/tracesim/run.sh`. That decides ADR-5 and ADR-14.
- **P0.10.** R2, GCS and B2 credentials; S3 Express One Zone; the lane
  test with a shaped 200 MB/s bulk upload and enough samples for p99;
  PUT latency per size with 8 parallel streams.
- **P0.12.** The systemd FD-store variant (`sd_notify(FDSTORE=1)` and a
  `LISTEN_FDS` recovery path).
- **Crypto amendment.** The maintainer decides F2, F5, F6, F7, F9 and F10
  (ADR-11) before phase 1 freezes the extent, commit and bucket formats.
  The external reviewer's answers are also outstanding.

## Privileges, hosts and scratch space

- **Needs root** for P0.1–P0.5 (`modprobe ublk_drv`, `/dev/ublk-control`,
  `mkfs`, `mount`, loop devices, `null_blk`, `sysctl`/governor). P0.6–P0.8,
  P0.9 and P0.10 run unprivileged (P0.10 may start a MinIO container if
  docker is available). Prefer a **disposable VM** (kernel 7.0 or 6.19+,
  ≥ 8 vCPUs, ≥ 32 GiB RAM, ≥ 100 GiB scratch on a fast disk). On the dev
  box (Linux 7.0.0-31-generic, 32 CPUs; `ublk_drv`, `null_blk`, `zloop`,
  `brd` are modules; `/dev/ublk-control` does not exist until `ublk_drv` is
  loaded; `CONFIG_BLKDEV_UBLK_LEGACY_OPCODES` is not set) obey every rule in
  CONVENTIONS "Safety rules". If non-interactive root is unavailable,
  complete the unprivileged milestones, write the privileged scripts and
  report P0.1–P0.5 as "scripts ready, not run".
- Scratch directory `$LUNIX_SPIKE_DIR` (default `/var/tmp/lunix-spike`),
  at least 60 GiB free for P0.3/P0.5 images; P0.6 traces need their own
  directory `$LUNIX_TRACE_DIR` (default `/var/tmp/lunix-traces`) with
  ≥ 150 GiB free if the full Alibaba/Tencent sets are fetched (see P0.6;
  a stated subset is acceptable).
- The file system under the scratch dir for ublk backing files must be
  ext4 or xfs on a real disk (not tmpfs; not the ublk device under test).

## Layout produced by this plan

```
spikes/common/            hostinfo.sh, ublk-leftovers.sh, fio-matrix.sh, lib.sh
spikes/ublk-baseline/     P0.1 cargo project (libublk null + loop targets, subcommands add|del|list|features)
spikes/ublk-datapath/     P0.2 (copy / USER_COPY / BATCH_IO targets that touch every byte)
spikes/ublk-recovery/     P0.3, P0.4 (file-backed recoverable target, run.sh drivers)
spikes/journal-bench/     P0.5
spikes/tracesim/          P0.6 (chunk-size and cache-policy simulators)
spikes/radixmap/          P0.7
spikes/crypto-kat/        P0.8 (known-answer + nonce-uniqueness tests)
spikes/s3probe/           P0.10
bench/<name>/RESULTS.md   one per milestone, plus raw/ (fio JSON, CSV)
bench/BASELINE.md         P0.11
```

Every spike is a standalone cargo project (`[workspace]` stanza in its own
`Cargo.toml` so it does not need a root workspace), builds with
`cargo build --release`, has a `README.md` of ≤ 30 lines (purpose, one
command to reproduce, prerequisites), and pins its dependencies exactly.
Do **not** create a root `Cargo.toml` workspace here; phase 1 does.

Dependency pins (check crates.io when you start and record the date and the
versions you actually used in each `RESULTS.md`): `libublk =0.4.8`
(latest stable on 2026-11-11, released 2026-08-29; ADR-18 pins 0.4.x
until 0.5.0 is evaluated), `io-uring 0.7.x`, `blake3 1`, `lz4_flex` or
`lz4`, `zstd 0.13`, `aws-lc-rs`, `object_store 0.14` (Constellation's
version), `clap`, `hdrhistogram`. System tools: `fio ≥ 3.36`,
`e2fsprogs`, `xfsprogs`, `btrfs-progs`, `util-linux`, `jq`, `python3`.

## Order and time boxes

Suggested order: P0.1 → P0.2 → P0.3 → P0.4 (privileged, share a harness),
then P0.5; P0.6, P0.7, P0.8 and P0.10 are independent of each other and of
the ublk work and may be interleaved while long benchmark runs execute;
P0.9 is conditional; P0.11 is last. Time boxes: P0.1–P0.4 one day each,
P0.5 half a day, P0.6 two days (trace handling dominates), P0.7 one day,
P0.8 half a day, P0.9 one day, P0.10 half a day. A box overrun is reported,
not hidden; do not silently drop a cell of a matrix to stay inside it —
mark it "not run" with a reason.

---

## Common setup (do first; part of P0.1's deliverables)

1. `spikes/common/hostinfo.sh` prints the CONVENTIONS "Benchmark hygiene"
   header as Markdown: CPU model/cores/SMT, NUMA layout, kernel and distro,
   memory, block devices and the file system of `$LUNIX_SPIKE_DIR`,
   governor, `io_uring_disabled`, tool versions, and whether
   `/boot/config-$(uname -r)` has `CONFIG_BLK_DEV_UBLK`,
   `CONFIG_BLKDEV_UBLK_LEGACY_OPCODES`, `CONFIG_BLK_DEV_NULL_BLK`.
2. `spikes/common/ublk-leftovers.sh` implements CONVENTIONS safety rule 5.
   Exit 0 only if clean. Run at the start and end of every session.
3. `spikes/common/lib.sh`: `require_root`, `skip <reason>` (exit 77),
   `register_device <path>` (append to `$RUN_DIR/created-devices`),
   `safe_path <p>` (fails unless `p` resolves under `$LUNIX_SPIKE_DIR` or is
   listed in `created-devices`), and a standard `cleanup` trap.
4. `spikes/common/fio-matrix.sh <device> <outdir> <matrix-file>`: runs the
   cells of a matrix (one line per cell: `bs rw iodepth numjobs`), 3
   repetitions each, saves `fio --output-format=json` per run, writes a
   `summary.csv` (median/min/max IOPS, bandwidth, clat p50/p99/p99.9, fio
   `usr_cpu`/`sys_cpu`), and prefixes every cell with `safe_path`. The
   daemon's CPU time is sampled by the caller via `--daemon-pid`.
5. Verify the prerequisites and write the verdicts into
   `bench/ublk-baseline/RESULTS.md` §Host: `modprobe ublk_drv` works,
   `/dev/ublk-control` appears, `sysctl kernel.io_uring_disabled` is 0
   (or 1 with the group), `modprobe null_blk` works.

---

## P0.1 — ublk baseline: libublk null and loop vs `null_blk` and `loop`

**Goal:** measure the framework's own cost so every later number has a
reference, and confirm ADR-18's choice of libublk.

**Steps:**

1. `spikes/ublk-baseline`: a binary `ublk-spike` using `libublk =0.4.8`
   with subcommands:
   - `add --target null|loop --backing <file> --queues N --depth D
     [--cpus list] [--flags …]` creating a device with
     `CMD_IOCTL_ENCODE`, 4096-byte logical/physical blocks, `max_sectors`
     for 1 MiB, `attrs = VOLATILE_CACHE | FUA`, one pinned thread per
     queue, no worker pool (this measures the framework);
   - `del --id N`, `list`, and `features` (used by P0.3).
   The `loop` target uses io_uring on a backing regular file opened
   `O_DIRECT` and answers FLUSH with `fdatasync`; the `null` target
   completes every request immediately (reads return whatever is in the
   buffer — document that this is a throughput probe only).
   Base both on libublk's `examples/null.rs` and `examples/loop.rs`; keep
   their logic and say so in the README.
2. Kernel references, created per run and registered for cleanup:
   - `null_blk`: `modprobe null_blk nr_devices=1 gb=64 bs=4096
     submit_queues=8 queue_mode=2 irqmode=0 completion_nsec=0` (add a
     `memory_backed=1` variant for data-bearing comparisons);
   - `loop`: `losetup --direct-io=on -f --show <file>` on a 32 GiB
     preallocated file under `$LUNIX_SPIKE_DIR`.
3. Device settings for ublk targets: 8 queues × depth 256 (so QD 128 with 8
   fio jobs never starves a queue), queue threads pinned to CPUs 0–7 of one
   NUMA node, fio jobs on CPUs 8–15 of the same node. Also run a 1-queue ×
   depth 128 configuration for the QD1/latency rows. Disable no
   mitigations; leave the CPU frequency policy as documented in hygiene.
4. fio matrix (`spikes/common/matrices/baseline.txt`): bs ∈ {4k, 64k, 1M} ×
   rw ∈ {randread, randwrite, read, write} × iodepth ∈ {1, 32, 128} ×
   numjobs ∈ {1, 8}; 72 cells; `--runtime=20 --ramp_time=3`, 3 repetitions,
   `--direct=1 --ioengine=io_uring --norandommap --randrepeat=0
   --group_reporting`, device size 64 GiB (writes to ublk-null are free;
   writes to loop hit the file, so size the loop test file to 32 GiB and
   use `--size=32g`). The full 72 cells run for the `null_blk` and
   `ublk-null` pair. For `loop` and `ublk-loop` run the reduced set: bs ∈
   {4k, 64k} × rw ∈ {randread, randwrite, write} × iodepth ∈ {1, 32, 128} ×
   numjobs = 8 (iodepth 1 also at numjobs 1); the backing file is
   preallocated and pre-written once so reads hit allocated extents.
5. Record per cell: IOPS, MB/s, clat p50/p99/p99.9, fio CPU%, daemon CPU
   seconds per million I/Os (queue threads only), plus the ratio to the
   kernel reference.
6. Check DESIGN §20's 4K random-read target (≥ 500K IOPS p99 ≤ 150 µs at 8
   queues on cache hits): report the `ublk-null` figure as its ceiling.
7. Also record (small cells, for later reference): `ublk-null` with 1 queue
   at QD1, 4k randread: p50/p99 latency vs `null_blk` (the per-I/O
   framework overhead in µs).

**Deliverables:** `spikes/ublk-baseline/`, `spikes/common/`,
`bench/ublk-baseline/{RESULTS.md,raw/}`.

**Gate (ADR-18, fixed in advance):** libublk is adopted if, at 4k randread,
numjobs 8, QD 128, `ublk-null` reaches ≥ 1.0 M IOPS **and** ≥ 50 % of
`null_blk`, **and** at QD 1 the added median latency over `null_blk` is
≤ 15 µs. If it fails, determine whether the loss is libublk (compare with
the kernel selftest server `kublk`, built from `tools/testing/selftests/ublk`
if kernel sources are available, else with `ublksrv`'s `ublk` null target)
or the host; report both, and record ADR-18 as "amended: drop to
`io-uring` crate + libublk `sys` bindings" only if `kublk`/the raw ring
reaches the thresholds and libublk does not. Also state the versions
tested and whether 0.5.0 exists on crates.io yet (do not evaluate it here
unless it is published).

---

## P0.2 — copy path vs `UBLK_F_USER_COPY` vs `BATCH_IO` (ADR-17)

**Goal:** decide the default data-transfer mode for Lunix's final
architecture, where the daemon must read every written byte (hash) and
produce every read byte.

**Prerequisite probe:** run `features` (P0.1) and record the kernel's
`GET_FEATURES` mask. If `BATCH_IO` is absent, run the other two variants and
mark BATCH_IO "unavailable on kernel X"; if `USER_COPY` is absent, ADR-17's
"used when present" is moot, and report that. If libublk 0.4.8 does not
expose `BATCH_IO`, check `examples/batch.rs` upstream; if it still cannot be
driven, use libublk's `sys` bindings for the BATCH commands in
`spikes/ublk-datapath` and say how much code that took.

**Steps:**

1. `spikes/ublk-datapath`: one binary, three data modes selected by flag,
   same target logic, so only the transfer mechanism differs:
   - `--mode copy` (default per-I/O buffers, kernel copies in task_work);
   - `--mode user-copy` (`UBLK_F_USER_COPY`; the server `pread`/`pwrite`s
     `/dev/ublkcN` at the per-tag offset, issued from the queue thread, or
     from a worker thread in `--workers N` mode);
   - `--mode batch` (`UBLK_F_BATCH_IO`, with the batch size and the
     worker-completes-any-tag capability, if the interface offers it).
2. Target logic (identical across modes), selected by `--work`:
   - `none`: complete immediately (isolates the transfer path);
   - `hash`: BLAKE3-hash the payload of every WRITE (so USER_COPY must read
     it) and, for READ, fill the buffer with a deterministic pattern from
     the LBA (so the server writes every byte);
   - `hash+lz4`: additionally LZ4-compress each 64 KiB chunk of a write
     into a throwaway buffer. Pattern data compresses ~2×; report the
     entropy used.
   With `--workers N`, `hash` and `lz4` run on a bounded OS-thread pool and
   the completion returns to the queue thread (mode `copy`/`user-copy`) or
   is committed from the worker (`batch` if it supports that); reproduce the
   Lunix ADR-18 shape.
3. Matrix: bs ∈ {4k, 64k} (add 1M for the `none` work) × rw ∈ {randread,
   randwrite, write} × iodepth ∈ {1, 32, 128} × numjobs ∈ {1, 8}; queues 8
   (and queues 1 for the QD1 rows), pinned as in P0.1; 3 repetitions.
   Per cell record IOPS, p50/p99/p99.9, and daemon CPU µs per I/O.
   Repeat the headline cells (64k write, work=hash, QD32, nj 8; 4k randwrite
   QD1) with `--workers 0` and `--workers 4`.
4. Record memory: resident set and `max_io_buf_bytes × queue_depth × queues`
   for each mode.

**Deliverables:** `spikes/ublk-datapath/`, `bench/ublk-datapath/RESULTS.md`
(tables per work-mode; a one-screen summary table of the headline cells
with ratios to `copy`).

**Decision rule for ADR-17 (fixed in advance):** using `--work hash` with
`--workers 0` and the 8-queue configuration:

- **USER_COPY** becomes the default if, at bs 64k writes and QD 32/128, it
  gives ≥ 10 % more IOPS **or** ≥ 10 % less daemon CPU per I/O than copy,
  **and** at bs 4k randwrite QD 1 its p50 latency is ≤ copy's + 2 µs, **and**
  no cell regresses by more than 5 % IOPS. Otherwise copy is the default
  and USER_COPY stays "used only if the spike shows it wins", i.e. unused.
- **BATCH_IO** becomes the default (with copy or USER_COPY, whichever won)
  if, at QD 128 and numjobs 8 (4k and 64k), it gives ≥ 10 % more IOPS **or**
  ≥ 10 % less CPU per I/O, **and** QD1 p50 is ≤ +2 µs and p99 ≤ +10 % vs the
  non-batch mode, **and** P0.3's recovery cycle passes with BATCH_IO
  enabled (the recovery milestone re-runs its core test in this mode). If
  not, BATCH_IO is "used when present" only for deployments that opt in via
  config, and the compat path stays the tested default.
- If `BATCH_IO` and `USER_COPY` cannot be combined with the worker-pool
  completion model, report which constraint (queue-thread-only completion)
  applies; ADR-18's thread model may need an amendment.

---

## P0.3 — Recovery and the feature-probe table

**Goal:** show that a `kill -9` of the daemon under load, with a mounted
file system, loses and corrupts nothing, and measure the stall; produce the
host's feature table for ADR-17.

**Steps:**

1. `spikes/ublk-recovery`: a **recoverable file-backed target** — the
   minimum that preserves a mounted fs across daemon death. Backing store:
   a preallocated 16 GiB file opened `O_DIRECT` (state lives in the file,
   which survives the process); WRITE → pwrite, READ → pread, FLUSH →
   `fdatasync`, FUA → `RWF_DSYNC`. Flags:
   `CMD_IOCTL_ENCODE | USER_RECOVERY | USER_RECOVERY_REISSUE | QUIESCE |
   UPDATE_SIZE` (+ `NO_AUTO_PART_SCAN`, `SAFE_STOP_DEV` when GET_FEATURES
   reports them). Startup logic is ADR-16's: if the device id exists and is
   quiesced (`START_USER_RECOVERY` → re-fetch → `END_USER_RECOVERY`), else
   `ADD_DEV`. On recovery verify what the driver retained: record whether
   `SET_PARAMS` must be re-sent (open question in the ublk note),
   `dev_info` fields, and the queue affinity.
   Subcommands: `run --dev-id N --backing F [--recover]`, `del`, `features`.
2. **Feature probe:** `features` issues `UBLK_U_CMD_GET_FEATURES`, decodes
   each bit of `UBLK_F_*` known to the 7.0 header
   (`/usr/src/linux-headers-*/include/uapi/linux/ublk_cmd.h`), and prints a
   Markdown table `flag | bit | set on this kernel | in ADR-17's mandatory /
   used-when-present / never lists | note`. Add rows that need a live
   test: attempt `ADD_DEV` with `NO_AUTO_PART_SCAN` and with `SAFE_STOP_DEV`
   and report accepted/rejected; verify with a GPT-labelled backing file
   that `NO_AUTO_PART_SCAN` suppresses `/dev/ublkbNp1` until `partx -a`;
   verify `SAFE_STOP_DEV` semantics by `STOP_DEV` while a file system is
   mounted (it must refuse or defer rather than yank the device) and report
   the observed behaviour. If a kernel git tree is available locally,
   fill "first kernel release" with `git tag --contains <commit>`; else
   leave that column UNVERIFIED (ADR-17's floor of 6.16 stays "provisional").
3. **Crash loop driver** `run-crash-loop.sh` (root, scratch only):
   1. start the daemon, `mkfs.ext4` the device (default features), mount at
      `$LUNIX_SPIKE_DIR/mnt/rec` with `nosuid,nodev`;
   2. start fio on a file in the mount: `--rw=randrw --rwmixwrite=70
      --bs=4k,16k,64k mix via bssplit --ioengine=io_uring --iodepth=32
      --numjobs=4 --direct=1 --verify=crc32c --verify_backlog=64
      --verify_fatal=1 --do_verify=1 --time_based --runtime=300 --size=4g`
      and a second fsync-heavy job (`--fsync=1`, 4k writes, 2 jobs) so
      acknowledged-durable writes are exercised;
   3. at random intervals of 5–20 s, `kill -9` the daemon; a supervisor
      restarts it in recovery mode immediately; also run phases with a
      supervisor delay of 1 s and 10 s (I/O queues during QUIESCE), and
      phases with two consecutive kills inside the recovery window;
   4. record, per cycle, the stall time: fio's worst clat during the
      window plus a 1 ms heartbeat writer's max inter-completion gap; also
      the daemon's time from `exec` to `END_USER_RECOVERY` complete;
   5. after the run: stop fio (it must have finished with zero errors and
      zero verify failures), `umount`, `fsck.ext4 -fn` (must be clean),
      then remount and re-verify with `fio --verify_only` reading the
      state saved with `--verify_state_save=1`. Repeat the whole sequence
      for **xfs** (`xfs_repair -n`) as a second file system with 10
      cycles.
4. **QUIESCE path** (planned handover, `lunixd upgrade` model): run the
   same workload; replace the daemon by `QUIESCE_DEV` → start new process →
   recover, 20 times, no `kill`. Report stall times next to the kill -9
   ones. If the hung-device watchdog (ublk note §9 item 6) is cheap to
   try, test switching a stuck device to `FAIL_IO` and confirm fio sees
   EIO instead of a hang; report the observed behaviour.
5. **BATCH_IO recheck:** if P0.2 selected or is considering BATCH_IO, rerun
   steps 3 (20 cycles) and 4 with it enabled; this satisfies the P0.2
   precondition.
6. Negative controls (must be run, they validate the harness): (a) recover
   with `USER_RECOVERY` but **without** `REISSUE`: expect EIO to fio and a
   non-clean outcome or a fs error; (b) a daemon that drops the last write
   before the kill (fault toggle) must make the verify step fail. If a
   control passes cleanly, the harness is not testing what it claims; fix
   it before trusting the main results.

**Deliverables:** `spikes/ublk-recovery/`, `bench/ublk-recovery/RESULTS.md`
containing the feature table, per-phase stall statistics (n, p50, p99, max)
and the checks run.

**Gate (ADR-17, fixed in advance):** pass iff, with copy mode (and BATCH_IO
if enabled for the re-check), **all** of: 30 consecutive kill -9 cycles on
ext4 and 10 on xfs with zero fio errors and zero verify failures; fsck clean
after every phase; no daemon-death stall longer than 30 s at a 1 s restart
delay (report p99 regardless); both negative controls fail as expected.
Mandatory flags (`CMD_IOCTL_ENCODE`, `USER_RECOVERY`, `QUIESCE`,
`UPDATE_SIZE`) all present on this kernel. Failure ⇒ ADR-17 becomes
"amended" with the observed constraint (for example "REISSUE unusable on
kernel X; use `FAIL_IO` plus a remount procedure") and the phase-1
recovery design must be revisited before proceeding.

---

## P0.4 — `UPDATE_SIZE` online grow; FLUSH/FUA visibility

**Goal:** confirm ADR-20 (online grow) and ADR-17's parameter choices
(`attrs = VOLATILE_CACHE | FUA`) work end to end.

**Steps (reuse the recoverable target from P0.3, plus a `resize`
subcommand issuing `UPDATE_SIZE`):**

1. **Grow:** backing file 16 GiB → 64 GiB (sparse extension), mounted ext4
   with fio running (`--rw=randwrite --bs=4k --iodepth=16 --numjobs=2
   --verify=crc32c`): `UPDATE_SIZE`, confirm `/sys/block/ublkbN/size` and
   `blockdev --getsize64` changed without a remount, `resize2fs` online to
   the new size, fio finishes without errors, `fsck.ext4 -fn` clean after
   unmount, and capacity reported by `df`. Repeat with xfs (`xfs_growfs`)
   and btrfs (`btrfs filesystem resize max`). Repeat once while a
   recovery cycle is happening (grow immediately after a `kill -9` +
   recovery) and once growing to exactly the current size (no-op) and to a
   non-multiple of the chunk size (e.g. 64 GiB + 4 KiB·3) to confirm the
   kernel accepts it. Try an `UPDATE_SIZE` that shrinks and report the
   kernel's behaviour (ADR-20 says shrink is an explicit, separate
   operation; this only records what the driver does).
2. **Write-cache visibility:** read
   `/sys/block/ublkbN/queue/{write_cache,fua,rotational,discard_granularity,
   max_sectors_kb,max_hw_sectors_kb,logical_block_size,physical_block_size,
   optimal_io_size}` after `ADD_DEV`; expected `write_cache = write back`,
   `fua = 1`. Confirm that dropping `VOLATILE_CACHE` yields `write through`
   (control).
3. **FLUSH counting:** the target counts REQ_OP_FLUSH, FUA writes, and plain
   writes (atomic counters, printed on SIGUSR1). Run, on ext4, xfs and btrfs
   with default `mkfs` and default mount options, a workload of N=10 000
   iterations of `pwrite 4k; fsync` (and `fdatasync`; and `O_SYNC`
   writes; and `O_DSYNC`) using a small C-free helper (fio
   `--fsync=1` / `--fdatasync=1` or `--sync=1`), single job, and report
   FLUSH per fsync, FUA per fsync, writes per fsync. Repeat with 8 jobs
   fsyncing concurrently to show the kernel's own flush coalescing
   (FLUSH per fsync should drop; record it). Also count what `mkfs`, `mount`
   and `umount` issue.
4. Record the numbers as inputs to DESIGN §8 (flush-group coalescing) and
   §4 (journal sync points); no ADR change unless a rule below fires.

**Deliverables:** `bench/ublk-recovery/RESULTS.md` §P0.4 (or
`bench/ublk-resize-flush/RESULTS.md`), updated target.

**Gate (fixed in advance):** online grow works on ext4 and xfs with no I/O
errors and clean fsck, and the sysfs attributes show `write back` and
`fua 1`. A failure of either amends ADR-17 (params) or ADR-20 (grow via
`UPDATE_SIZE`). FLUSH-per-fsync figures are informational; flag, in the
report, any fs where a single `fsync` issues more than 3 FLUSH commands.

---

## P0.5 — Journal microbenchmark (ADR-4)

**Goal:** choose the journal's durability primitive and group-commit shape
(DESIGN §4: journal appends are `O_DIRECT`, io_uring, one `fdatasync` per
sync point).

**Steps:**

1. `spikes/journal-bench` (Rust, `io-uring 0.7.x`, one thread, registered
   file and fixed buffers): append records of size R ∈ {512 B (rounded to
   4 KiB sectors by padding, as the real journal would), 4 KiB, 16 KiB, 64
   KiB, 256 KiB} to a **preallocated** (`fallocate`, zero-filled so
   unwritten-extent conversion is not measured) 8 GiB file and, as a
   control, a non-preallocated growing file; `O_DIRECT | O_DSYNC` is a
   third mode for reference.
2. Strategies:
   - **A. group commit:** submit up to B appends (`WRITE_FIXED`), wait for
     their CQEs, then one `FSYNC` (with `IORING_FSYNC_DATASYNC`) covering
     the group; B ∈ {1, 4, 16, 64}, plus a time-window variant
     (collect for W ∈ {0, 50, 200} µs);
   - **B. linked:** `WRITE_FIXED` linked (`IOSQE_IO_LINK`) to `FSYNC`
     (datasync) for each group, one chain per group, up to 4 chains in
     flight;
   - **C. per-write `RWF_DSYNC`** (`WRITE_FIXED` with the flag) for each
     record, B = 1 reference;
   - **D. two files** (data/metadata split) — skip unless A/B show a
     metadata-journal bottleneck.
3. Workloads: open-loop offered load of 10 K, 50 K, 200 K records/s and a
   closed-loop saturation run; measure per-record durable-latency
   (submit → fsync CQE), per-flush latency, records per flush, throughput
   in MB/s, CPU of the thread. Also the effect of a concurrent 64 KiB
   sequential background read on the same device (the destage path reads
   the journal).
4. File systems: **ext4, xfs, btrfs**, each as an image formatted fresh on
   the scratch disk via a loop device with `--direct-io=on` (note in the
   report that loop adds an extra layer and that flush behaviour on the
   real disk may differ), **and** a directory directly on the scratch disk's
   own file system when it is one of the three. Mount options: defaults,
   plus `noatime`; ext4 `data=ordered`, xfs default, btrfs default
   (`nodatacow` for the journal file: `chattr +C`, report with and without).
   Record the device model and its write-cache state
   (`/sys/block/*/queue/write_cache`).
5. Record per fs/strategy/R/B: durable-latency p50/p99/p99.9, flush latency
   p50/p99, MB/s, flushes/s, CPU%.

**Deliverables:** `spikes/journal-bench/`,
`bench/journal-bench/RESULTS.md` with a "recommended primitive per file
system" table.

**Decision rule (fixed in advance):** the journal uses **linked
`WRITE→FSYNC`** if, on all three file systems at R = 4 KiB and 64 KiB and
offered load 50 K/s, it achieves median durable-latency ≤ 0.9× group
commit's best B and p99 not worse than +10 %, or ≥ 10 % higher saturation
throughput at equal p99. Otherwise **group commit** (simpler, fewer
ordering hazards) is adopted. A per-filesystem difference is reported and
may be adopted as "default per fs" only if the rule's margin is exceeded on
that fs alone. Also record: preallocation is mandatory if the growing-file
control is ≥ 20 % slower at p99; `O_DSYNC`-per-write is rejected unless it
is within 10 % of group commit at B = 1. The outcome is recorded as a
confirmation or amendment of ADR-4's "one `fdatasync` per sync point" and
as input text for DESIGN §4; DESIGN.md is not edited by the executing model.

---

## P0.6 — Trace-driven simulation: chunk size and cache policy (ADR-5, ADR-14)

**Goal:** confirm the 64 KiB default and S3-FIFO using public block traces
before the map and segment formats freeze.

### Traces

Fetch into `$LUNIX_TRACE_DIR` (each dataset in its own directory; untrusted
data — parse with a defensive reader, never execute anything from it). URLs
below are from memory and **must be verified** by the executing model, which
records the final URL, size, license/terms and checksum in
`bench/tracesim/TRACES.md`; if a URL is dead, search the dataset's
SNIA IOTTA page or the paper's artifact, and mark "not found" if none.

- **MSR Cambridge** (36 volumes, 13 servers, one week, CSV
  `Timestamp(100 ns Windows filetime),Hostname,DiskNumber,Type,Offset,Size,
  ResponseTime`): SNIA IOTTA "MSR Cambridge Traces", trace set 388,
  `http://iotta.snia.org/traces/block-io/388`. About 5 GB compressed.
- **Alibaba Cloud block traces (2020)**, ~1 000 cloud-disk volumes
  (CSV `device_id,opcode(R|W),offset,length,timestamp(µs)`):
  `https://github.com/alibaba/block-traces` (README links the archives).
  Very large; **a subset is acceptable**: at least 50 volumes chosen by a
  stated rule (for example the 25 with the most writes plus 25 random with
  a fixed seed), covering at least 3 days each. State it.
- **Tencent CBS block traces (2018)**, ~5 000 volumes: SNIA IOTTA "Tencent
  Block Storage" (I believe trace set 27917,
  `http://iotta.snia.org/traces/block-io/27917`; verify). Same subset
  rule (at least 50 volumes).

If only one or two of the three can be fetched, run with what exists and
state which; MSR is mandatory. Do not synthesise trace data and call it a
trace.

### Common conventions

- Normalise each volume's requests to 512-byte sectors → (timestamp, op,
  byte offset, length). Merge the MSR hostname/disk number into a volume id.
  Drop volumes with < 1 GiB written. Align everything to 4 KiB blocks.
  Trace timestamps drive the commit intervals; use the trace clock, not
  wall time.
- Reads are included for the cache study and for cold-RMW decisions; writes
  drive the rest.
- A write of a byte range touches chunks `[off/C, (off+len-1)/C]`; a chunk
  is **partially** written if the range does not cover it fully.
- Report each metric **per volume** and then as a distribution across
  volumes (p10/p50/p90/mean) per trace set; also the aggregate.

### P0.6a — chunk-size simulation (`spikes/tracesim`, Rust or Python)

For C ∈ {16 KiB, 64 KiB, 256 KiB} (also 1 MiB as a reference column) and
commit interval T ∈ {10 s, 60 s, 300 s}, replay each volume and compute:

1. **Cold RMW count** per 1 000 writes and per GiB written: a partial write
   to a chunk that has been written before in the trace (the old content
   exists) and is not in a modelled clean-chunk cache. Model the cache two
   ways: (i) infinite (lower bound: only first touch after the cache would
   miss — every previously written chunk has to be fetched once), and (ii)
   a finite cache of 1 % and 10 % of the volume's touched-chunk working
   set under LRU. Also count partial writes to never-written chunks (no
   fetch; zero-fill).
2. **Uploaded data bytes per commit interval:** at each cut (every T of
   trace time), the number of distinct chunks dirtied since the previous
   cut × C (before compression), and the **write amplification**
   `uploaded bytes / logical bytes written` in the interval. Apply two
   compression assumptions: ratio 1.0 and ratio 0.5. (Dedup is out of scope
   for this simulation: no content in the traces; say so.)
3. **Map size:** touched chunks × 48 B (leaf entries) with the radix
   structure's interior overhead (fanout 512, 48 B interior entries),
   counted as the number of leaves = distinct `ci / 512`, and the
   resulting bytes at full occupancy (24 KiB per leaf) and at 0.5
   compression; plus the figure for a fully-populated volume of the
   volume's size. Present as MiB per TiB of volume and per TiB touched.
4. **Dirty-leaf count per seal and map write amplification:** at each cut,
   distinct dirty leaf indices `ci / 512` (and the interior nodes on their
   paths); `map bytes sealed = dirty_leaves × 24 KiB × (1.0 | 0.5) +
   path nodes`; map write amplification = `map bytes sealed / data bytes
   uploaded`. Report p50/p90/max over cuts, per volume, and the
   distribution across volumes. Repeat with fanout 256 and 1024 as a side
   table (leaf size scales) — informational for P0.7.
5. **Sanity checks to include as tests:** chunk-count and offset-math unit
   tests; a hand-computed 10-request fixture; determinism (fixed seeds,
   byte-identical CSV output on rerun).

**ADR-5 decision rule (fixed in advance).** Using the medium commit
interval T = 60 s and ratio 0.5 as the primary figure (T = 10 s and 300 s
are checked for sign agreement), compute across all volumes in the combined
set, per chunk size C:
`S3(C)` = data bytes uploaded + map bytes sealed per logical GiB written,
`RMW(C)` = cold-RMW fetches (finite 10 % cache) per 1 000 writes,
`MAP(C)` = map bytes per TiB touched.

- **64 KiB is confirmed** if at both p50 and p90 across volumes:
  `S3(64) ≤ 1.25 × min over C of S3(C)`, **and** `RMW(64) ≤ 2.0 × RMW(16)`,
  **and** map write amplification (p90) at 64 KiB ≤ 25 %.
- **Amend the default to 16 KiB** (and keep 64 KiB as the bulk profile) if
  `S3(16) ≤ 0.75 × S3(64)` at p50 **and** at p90. `MAP(16)` is expected at
  about 4× `MAP(64)` (ADR-5's trade-off); record it, but it is not a fail
  condition unless it exceeds 6×.
- **Amend to 256 KiB** only if `S3(256) ≤ 0.75 × S3(64)` at p50 and p90 and
  `RMW(256) ≤ 1.5 × RMW(64)`.
- Otherwise keep 64 KiB. Do not change the default for gains below these
  margins. Report the volumes where the rule fails separately; a `db`
  profile recommendation (ADR-5 mentions 16 KiB) may be added from them.

### P0.6b — cache policy simulation

Implement (or wrap) policies at chunk granularity (C = 64 KiB, also 16 KiB
as a check): **LRU**, **S3-FIFO** (small 10 %, main 90 %, ghost = main
size, frequency cap 3, per the SOSP'23 paper), **SIEVE** (NSDI'24),
**W-TinyLFU** (1 % window, SLRU main with 80 % protected, 4-bit count-min
sketch with doorkeeper, reset period 10× capacity; Caffeine's design), and
optionally ARC/2Q for context. Cross-check your implementations against
libCacheSim (`https://github.com/1a1a11a/libCacheSim`) on at least three
volumes; discrepancies > 0.5 % absolute in miss ratio must be explained or
fixed. Inputs: all reads and writes of each volume as accesses (writes
insert into the clean cache after destage, as Lunix does; model dirty data
as pinned and outside the evictable set by excluding in-flight dirty chunks
until the next cut). Cache sizes: 0.5 %, 1 %, 2 %, 5 %, 10 %, 25 % of the
volume's distinct touched chunks (working set). Metrics: request miss ratio
and byte miss ratio per policy/size/volume, and the number of S3 GETs
per 1 000 reads at 64 KiB. Add a scan-resistance test: inject a
sequential scan of 2× cache size into the middle of 10 representative
volumes. Stretch (report-only): a segment-granular variant where eviction
removes whole 64 MiB groups of chunks in write-time order (the segment-FIFO
layout of DESIGN §4) to estimate what hit ratio is lost to segment-granular
eviction.

**ADR-14 decision rule (fixed in advance):** at cache sizes 1 %, 5 % and
10 % of the working set, over all volumes, compare the median miss ratio:

- **S3-FIFO is accepted** if for every one of those three sizes its median
  miss ratio is ≤ 1.05 × the best policy's, **and** ≤ LRU's, **and** it
  loses to LRU on fewer than 25 % of volumes.
- **Amend to SIEVE** if SIEVE's median miss ratio is ≤ 0.95 × S3-FIFO's at
  two of the three sizes (SIEVE is simpler).
- **Amend to W-TinyLFU** only if it is ≤ 0.90 × S3-FIFO's at all three
  sizes (extra moving parts need a clear win).
- A scan test in which S3-FIFO's miss ratio is worse than LRU's by more
  than 2 % absolute is a failure of the rule irrespective of the medians.

**Deliverables:** `spikes/tracesim/`, `bench/tracesim/{TRACES.md,
RESULTS.md,raw/}` (per-volume CSVs; plots as PNG/SVG are welcome, using
matplotlib if Python), outcome blocks for ADR-5 and ADR-14.

---

## P0.7 — Radix Merkle map prototype (ADR-8)

**Goal:** show that the fixed-radix Merkle map delivers O(dirty) seals,
bounded memory, O(changed) diffs and lazy cold mounts at 1 TiB and 10 TiB,
and quantify it against the flat-table alternative that ADR-8 rejects.

**Check first:** DESIGN §7 gives depth as `ceil(log512(chunks))`: 3
levels up to 8 TiB and 4 up to 4 PiB at 64 KiB chunks. This was
corrected on 2026-11-11 after the first draft claimed 3 levels for
32 TiB. Compute the table for 1, 8, 10 and 32 TiB at 16, 64 and 256 KiB
and put it at the top of RESULTS.md.

**Steps (`spikes/radixmap`, Rust, single crate with a bench binary):**

1. Types: `ChunkId` (32 B), `Hint{seg u32, off u32… 16 B total}`, `Entry` 48
   B; leaf = 512 entries (24 KiB), interior = 512 × (child hash 32 B +
   location 16 B) (24 KiB); node hash = BLAKE3 over the canonical encoding
   (domain-separated by level); a well-known empty-node hash per level for
   holes. Fanout is a const generic or runtime parameter (256/512/1024).
2. Operations: `get(ci)`, `set(ci, entry)` into a **mutable overlay** (hash
   map of leaf-index → copy-on-write leaf), `seal()` (serialise dirty
   leaves, compress with LZ4 [and zstd-1 as a variant], recompute hashes up
   the paths, append nodes to an in-memory/`File` "map pack", return the new
   root and bytes written), `open(root)` with a bounded LRU node cache and
   lazy node loading through a `NodeSource` trait (file-backed with a
   configurable per-read latency to simulate S3), and `diff(rootA, rootB)`
   yielding changed leaf-index ranges by pruning equal subtrees.
3. **Flat baseline:** a flat table of 48 B entries (one per chunk), with
   (a) *full-table seal*: compress and hash the whole table in 1 MiB
   blocks, and (b) *block-delta seal*: only blocks containing changes are
   rewritten, but no tree, so the root hash is the hash of the block-hash
   list (O(volume/blocks) to recompute) and diff is a full scan of block
   hashes. Implement the minimum to measure seal time, bytes written, diff
   time, memory.
4. Workloads, at 1 TiB and 10 TiB logical (64 KiB chunks: 16.8 M and 168 M
   chunks), synthetic content (random `ChunkId`, hint fields incrementing):
   - **build:** bulk-populate 100 % and 10 % of chunks (sequential, then
     random order); record time, peak RSS, sealed bytes, node counts;
   - **seal:** from the 100 % populated state, dirty D leaves for D ∈ {1,
     10, 100, 1 000, 10 000, 100 000} with (i) uniformly random chunk
     positions (one chunk per leaf, worst case for amplification), (ii)
     Zipf-clustered positions, (iii) sequential runs; record seal time
     (single thread, excluding upload), bytes written (uncompressed and
     LZ4), BLAKE3 time share, and the amplification vs the dirty data;
   - **diff:** two roots differing in k leaves, k ∈ {1, 100, 10 000};
     record time and number of nodes read; assert nodes read ≤ 2 × k ×
     depth;
   - **cold open:** open root with an empty cache and a node source with
     25 ms/GET latency, time to first `get`, number of GETs (= depth), and
     the memory with the cache bound at 64 MiB, 256 MiB and unbounded
     after a random-read sweep of 1 M lookups;
   - **fanout sweep:** fanout ∈ {256, 512, 1024} for seal(D=1 000 and
     10 000, uniform).
5. Unit tests: roots are history-independent (two insert orders give the
   same root); an old root remains readable after a seal; `diff` equals a
   brute-force comparison on random 1 M-entry trees; empty-subtree sharing
   (a 10 TiB hole volume has a handful of nodes).

**Deliverables:** `spikes/radixmap/`, `bench/radixmap/RESULTS.md` (depth
table, workload tables with the flat baseline alongside).

**Gate (ADR-8, fixed in advance), measured at 10 TiB, fanout 512, 1 core:**

1. `seal` with 1 000 uniform dirty leaves takes ≤ 100 ms and writes ≤ 1.1 ×
   (D × 24 KiB × LZ4 ratio) + upper nodes (≤ 40 MiB uncompressed in
   total), and with 10 000 dirty leaves takes ≤ 1 s;
2. `diff` for k = 1 000 differing leaves takes ≤ 200 ms warm and reads ≤ 2·k·
   depth nodes;
3. the cold `open` + first `get` costs exactly `depth` node reads;
4. resident memory with a 256 MiB cache bound stays ≤ 256 MiB + overlay +
   64 MiB over the random-lookup sweep;
5. full-population in-memory size at 10 TiB is reported (expected ≈ 8 GiB at
   48 B/chunk uncompressed — it is why the cache is bounded; state it).

If all five hold, ADR-8 is **accepted (gate)** and the flat comparison is
recorded as the evidence. If any fails by ≤ 3×, report the closest fanout
from the sweep (**amend** to it if it beats fanout 512 by ≥ 25 % on seal
bytes at D ∈ {100, 1 000, 10 000} and satisfies all five). If any fails by
> 3×, ADR-8 is **not accepted**; report the dominant cost (hashing,
serialisation, allocation) and propose the mitigation the DESIGN already
names (map-delta records in DELTA commits) as a new proposed ADR rather
than inventing a format here. The flat baseline's role is advisory: if the
tree's seal cost at D = 1 000 is within 2× of the block-delta flat table's
**and** the flat table lacks diff and root verification, record that the
tree is justified by function, not by seal cost.

---

## P0.8 — Crypto design review for ADR-11

**Goal:** a written review before the key hierarchy and extent framing
freeze, with questions for an external reviewer. The executing model is not
the cryptographer of record; its output is a threat model, a checklist
with findings, and tests that tripwire the nonce rule.

**Steps:**

1. Read ADR-11, DESIGN §5 and §15, the data-path note §5, Constellation's
   `crates/store-s3/src/e2e.rs`, and `docs/security-audit-2026-09.md`
   (Constellation). Write
   `docs/explanation/research/<today>-adr-11-crypto-review.md`
   (question, scope, threat model, findings table with severity, open
   questions, sources).
2. Cover at least these checklist items, each with a verdict (OK / issue /
   needs-reviewer) and a reasoning paragraph:
   1. **Nonce/key uniqueness of the salted per-extent key.** ADR-11 was
      amended on 2026-11-11 after this plan's drafting found a hole. The
      first draft derived the key from `DEK_epoch ‖ chunk_id` with a
      constant nonce, so recompression (ADR-12) or a zstd version change
      would reuse `(key, nonce)` over different bytes.
      The current rule is: key = `derive_key("lunix extent v1", DEK_epoch ‖
      chunk_id ‖ salt)`, with a random 192-bit salt per extent stored in
      the header and a constant nonce. Verify the following:
      - every encryption draws a fresh salt, including retries, recompress,
        compaction re-encodes and crash-retried destage;
      - the 192-bit salt makes key collisions negligible at the expected
        extent counts;
      - the derive_key context string is domain-separated from the other
        purposes.
      Also cost the per-extent key schedule (BLAKE3 derive plus AES key
      expansion) at 64 KiB and 16 KiB extents.
   2. Same-chunk-id different *epochs* and *families* (family keyring,
      clones): are `DEK_epoch` and family id both in the derivation?
   3. AAD contents (format version, codec, `ulen`, `chunk_id`): which
      attacks do they prevent (relocation, codec swap, truncation,
      decompression-bomb `ulen` tampering); are volume id/segment/offset
      needed for relocation resistance across volumes in one family?
   4. Keyed addressing: the addressing key and what the provider learns
      (equality of chunks within a family, sizes, access patterns); the
      compress-then-encrypt length side channel; the `pad` option.
   5. Verify-before-use order (tag → bounded decompress → recompute
      `chunk_id`) and constant-time comparisons; behaviour on tag failure.
   6. Commit/DELTA/lease encryption: XChaCha20-Poly1305 with random 24-byte
      nonces under the metadata key — volume of messages per key, key
      separation per object kind, AAD = object identity; MAC over the map
      root, epoch and parent in FULL commits; rollback and fork detection
      requiring an anchor outside the bucket (DESIGN open question):
      enumerate the candidate anchors (local state, KMS context, transparency
      log) and what each defends.
   7. Key slots: Argon2id parameters and their storage; KMS wrapping with an
      encryption context binding family id and slot id; slot add/remove and
      the semantics of *removing* a slot (it does not revoke an exposed KMK;
      state that); KMK compromise vs DEK-epoch rotation (rotation protects
      only new writes); passphrase change as KMK re-wrap.
   8. FIPS profile (HKDF-SHA-256, HMAC-SHA-256): equivalence of derivation
      structure; whether `aws-lc-rs` AES-GCM with an externally fixed nonce
      is permitted by its API in FIPS mode (it normally forces a unique IV
      source); note the conflict with a fixed nonce if any.
   9. Memory handling: `mlock`, `Zeroizing`, core-dump forbid, swap, keys on
      queue threads; derive-key cost per chunk (the cost of a BLAKE3 `derive_key` per
      chunk is measured in step 3).
   10. Downgrade: an authenticated volume descriptor so a bucket-writer
       cannot flip a volume from encrypted to plaintext (Constellation audit
       finding 12).
   11. Cross-protocol/domain separation strings and versioning; test vectors.
3. `spikes/crypto-kat`: Rust tests using the real crate choice (`aws-lc-rs`):
   (a) known-answer vectors for the derivation (publish them in the note),
   (b) encrypt/decrypt round trips with tamper cases for each AAD field,
   (c) a **nonce-uniqueness tripwire test** modelling the extent writer:
   it must fail in the model if a `(key, nonce)` pair is used for two
   distinct plaintexts, and it demonstrates item 1 concretely by showing the
   XOR leak with two codecs, (d) measured throughput of
   `derive_key + AES-256-GCM seal` for 4 KiB, 16 KiB and 64 KiB inputs on
   one core (to cost the per-chunk derivation), (e) the same with the
   fix(es) proposed in item 1.
4. List **questions for an external reviewer** (a numbered list at the end of
   the note, each self-contained, with the relevant ADR/DESIGN excerpt
   inlined).

**Deliverables:** the note, `spikes/crypto-kat/`,
`bench/crypto-kat/RESULTS.md`.

**Gate (ADR-11):** the review cannot accept the ADR on its own. Outcome is
one of *amended* (a finding requires a design change, with the proposed
text) or *accepted pending external review* (no blocking finding; the ADR's
status stays proposed until the reviewer answers). A finding of severity
"breaks confidentiality or integrity" blocks the format freeze: state that
loudly at the top of the report.

---

## P0.9 — Shared crates: `replicix-core` extraction (conditional on ADR-19)

**Precondition check (do this first):** open
`docs/explanation/DECISIONS.md` ADR-19. If the maintainer has not marked it
accepted (status line or a dated acceptance block), **skip this milestone**
and say so in the report; do not interpret silence as consent and do not
create any files. The stopgap named in ADR-19 (copying files into Lunix
with MPL headers kept) is *not* part of this plan; phase 1 decides it.

**Steps (only if accepted):**

1. Location: a new sibling workspace `/home/bra/cvs/replicix/replicix-core`
   unless ADR-19's acceptance block names another. Do not initialise a git
   repository (the coordinator does). `Cargo.toml` workspace, MPL-2.0
   `LICENSE`, `rust-toolchain.toml` copied from Constellation, edition 2021.
2. Crates, with sources from Constellation's checkout (read-only; copy,
   keep the MPL-2.0 header in every copied file, record the source commit or
   "uncommitted" in each crate's `PROVENANCE.md`):
   - `replicix-upload-concurrency` ← `crates/upload-concurrency/src/{lib,gate}.rs`
     (as-is);
   - `replicix-objcas` ← `crates/store-s3/src/{cas,classify,probe,
     aws_auth}.rs`, the single-writer part of `lease.rs`, and the
     preflight machinery from `store.rs::preflight`/`engine/src/doctor.rs`
     that does not need FS types. Decouple: replace `constellation_platform`
     clock/`native()` with a `Clock` trait (system clock default, injectable
     in tests); no dependency on `fs-core`, `mtree` or `platform`; keep the
     30 s retry cap and `RedactingConnector`; pin `object_store` to the
     version Constellation uses (the classifier text-matches errors);
   - `replicix-chunkfmt` ← `ChunkHash` and `ChunkLayout::slices` from
     `fs-core/src/chunk.rs`, `format.rs` (`CCH1`, `StreamingDecoder`),
     `codec.rs` (add `CODEC_LZ4 = 2`; keep `max_decompressed_len` and
     `decompress_bounded`), and `e2e.rs` (KMK, Argon2id wrap, derive-by-
     purpose, XChaCha20-Poly1305 envelope; **drop** the gossip purpose;
     **do not** add Lunix's AES-GCM extent scheme here — it is gated by
     P0.8 and phase 1);
   - `replicix-platform-lite` ← `crates/platform/src/{lock,fs,secrets}.rs`
     parts: process lock, fs primitives, core-dump forbid; feature-gate any
     `zbus` use.
   - `csi-common` is **not** extracted in phase 0 (phase 6).
3. Tests: carry over the unit tests for each moved module; add **golden
   compatibility tests**: objects produced by Constellation's `encode_object`/
   `encrypt_object` (checked-in fixtures generated from Constellation's
   code, with the generating command in the fixture README) decode in
   `replicix-chunkfmt`, and vice versa, byte for byte.
4. Constellation must be unaffected: do **not** edit Constellation in this
   milestone except, if the maintainer's ADR-19 acceptance says so, adding a
   `docs/plans/v1/wip/` stub. Verify `cd /home/bra/cvs/replicix/constellation
   && unset CARGO_TARGET_DIR && cargo check --workspace` still passes
   (compare to a baseline run before you start; if the baseline already
   fails, report it and do not "fix" it).
5. Write `replicix-core/README.md` listing the crates, their public API
   surface, the migration plan for Constellation (`path` dependencies first,
   then swap one module at a time) and what remains Lunix-private.

**Gate:** `cargo fmt --all`, `clippy --workspace --all-targets -- -D warnings`
and `cargo test --workspace` green in `replicix-core`; golden tests pass in
both directions; no identifier or crate name containing `constellation` in
the new workspace (`rg -i constellation` outside `PROVENANCE.md`, README and
comments about origin returns nothing); every copied file keeps its MPL-2.0
header; Constellation's `cargo check --workspace` result is unchanged.
Outcome recorded under ADR-19 as "executed: replicix-core at <path>,
Constellation not yet migrated".

---

## P0.10 — S3 probe sweep (ADR-2, DESIGN §9 `doctor`)

**Goal:** a provider matrix: which backends support the conditional-write
protocol the authority needs, and what PUT/GET latency to expect (inputs to
the sync-mode fsync estimate and to the `doctor` design).

**Credentials (read the CONVENTIONS rule 6 again):** never hardcode,
print, or write keys, bucket names holding private data, or signed URLs
into any file. For each provider `<NAME>` ∈ {`AWS`, `R2`, `OVH`, `MINIO`,
and any other the maintainer has configured} read:
`LUNIX_PROBE_<NAME>_ENDPOINT` (optional for AWS), `_REGION`, `_BUCKET`,
`_PROFILE` (an AWS shared-config profile name) or the standard `AWS_*`
variables supplied by the maintainer's environment. A provider whose
variables are unset is **skipped** and listed as "not run: no credentials".
The only provider you may bring up yourself is a throwaway local MinIO
(`docker run` with generated random root credentials kept in memory/env,
bound to 127.0.0.1, removed at the end); skip it if docker is unavailable.
All objects are written under `lunix-p0-probe/<run-id>/`; list and delete
only that prefix at the end, never anything else. Cap the run at about
5 000 requests and < 2 GB transferred per provider and state the estimated
cost in the report.

**Steps (`spikes/s3probe`, Rust):**

1. Prefer re-using code: if P0.9 ran, depend on `replicix-objcas`; else
   depend by `path` on Constellation's `constellation-store-s3`
   (`probe_cas_semantics`, `cas::put_conditional`) from the spike only, and
   note that this is a spike-only dependency, pinned to the same
   `object_store` version.
2. Semantics probes per provider (each prints PASS/FAIL/UNSUPPORTED plus
   the HTTP status observed):
   - `If-None-Match: *` create: first wins, second gets 412;
   - `If-Match` swap success, and a stale ETag gets 412; `If-Match` on a
     deleted key (404 handling);
   - 8-way concurrent create of one key: exactly one winner, others
     412/409-then-retry; repeat 20 times; 8-way concurrent swap: exactly one
     winner per round, 100 rounds, with torn-read check;
   - ranged GET (`bytes=a-b`, suffix range); read-your-write on PUT then
     GET and PUT then LIST (consistency); multipart create/abort (the
     probe, not a transfer); versioning state if queryable; clock skew via
     the `Date` header against local time (> 2 s skew flagged).
3. Latency/throughput per provider from the machine you run on (record its
   public region/IP geography only as a coarse label, e.g. "EU home
   broadband" or "AWS eu-central-1 EC2", plus TCP connect and TLS RTT):
   - sequential PUT and GET at 4 KiB and 64 KiB (n = 200 each) and at
     16 MiB (n = 40); distinct keys; record p50/p90/p99/max and TTFB for
     GETs;
   - parallel PUT/GET of 16 MiB objects at concurrency 1, 8, 32; aggregate
     MB/s;
   - `HEAD` and a 10-key `LIST` latency.
   Repeat at two different times if feasible (note "time of day").
4. Output a CSV per provider (`bench/s3-probe/raw/<provider>.csv`) and a
   Markdown matrix in `RESULTS.md`: rows = providers, columns = semantics
   results, latency percentiles, caveats (such as "R2: LIST not strongly
   consistent for X"). Quirks discovered that Constellation's classifier
   does not already handle are listed as bugs against `replicix-objcas`/
   `classify.rs`.

**Deliverables:** `spikes/s3probe/`, `bench/s3-probe/{RESULTS.md,raw/}`.

**Gate / rule (ADR-2, DESIGN §9):** a provider qualifies as an *authority*
backend iff all of: create-if-absent and If-Match swap behave as above, the
8-way create has exactly one winner in 20/20 rounds, the 8-way swap has
exactly one winner in 100/100 rounds with no torn reads, and PUT-then-GET is
read-your-write. A provider that fails is a *mirror-only* backend (record
the reason). The measured 4 KiB/64 KiB PUT p50 per provider becomes the
"≈ 1 PUT RTT" figure for sync-mode fsync in DESIGN §20. AWS, R2 and OVH
results decide whether ADR-2's "authority requires conditional writes"
holds for the three launch providers; a launch provider that fails is
escalated to the maintainer in the report (do not edit ADR-2).

---

## P0.12 — Keeper-held RAM write buffer (ADR-26)

Run this after P0.3, reusing its file-backed recoverable target and
driver. It needs root.

**Steps:**
1. Add a `--sink mem` mode to the P0.3 target.
   - Writes are appended to a `memfd` ring, in a journal-like record
     format with a header `{magic, device id, generation}`, and are
     acknowledged from RAM.
   - FLUSH copies the ring's records up to the flush point into the
     backing file and `fdatasync`s it. This stands in for the S3 DELTA
     commit.
2. Write a keeper, `spikes/ublk-recovery/keeper`.
   - It receives the memfd via `SCM_RIGHTS` on a Unix socket under
     `$LUNIX_SPIKE_DIR` and hands it back to the restarted daemon.
   - Also try the systemd FD store with a transient unit
     (`systemd-run -p FileDescriptorStoreMax=4`), if available.
     Otherwise mark it "not run".
3. Run the P0.3 cycle with ext4 mounted and fio verify running: kill -9
   the daemon, restart it, take the memfd from the keeper, replay it,
   recover. Run at least 20 cycles.
4. **Lost keeper:** kill both the keeper and the daemon. The restarted
   daemon must find no buffer (or a generation mismatch), refuse
   recovery and delete the device. Record what the mounted ext4 and
   fio see. EIO is expected. Silent success with stale data is a
   failure.
5. Measure the acknowledgement latency of the RAM sink against the P0.5
   journal (4K QD1/QD32), and the replay time of a 256 MiB buffer.

**Gate:**
- All cycles finish with fsck clean.
- fio verify finds no mismatch for any write that was acknowledged by a
  FLUSH before the kill.
- The lost-keeper case never resumes the device.

Results go in `bench/ublk-recovery/RESULTS.md` §P0.12.

## P0.11 — Close-out: decision records and `bench/BASELINE.md`

**Steps:**

1. For each gated ADR (2, 4, 5, 8, 11, 14, 17, 18, and 19 if P0.9 ran),
   append the dated "Gate outcome" block described in CONVENTIONS
   "Recording decisions" to `docs/explanation/DECISIONS.md`: status
   (*accepted (gate)* / *amended* / *rejected* / *pending external review* /
   *not run*), the rule from this plan, the measured values, and links to
   the `bench/…/RESULTS.md` files and notes. Add new proposed ADRs for
   findings that need a decision and have no ADR (for example a nonce-
   uniqueness fix from P0.8, a map-delta mitigation from P0.7, a DESIGN §7
   depth correction). Update the Status paragraph at the top of
   DECISIONS.md to say which ADRs are now gate-accepted and which remain
   proposed, in the existing prose style.
2. List, in the report (not in DESIGN.md), every DESIGN.md line that the
   outcomes contradict, with a suggested replacement; the maintainer applies
   them (CONVENTIONS rule 5).
3. Write `bench/BASELINE.md`: the host header; one table of headline numbers
   with the source `RESULTS.md` for each (kernel-reference vs ublk null/loop
   IOPS and latency, USER_COPY/BATCH_IO deltas, recovery stall p50/p99/max,
   FLUSH per fsync per fs, journal flush latency per fs, S3 PUT/GET p50 per
   provider, map seal/diff figures, chunk-size and cache-policy verdicts);
   the DESIGN §20 target table with a "ceiling measured / target / status"
   column; known limitations of the measurements (VM or bare metal,
   loop-backed file systems, trace subset, provider vantage point); and the
   exact commands to rerun each benchmark (phase 1 and 8 CI reuse them).
4. Final hygiene: `bash spikes/common/ublk-leftovers.sh` clean; modules you
   loaded (`null_blk`) removed if you loaded them; governor and sysctls
   restored; large scratch files under `$LUNIX_SPIKE_DIR` deleted (keep
   `raw/` results); trace files left in place and listed with sizes.
5. Update `docs/plans/v1/README.md` row 00 to the final state in your
   report's wording; the coordinator moves the plan file to `done/` after
   review.

**Gate:** every gated ADR has an outcome block or an explicit "not run:
reason" entry; `bench/BASELINE.md` exists and every number in it traces to a
file under `bench/`; each spike builds (`cargo build --release`, `cargo
fmt --check`, `cargo clippy -- -D warnings`) and its README's reproduce
command runs (or is marked "needs root"/"needs credentials").

---

## Out of scope

- Any product code: no `lunixd`, no workspace at the repo root, no journal
  or segment format implementation, no real destage pipeline. The spike
  targets are throwaway; do not let their structure leak into phase 1
  decisions beyond what a RESULTS.md states.
- Editing `DESIGN.md`, changing ADR text above the gate-outcome blocks, or
  changing Constellation (P0.9 aside, and then only verification).
- Zero-copy ublk modes (`SUPPORT_ZERO_COPY`, `AUTO_BUF_REG`, `NEED_GET_DATA`),
  unprivileged ublk, zoned/integrity features (ADR-17 excludes them).
- Evaluating libublk 0.5.0's tokio executor (only noted if published),
  `tokio-uring`/`monoio`/`glommio` (rejected in ADR-18), S3 Express One Zone
  and GCS adapters, multi-backend mirroring, GC, CSI, encryption
  *implementation* in the product (P0.8 is review plus tests only).
- Dedup-ratio and compression-ratio measurement on real data (phase 1
  reports them); the trace simulation has no content.
- Performance tuning of the host (huge pages, IRQ affinity, SQPOLL) beyond
  the pinning in CONVENTIONS.
- xfstests, fuzzing and CI wiring (phases 1 and 8).

## Report format

Follow CONVENTIONS "Reporting back" and, in addition:

1. **Header:** host (VM/bare metal, CPU, kernel), dates, tool and crate
   versions used, whether root and credentials were available.
2. **Per milestone P0.1–P0.11:** status (done / partial / skipped / not
   run), the command that reproduces it, the headline numbers (a 3–6 row
   table), the gate rule restated in one line, the measured values, and the
   outcome (*accepted (gate)*, *amended*, *rejected*, *pending external
   review*, *skipped*).
3. **Findings that change the design** (for example the nonce-reuse
   analysis in P0.8, the §7 depth correction, any ublk feature missing on
   this kernel), ranked by impact, each with the evidence file.
4. **DESIGN.md lines contradicted** with suggested replacements (not
   applied).
5. **Deviations** from this plan and why; cells or matrices not run.
6. **Leftover check:** output of `spikes/common/ublk-leftovers.sh` at the end
   and the list of files left in `$LUNIX_SPIKE_DIR`/`$LUNIX_TRACE_DIR`.
7. **Open questions for the maintainer** (ADR-19 acceptance, credentials
   for further providers, the external crypto reviewer).
