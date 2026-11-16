# M16 prep report: performance gate and the ADR-5 chunk-size benchmark

- Milestone M16, **preparation only**: branch `p1/m16`, base `321e9dc`, 2026-11-16. No gate was run (no Lunix device, no root
  run of Lunix, and the host is not quiet: load average 8-10 from other agents). Host: kernel 7.0.0, 16 CPUs (AMD EPYC, no
  cpufreq exposed), 30 GiB, fio 3.41, PostgreSQL 18.6 (server and pgbench installed), `perf` present, `null_blk` available;
  this work used CPUs 12-15.
- Files: `tests/perf/`, `bench/phase1/perf/`, `bench/phase1/chunksize/` (the three paths M16 owns). Nothing else was touched.

## What is ready

| item | files | state |
|---|---|---|
| shared harness | `tests/perf/lib.sh` (copy of M15's `tests/exit/lib.sh`), `tests/perf/perflib.sh` (quiet-host check, governor set/restore, daemon CPU seconds and thread split from `/proc`, resident set, fio summary, pinned attach) | shellcheck clean; ran against a fake |
| Part A (DESIGN §20 cells) | `tests/perf/partA.sh`, `bench/phase1/perf/fio/{fill,r4k,w4k,seqw,seqr}.fio` | against the fake |
| same-session references | `bench/phase1/perf/fio/{nullblk-ref,journal-raw}.fio`, run by `partA.sh` | `null_blk` module exists on the host; the journal file system limit measured next to the volume directory |
| Part A analysis | `bench/phase1/perf/analyze.py` | run on synthetic fixtures (`tests/perf/make-fixtures.py`) |
| Part B (ADR-5) | `tests/perf/chunksize.sh`, `bench/phase1/chunksize/fio/{randmix,seqrand}.fio`, `pglat.py`, `packages.txt` | fio workloads and a scale-1 pgbench against the fake; the VM-image workload is untested (no images) |
| Part B analysis and the rule | `bench/phase1/chunksize/analyze.py` | run on fixtures; prints the three clauses with numbers and the proposed "Gate outcome" block |
| self-tests | `tests/perf/selftest.sh` (no root), `selftest-fake-root.sh` + `fakes/fake-lunix.sh` | see "Checked how" |

### Part A

- Per (chunk 64 K and 16 K) x (4 and 8 queues): fresh 32 GiB volume, `journal_max` 1 GiB, queue threads pinned to
  `QCPUS_4` (0-3) or `QCPUS_8` (0-7) with `lunix attach --cpus`, the daemon process to `DAEMON_CPUS` (0-7), fio to `FIO_CPUS`
  (8-15): disjoint sets, stated in the output. Cells `r4k` (4k randread QD 128, 8 jobs, the 4 GiB working set written first),
  `w4k` (60 s), `seqw` (1M, QD 32, 50 % compressible, each job its own slice, wrapping), `seqr`; 3 repetitions, 30 s + 5 s
  ramp; then `r4k` and `seqr` once with `verify_reads = true` (edited into `volume.json` of the detached volume; the key
  must exist or `set_vj` fails loudly). Outer `timeout -k` 1.5 x runtime; the fio command lines are the committed job files
  plus the exported variables, recorded per cell in `results.jsonl`.
- Per cell: IOPS, bandwidth, clat p50/p99/p99.9, daemon CPU seconds over the measured window (`/proc/<pid>/stat`
  utime+stime, all threads), CPU microseconds per I/O, the thread split by thread name, resident set before and after and the
  high-water mark, the idle resident set after attach, journal bytes in use at the end of write cells and 10 s later (the
  destage backlog), the load average, `perf stat` and `perf record` (top of `perf report`) of the first repetition when
  `perf` exists (`PERF=1`).
- References of the same session: null_blk with the same four cells (memory backed, no completion delay) and the raw fio limits
  of the journal's file system (4k random writes at depth; 64 k and 4 k writes with `fdatasync` after each, the shape of group
  commit).
- `analyze.py`: median and min-max, noisy when max/min > 1.2, the verdict per row at the better of 4 and 8 queues (for `r4k`
  the p99 target at the same queue count) with the oversubscription printed ("8 queue threads + 8 fio jobs = 16 on 16 CPUs");
  for every shortfall the BASELINE ceiling, the null_blk and journal references, CPU per I/O, the busiest thread's share of a
  core and the split. No threshold is read from anywhere but the DESIGN §20 table in the script.

### Part B

