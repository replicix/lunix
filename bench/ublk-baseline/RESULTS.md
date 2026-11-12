# P0.1 results: libublk null/loop vs kernel references

Spike: `spikes/ublk-baseline` (binary `ublk-spike`, libublk `=0.4.8`, io-uring `=0.7.16`, clap 4.6.7).
Reproduce: `cd spikes/ublk-baseline && cargo build --release && sudo ./run-baseline.sh`
(run as: `RUNTIME=5 RAMP=1 REPS=3`). Date: 2026-11-11. Spike revision: uncommitted.
Crates.io check on the host (`cargo search`/`cargo info`, 2026-11-11): libublk latest = 0.4.8, no 0.5.x published, so 0.5.0 was not evaluated; io-uring latest = 0.7.16.

## Host

AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD (write cache reported as
"write through"), ext4 root. Kernel 7.0.0 (Ubuntu), Ubuntu 26.04. fio 3.41. Full header: `raw/hostinfo.md`.
CPU governor and turbo: **not exposed** (no cpufreq in this VM), so no governor was set or restored.

Prerequisite verdicts (common setup step 5):

| check | result |
|---|---|
| `modprobe ublk_drv` | works (loaded by this session; still loaded, see ../ublk-recovery/RESULTS.md) |
| `/dev/ublk-control` | appears after the modprobe |
| `kernel.io_uring_disabled` | 0 |
| `modprobe null_blk` | works (with the plan's parameters; loaded by this session) |
| `UBLK_U_CMD_GET_FEATURES` mask | `0x7ffff`: all 19 flags the 7.0 header defines are set (see P0.3 table) |
| `CONFIG_BLKDEV_UBLK_LEGACY_OPCODES` | not set; libublk uses `UBLK_F_CMD_IOCTL_ENCODE` (we pass it explicitly) |

## Deviations from the plan (read before the numbers)

1. **CPU split 4 + 4, not 8 + 8.** The host is shared; this agent owns CPUs 0-7. Queue threads are pinned to
   CPUs 0-3 and fio to CPUs 4-7 (disjoint). With 16 vCPUs blk-mq maps CPU k to its own hardware queue only when
   `nr_hw_queues` is 16 (with 8 queues a queue covers two adjacent CPUs, so fio on 4-7 would have reached only
   two queues), so ublk uses **16 queues x depth 256** (4 are active) and `null_blk` uses `submit_queues=16`.
   `numjobs=8` therefore means two fio jobs per CPU, which shows up as a ~3 ms p99 (scheduler time slice) on
   null_blk and ublk at QD32/128 with 8 jobs: that tail is a host artefact, not a device property.
   The 1-queue configuration (QD1 rows) uses 1 queue x depth 128, thread on CPU 0, fio on CPU 4.
2. **Shorter runs.** `--runtime=5 --ramp_time=1` (plan: 20/3), 3 repetitions, to fit the box. Noisy cells
   (max/min > 1.2) are flagged `Y` in the `noisy` column of the summaries; none of the headline cells is flagged.
3. **Loop backing file 16 GiB** (plan: 32 GiB) because of the 20 GiB disk budget; fio `--size=16g`.
4. **Latency is total latency (submission to completion)**, `fio --lat_percentiles=1`, not clat: with io_uring
   `null_blk` completes inline during submit and clat would read ~0.
5. **Daemon CPU** is the whole process (`/proc/<pid>/stat`, utime+stime) divided by the I/Os fio completed,
   scaled by runtime/(runtime+ramp); the main thread is idle, so it is the queue-thread cost.
6. `null_blk memory_backed=1` variant: **not run** (time; the null variant answers the framework-cost question).
7. A stale smoke-test shell from an earlier session step was found idle on the host mid-run (it owned no devices
   and had no I/O in flight; it had finished before the real run started) and was killed; no numbers were affected.
8. The loop rows are disk-bound (see below): the local disk delivered only ~6 K IOPS during the run (another
   workload shares the host), so the loop comparison says nothing about framework cost. Treat as inconclusive.

## Gate (ADR-18, fixed in advance)

libublk is adopted if, at 4k randread, numjobs 8, QD 128, `ublk-null` reaches >= 1.0 M IOPS **and** >= 50 % of
`null_blk`, **and** the added QD1 median latency over `null_blk` is <= 15 us.

| measure | null_blk | ublk-null (libublk 0.4.8) | threshold | verdict |
|---|---|---|---|---|
| 4k randread nj8 QD128 IOPS (min-max over 3 reps) | 4 900 347 | 3 829 625 (3 828 430-3 842 144) | >= 1.0 M | met |
| ratio | | 0.78 | >= 0.50 | met |
| 4k randread QD1 nj1, 1 queue, lat p50 | 0.7 us | 3.1 us (**+2.4 us**), p99 9.9 us | <= +15 us | met |
| same, 16-queue config | 0.7 us | 3.1 us | | |

**Outcome: ADR-18 accepted (gate).** The loss at 78 % is the queue threads: the four daemon CPUs are saturated
(0.85 us of daemon CPU per I/O). The kernel selftest server `kublk` was not needed because the gate passed.

## Headline numbers (full tables: `raw/compare-null-16q.md`, `raw/compare-null-1q.md`, `raw/compare-loop.md`)

fio command per cell (`spikes/common/fio-matrix.sh`): `fio --name=cell --filename=<dev> --direct=1 --ioengine=io_uring
--rw=<rw> --bs=<bs> --iodepth=<qd> --numjobs=<nj> --time_based --runtime=5 --ramp_time=1 --norandommap --randrepeat=0
--group_reporting --lat_percentiles=1 --cpus_allowed=4-7 --cpus_allowed_policy=split --output-format=json`.
Matrix `spikes/common/matrices/baseline.txt` (72 cells), 3 reps; raw JSON in `raw/*/json.tar.xz`.

| cell (16 queues) | null_blk IOPS | ublk-null IOPS | ratio | null_blk lat p50/p99 us | ublk lat p50/p99 us | ublk daemon CPU us/IO |
|---|---|---|---|---|---|---|
| 4k randread nj8 QD128 | 4 900 347 | 3 829 625 | 0.78 | 103.9 / 3096.6 | 134.1 / 3129.3 | 0.85 |
| 4k randread nj8 QD32 | 4 896 096 | 3 866 411 | 0.79 | 25.7 / 32.6 | 33.0 / 3031.0 | 0.84 |
| 4k randread nj1 QD32 | 1 217 194 | 978 373 | 0.80 | 26.0 / 33.0 | 32.6 / 38.7 | 0.82 |
| 4k randwrite nj8 QD128 | 4 691 779 | 3 616 265 | 0.77 | 108.0 / 3129.3 | 142.3 / 3162.1 | 0.82 |
| 64k randread nj8 QD32 | 2 488 177 | 2 532 125 | 1.02 | 50.9 / 3063.8 | 99.8 / 130.6 | 1.58 |
| 64k write nj8 QD32 | 2 434 786 | 991 563 | 0.41 | 52.0 / 3063.8 | 261.1 / 321.5 | 4.02 |
| 64k write nj8 QD128 | 1 711 733 | 403 534 | 0.24 | 297.0 / 3358.7 | 2572.3 / 2998.3 | 9.92 |
| 1M read nj8 QD32 | 152 028 | 25 254 | 0.17 | 1679 / 2540 | 10158 / 10158 | 158 |

Observations:

- Small I/O: ublk costs ~0.8 us of daemon CPU per 4k I/O (copy mode); with 4 daemon CPUs the ceiling is ~3.8 M IOPS.
- Large I/O: the copy path dominates (64k write 4-10 us/IO, 1M 160-190 us/IO) and ublk reaches 17-41 % of null_blk
  at 64k-1M with 8 jobs. This is the copy/USER_COPY question for P0.2.
- DESIGN section 20 target (4k random read >= 500 K IOPS, p99 <= 150 us on cache hits at 8 queues): the `ublk-null`
  ceiling is 978 K IOPS at nj1 QD32 with p99 38.7 us (one job, one queue), and 3.8 M IOPS with 4 daemon CPUs; so the
  framework leaves ~2x (one queue) to ~7x (four) headroom for the product's own work. p99 at QD >= 32 with 8 jobs is
  dominated by the shared-CPU time-slice artefact described above. The target is measured at 4 active queues, not 8.
- Loop (16 GiB, O_DIRECT file, reduced matrix, `raw/compare-loop.md`): kernel loop and ublk-loop both deliver
  ~5.8-6.0 K IOPS (4k, any QD) and ~290-350 MB/s at 64k: **the disk was the limit**, ratios 0.86-1.04 are all within
  noise. Two cells (4k write QD32/128, ratios 3.2 and 12.8) show ublk-loop ahead, which was not investigated
  (the disk is the limit and plain `write` cells wrote to an already-full file); do not read them as a ublk advantage. Inconclusive for framework overhead; the null pair carries the conclusion.

## Versions

libublk 0.4.8, io-uring 0.7.16, fio 3.41, rustc 1.96.0, clap 4.6.7, anyhow 1.0.104. 0.5.0 not published.
Module handling: `ublk_drv` and `null_blk` were loaded by this session; `null_blk` was removed after each use, `ublk_drv` could not be (see the P0.3 results).