- `chunksize.sh`: workloads `randmix` (30 min bssplit mix, QD 32, 30 % duplicates, 50 % compressible, 20 GiB file on ext4),
  `seqrand` (16 GiB sequential fill, then a 15 min random overwrite), `pgbench` (scale 1000, `-c 16 -j 4 -T 1800`, default
  `full_page_writes`, ext4 **and** xfs, per-transaction log at 10 % sampling for the latency percentiles, PostgreSQL version
  recorded) and `vm` (`IMAGES="a.raw b.raw c.raw"` copied to the volume, then per image a chroot on a loop-mounted copy:
  `apt-get update`, `upgrade`, install of `packages.txt` (250 names, those the distribution offers; the count that installed is
  recorded and must be at least 200)). Every cell: fresh 48 GiB volume (`--id-len 16`, `auto`, integrity mode, `journal_max`
  1 GiB, queues 4), df-guard before every mkfs, status before and after the workload, daemon CPU seconds, three GC passes 75 s
  apart (the grace is 60 s) and a final status, detach, `lunix fsck --deep` (the recount), the on-disk store.
- `analyze.py` computes, per workload, chunk size and file system: logical GiB written, dedup and compression, live data, map
  bytes per TiB mapped and per GiB written, the map seal write-amplification **proxy**, store after the final GC, stored
  (data plus map) bytes per logical GiB, daemon CPU per GiB, `cold_rmw` per 1000 writes, IOPS/latency (fio) or TPS and latency
  percentiles (pgbench); cells run more than once get median and min-max, cells run once are marked "noise caveat". It applies
  the **advisory rule exactly as fixed in the plan** (clause 1: stored bytes per logical GiB over the VM-image and pgbench
  workloads >= 15 % lower at 16 KiB; clause 2: daemon CPU per GiB <= 1.3x; clause 3: Part A rows that pass at 64 KiB pass at 16 KiB,
  from `perf/analyze.py --json`), prints the recommendation and the proposed "Gate outcome" block for ADR-5, and prints the
  per-workload numbers beside the aggregate so a split is visible.

## Checked how

| check | result |
|---|---|
| `bash -n`, `shellcheck -S warning`, `python3 -I` syntax of every script | clean |
| `fio --parse-only` of every job file with the exported variables | PASS (`selftest.sh` on the host) |
| both analysers on synthetic fixtures (`selftest.sh`) | PASS: the shortfall, verify_reads and rule sections appear |
| `selftest-fake-root.sh`: `partA.sh` (tiny: 1 GiB working set, 2 reps, 6 s cells, null_blk reference, journal reference, verify_reads switch) and `chunksize.sh` (randmix, seqrand, pgbench scale 1, final GC, fsck) against `fakes/fake-lunix.sh` (loop devices instead of ublk), on a tmpfs scratch directory | PASS for both (`partA.sh` 8 cells + 2 verify_reads cells + both references; `chunksize.sh` randmix, seqrand and pgbench each with GC, deep fsck). Bugs found this way and fixed: a global `RUNTIME` overwritten by a cell, `initdb` as root and without a usable locale, the df-guard size not overridable for a dry run |

The fake proves the control flow only. The numbers it produced (fio on a tmpfs-backed loop device) mean nothing, and the
daemon CPU split is empty because the fake daemon sleeps.

## What each gate run needs

| need | detail |
|---|---|
| a quiet host | `LUNIX_HOST_QUIET=1` is required by `partA.sh` and `chunksize.sh` (exit 77 without it; `FORCE_NOISY=1` for a dry run, recorded as noisy). Other agents' load is 8-10 now on CPUs 0-11. Part A wants CPUs 0-15 free: queue threads on 0-7, fio on 8-15 |
| binaries | `lunixd` release (`LUNIXD`), `lunix` CLI (`LUNIX`), `lunix-ublk-testd` (`TESTD`), optional `lunix-fsck` (`LUNIX_FSCK`) |
| tools present on the host | fio 3.41, `perf`, `null_blk` (module present), PostgreSQL 18.6 server and `pgbench` (`/usr/lib/postgresql/18/bin`), jq, python3. Nothing was installed for M16 |
| for the VM workload | three raw cloud images (two adjacent releases and one other distribution) in `IMAGES`; network from the host to the distribution mirrors (apt runs in the chroot); `/dev` is bound read-only into the chroot (see question 5) |
| command | `tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_SPIKE_DIR=<abs> LUNIXD=.. LUNIX=.. TESTD=.. bash tests/perf/partA.sh` and `.../chunksize.sh`; each prints its cap (set `LUNIX_ROOT_TIMEOUT` to cap + 600) |

### Expected runtime and disk (estimates)

| run | runtime | peak disk |
|---|---|---|
| Part A, both chunk sizes, 4 and 8 queues, 3 repetitions, verify_reads cells, references | about 80 min + 5 min references (4 volume setups, fill 1 min, r4k 3 x 35 s, w4k 3 x 65 s, seqw 3 x 35 s, seqr 3 x 35 s, 2 verify_reads cells, fsck) | working set 4 GiB plus 8 GiB of seqw slices rewritten in place, GC on: store below 25 GiB; df-guard asks 30 |
| Part B, `randmix` + `seqrand` | 2 sizes x (45 min + 35 min) about 2.7 h | file 20 GiB and 16 GiB, store below 30 GiB |
| Part B, `pgbench` ext4 and xfs | 2 sizes x 2 file systems x (init about 15 min + 30 min + 3 GC passes 4 min + fsck) about 4.5 h | scale 1000 is about 15 GB plus WAL: store below 40 GiB per cell |
| Part B, `vm` | 2 sizes x about 1 h = 2 h | three raw images plus apt churn: below 40 GiB |
| total of Part B | about 9-10 h | one 48 GiB volume at a time, below the 100 GiB cap |

The plan asks for three repetitions of the shortest cells and the long cells once: `REPS` (default 1) and `REPS_<WORKLOAD>`
(for example `REPS_VM=3`) set it; which cells are "shortest" is the lead's call.

## Open questions for the lead

1. **Disk.** The host has 121 GiB free and the floor is 120, so `df-guard` refuses every `mkfs` (see M15's report); Part B needs
   at least 180 GiB free (60 + 120), Part A 150.
2. **Part A pinning on a 16 CPU host.** With 8 queues, 8 queue threads + 8 fio jobs = 16 threads on 16 CPUs, and the daemon's
   journal, GC and worker threads share the CPUs 0-7 with the queue threads: the oversubscription is printed, not removed. The
   plan's "8 workers" for `seqw` and any worker-count option of `lunixd` are not set by the script (no CLI flag found on main);
   say if there is a config key to add to `lunixd.toml` via `CFG_EXTRA`.
3. **Read cache.** `volume.json` `read_cache_max` defaults to 128 MiB, the Part A working set is 4 GiB: "cache hits" for `r4k`
   depends on whether local reads count as hits regardless. The script leaves the default; `set_vj <name> read_cache_max <bytes>`
   before the cell is a one-line change if the lead wants the cache to hold the working set.
4. **Map seal write amplification.** There is no cumulative counter of map bytes written; the analysis uses the space ledger's
   map bytes before the final GC over the bytes written (a proxy, named as such in the table). A counter in `EngineStats` would
   make it exact.
5. **VM images in a chroot need the host's `/dev`.** The script bind-mounts it read-only (`--rbind -o ro /dev`, the host's
   devices, not the image's); the image's own mount is `nosuid,nodev`. CONVENTIONS rule 3 says every mount is `nosuid,nodev`
   under the scratch directory; this one is the exception. The alternative is `mknod` inside the image, which `nodev` forbids.
   The deb cache of each image is kept outside the volume (`$LUNIX_SPIKE_DIR/apt-cache-img<i>`) so the second chunk size reuses
   the packages, but `apt-get update` lists are fetched on the day, so the two sizes see the same package set only when run the
   same day: run both chunk sizes back to back.
6. **PostgreSQL user.** The `postgres` user cannot traverse the home directory that holds the scratch directory (mode 0750), so
   the server and pgbench run as the owner of the scratch directory (`PG_USER`). `initdb -A trust`, unix socket only (`listen_addresses=''`, port 5433), `shared_buffers` 2 GB,
   everything else default. If the lead wants PostgreSQL tuned (for example `max_wal_size`), say so; the plan says defaults.
7. **GC and `lunix gc`.** The final GC passes use `lunix gc NAME --json`; main's `lunixd` answers "no collector" until M17.B is
   wired (`daemon.rs`). Without it the "on-disk store after the final GC" equals the store before GC.
8. **Counters.** The analysis reads `lunix status --json` fields `stats.{bytes_written,ops_write,cold_rmw}`, `space.map`,
   `journal_used` and `fsck --json` `ratios.*`; `cold_rmw` and the other counters are "persisted in the checkpoint, never
   authoritative", so the harness takes deltas of two attached samples.
9. **The `auto` codec choice.** The sequential ingest cell of the plan is "on 8 workers with `auto`"; the harness uses the
   volume default `--compress auto` for every cell.
10. **Statistics.** Median and min/max over three repetitions for Part A cells; the long Part B cells run once and the
    tables say so; no confidence intervals are computed because three samples do not support them (the plan asks for median
    and min/max).
