# M15 results: phase 1 exit tests (DRAFT, 2026-11-18)

Status: fio verify and ratios are measured on the merged build (`03b858e`); the crash-point reruns and the 4096-byte-sector xfstests references are still running and
are marked pending below. Everything else below was measured. Raw results (results.jsonl, results.md, stall tables,
xfstests logs and the comparison, ratio JSON) are under `raw/`; the scripts are `tests/exit/*.sh`, the fio job files
`bench/phase1/exit/fio/*.fio`, the tables are made by `summarize.py` and `xfstests-compare.py`.

## Host (benchmark hygiene)

16 vCPUs (AMD EPYC), 30 GiB, Ubuntu 26.04, kernel 7.0.0, ext4 root on local NVMe, no cpufreq exposed (virtual machine),
fio 3.41, e2fsprogs 1.47.2, xfsprogs 6.18.0, xfstests `d07dca72` (2026-10-15). Daemon on CPUs 8-11 (4 queues, `journal_max`
512 MiB), fio on 12-15; the host was shared (other agents on 0-7). Release builds of the branch `p1/m15` (main at `fe151c2`
for the runs below; `6d16fbf` for the rerun). Exact fio job files: `bench/phase1/exit/fio/`.

## Summary of the gates

| item | state | verdict |
|---|---|---|
| 1 fio verify | 12 cells on `03b858e` | **PASS** (zero errors, fsck clean, peak store 64 GiB); settled store bound not shown by these cells |
| 2 xfstests | all six runs complete; comparison below | ext4: no Lunix-only failure but the reference fails 244 of 684 for an environment reason (needs a fix before it means anything); xfs: 3 Lunix-only (xfs/129, 234, 253), one reference hang excluded (xfs/118) |
| 3a kill -9 loops | ext4 100 cycles, xfs 50 cycles | **PASS** |
| 3b crash points | 79 points, 3 hits each (rerun on `03b858e`) | 211 PASS, 0 FAIL; 69 of 79 points reached and clean; 10 points never executed by the workload (listed), tamper step missing |
| 3c QUIESCE handover x20 (SIGUSR2) | 20 cycles | **PASS** |
| negative controls | M10 noreissue and fault, corrupted block, inflated fsyncer ack, dropped writes | **PASS** (all five tripped) |
| 4 ratios | both chunk sizes | **PASS** (table below) |
| leftovers | clean after every run (`raw/*/leftovers-end.txt`) | clean |

## 3a, 3c: kill -9 and handover loops (4 queues, 64 KiB chunks, integrity)

Every phase: fio errors 0, no verify failure, guest fsck clean, acknowledged fsyncs all verify, `lunix fsck --deep` clean
after the detach.

| run | phase | cycles | daemon exec to recovered ms p50 / p99 / max | kill to ready ms p50 / max | heartbeat stall ms p50 / p99 / max |
|---|---|---|---|---|---|
| ext4 | immediate | 50 | 478 / 3009 / 3009 | 536 / 3098 | 0 / 3466 / 3466 |
| ext4 | restart delay 1 s | 25 | 896 / 3739 / 3739 | 1952 / 4798 | 0 / 2452 / 2452 |
| ext4 | restart delay 10 s | 10 | 1255 / 1455 / 1455 | 11315 / 11515 | 0 / 11501 / 11501 |
| ext4 | double kill | 15 | 1571 / 5359 / 5359 | 1890 / 5652 | 1759 / 5749 / 5749 |
| xfs | immediate | 25 | 1946 / 2757 / 2757 | 2052 / 4018 | 0 / 6985 / 6985 |
| xfs | restart delay 1 s | 12 | 454 / 918 / 918 | 1509 / 2021 | 802 / 1565 / 1565 |
| xfs | restart delay 10 s | 5 | 509 / 1675 / 1675 | 10559 / 11740 | 1861 / 11736 / 11736 |
| xfs | double kill | 8 | 857 / 1903 / 1903 | 1196 / 2377 | 1713 / 2364 / 2364 |
| ext4 | QUIESCE handover (SIGUSR2) | 20 | 410 / 1333 / 1333 | 4496 / 7949 | 3197 / 5426 / 5426 |

## Negative controls

N1 the two M10 controls (no REISSUE, dropped writes) through the test daemon: both tripped. N2 one 4 KiB block overwritten in
a verified region: `fio --verify_only` failed. N3 the acknowledged index of the fsyncer raised: the verify reported bad
blocks. N4 (new full-stack control) `LUNIX_UBLK_FAULT_DROP_WRITES` (file-armed hook in the crashpoints build, writes are
acknowledged and dropped): a verified write failed; without the file it passed.

## 4 ratios (one 4 GiB cell each, raw device, bs = chunk size)

Measured dedup is the `lunix fsck --deep` recount; "nominal" is 1/(1-p). The data read back from the device
(`chunkstat.py`) agrees with fsck to four digits, so fio's data is what the knobs say. Gate: dedup within 10 % of nominal,
compression within 15 % of 1/(1-c), random data stored at most 1.01x: **met at both chunk sizes**.

| cell | dedup nominal | dedup 16 KiB | dedup 64 KiB | compression nominal | compr 16 KiB | compr 64 KiB |
|---|---|---|---|---|---|---|
| c0 d0 (random) | 1.00 | 1.000 | 1.000 | 1.00 | 1.000 | 1.000 |
| c0 d50 | 2.00 | 2.005 | 2.008 | 1.00 | 1.000 | 1.000 |
| c0 d90 | 10.00 | 9.978 | 9.909 | 1.00 | 1.000 | 1.000 |
| c50 d0 | 1.00 | 1.000 | 1.000 | 2.00 | 1.973 | 1.977 |
| c50 d50 | 2.00 | 2.005 | 2.008 | 2.00 | 1.973 | 1.977 |
| c50 d90 | 10.00 | 9.978 | 9.909 | 2.00 | 1.973 | 1.977 |
| c90 d0 | 1.00 | 1.000 | 1.000 | 10.00 | 9.472 | 9.571 |
| c90 d50 | 2.00 | 2.005 | 2.008 | 10.00 | 9.472 | 9.571 |
| c90 d90 | 10.00 | 9.978 | 9.909 | 10.00 | 9.472 | 9.570 |
| Linux 6.12 source tarball on ext4 (1.69 GiB written) | | 1.004 | 1.002 | | 5.447 | 6.111 |
| root file system copy (`/usr`, 5.9 GiB) | | 1.028 | 1.011 | | 2.699 | 2.827 |
| random data, stored / logical | | 1.0063 | 1.0016 | | | |

Store sizes (segments + metadata, du of the volume directory including the 0.5 GiB journal ring) are in `raw/m15-ratios-out`.

**GC settle (rerun `ratios2`, binaries of main `03b858e`, before pace, jwin and tipstat):** every cell waited 120 s after the write
(past the 60 s grace) and recorded `lunix gc --status`: in all 24 cells the collector is idle (state none, 0 condemned pending, 4-19
passes done) and the store is the live data plus the 0.5 GiB journal ring (for example c64 random: segments 4102 MiB, store 4627 MiB;
c64 c90 d90: segments 43 MiB, store 565 MiB; the same figures before and after the settle), i.e. 1.0 x live, inside the 1.3 x bound.
The ratio table above is from `ratios2` as well (both chunk sizes, same figures as the first run to the third digit).

## 2 xfstests (`-g quick -x dangerous`, 16 GiB TEST and SCRATCH; references = loop devices with 4096-byte sectors, `losetup --sector-size 4096`)

Details and per-test reasons: `xfstests-compare.md`; raw `raw/m15-xt2-*`, `raw/m15-xt3-*`. GNU coreutils go first in PATH (the host's
Rust coreutils break the tests); xfs/118 hung on the reference and is excluded on both sides.

| run | reference ran / failed | Lunix ran / failed | both pass | both fail | Lunix only | passes on reference, not on Lunix |
|---|---|---|---|---|---|---|
| ext4 64 KiB | 404 / 14 | 404 / 14 | 390 | 14 | **0** | 0 |
| ext4 16 KiB | 404 / 14 | 404 / 14 | 390 | 14 | **0** | 0 |
| xfs 64 KiB, 16 KiB | 751 / 23 | rerun with GNU PATH in progress (`xt4-*`) | | | | |

- **ext4: gate met.** Against the 4096-byte-sector reference, every test passes on Lunix that passes on the reference, the 14 failures are
  the same 14 on both sides (host environment: quota tools on tmpfs, no `newring`, statfs permission, stripe geometry, ...; and ext4/054,
  which fails on the reference too once the sector size is 4096: `mkfs.ext4 -b 1024` needs a 512-byte device), and the 7 tests that did
  not run on Lunix against the 512-byte reference (1 KiB blocks, 512-byte alignment) do not run on the 4096-byte reference either.
  The uutils error is absent. 16 KiB and 64 KiB chunks give the same result.
- **xfs/129, 234, 253** fail on the 4096-byte-sector reference exactly as on Lunix (`xfs_mdrestore` on a 4096-byte-sector device): explained,
  not Lunix defects. The 26 xfs tests that do not run on Lunix against the 512-byte reference (`block size 1024 cannot be smaller than sector
  size 4096`, ...) also do not run on the 4096-byte one (751 ran on both).
- **xfs, Lunix-only against the 4096-byte reference (first comparison, before the rerun): generic/294, 321, 634, 635, xfs/103, 132, 148.**
  These are an environment mismatch of the comparison, not yet findings: the Lunix xfs runs of that comparison were made *before* the GNU
  coreutils PATH fix (generic/294, 634, 635 print Rust-coreutils errors: `mknod: File exists (os error 17)`, `touch: Unable to parse date`), the
  4096-byte reference after it. The two Lunix xfs runs are being repeated with the fix (`xt4-l64-xfs`, `xt4-l16-xfs`); whatever still differs then is a
  Lunix-only failure to classify (xfs/132 printed `Structure needs cleaning` and xfs/148 is a metadata-corruption test, both worth a close look
  if they persist).

## 3b crash points (rerun `cp2`, binaries of main `03b858e`, before pace, jwin and tipstat)

The merged build lists **79 crash points**. A 120 s counting run (the daemon writes `LUNIX_CRASH_COUNT` on its quiesce exit) gives the
executions per point; each reached point is run at hit 1, the middle hit and 80 % of the count (60 % if that is not reached), on ext4,
and on xfs for the `engine.c*` points (3 hits each). Every run: churn workload (fio verify with backlog, two fsyncers, `lunix gc` every
15 s, `commit_interval_ms` 2000), the daemon kills itself at the hit, a supervisor restarts it in recovery mode, 30 s more load, then
fio zero errors (data byte-exact: crc32c verify of everything it wrote), guest fsck, fsyncers (every acknowledged write present),
detach and `lunix fsck --deep` (this verifies every extent's tag or AEAD and every chain link). Table per point:
`crashpoints-table.md`; raw results `raw/m15-cp2-*`.

| result | runs |
|---|---|
| PASS (crash happened, every check clean) | 211 (181 ext4, 30 xfs) |
| FAIL | **0** (no recovery failure, no fsck finding, no fio or fsyncer error) |
| UNREACHED | 15 (5 are variants of reached points, 10 are points the workload never executes) |

Recovery stall after a crash (daemon exec to recovered): p50 395 ms, p99 1801 ms, max 2303 ms (197 runs).
**Every point that the workload reaches (69 of 79) crashed and recovered clean at least once, and at every reached hit that fired.**

**Not reached, and why (10 points, count 0 in the counting run):**

| points | why the churn workload never executes them | what would reach them |
|---|---|---|
| `gc.urgent.after_mark` | the urgent GC pass runs only when the space ladder meets NoSpace; the volume never fills | a volume sized to fill, `on_full = wait`, and a writer |
| `store.leftover.before_unlink`, `.after_unlink` | removal of a leftover temporary file at open | a kill that leaves a `.tmp` or `.open` file, then a restart |
| `store.segment_delete.before_unlink`, `.after_unlink`, `.after_dirsync` | deletion of an orphan segment by the open-time sweep or by fsck repair, not by GC (GC uses `store.delete.*`, which are reached) | a crash after a sealed, never-named segment, then a restart; or `lunix fsck --repair --delete-orphans` under the crashpoints build |
| `store.set_aside.before_rename`, `.after_rename`, `.after_dirsync` | moving a damaged tip aside, an offline `fsck --repair --truncate-tip` | a damaged tip and a crashpoints build of `lunix-fsck` |
| `ublk.after_update_size` | the online resize path | `lunix resize` under load |

These ten need dedicated scenarios (a resize under load, a full volume, an fsck repair of a damaged copy); they are **not covered** by
this run and are listed as open for the final report. The 5 unreached variants (`destage.after_segment_append` last, `engine.c1.waited`
last, `engine.c7.floor` last, `index.before_manifest_swap` last, `store.pack.after_dirsync` mid) are reached points whose other hits passed: the count of
one run was not repeated in the crash run (variance of 10-20 %).

**The bar** the lead set (data byte-exact, tampering detected in integrity mode): byte-exactness is what fio verify and the fsyncers check
in every cell. Tamper detection is *not* exercised by the crash cells: `lunix fsck --deep` verifies tags (so a damaged copy would show),
and the negative control N2 shows that a corrupted block is caught on a live volume, but no crash cell flips a byte of a segment after
the crash and expects `NeedCredential`/`BadMac`-style refusal. That is a gap against the stated bar, to be covered by a tamper step in
the cp cells (flip one byte of a named segment after the daemon died, expect fsck error and an attach refusal), not yet written.

## 1 fio verify (rerun on the merged build): PASS in all 12 cells

**Build note:** these runs (and `ratios2`) used binaries of main `03b858e` (bstop, bstop-fu, gcrate merged), i.e. **before the later
`pace`, `jwin` and `tipstat` changes**. A rerun on the final main is needed if those change the I/O path.

Per cell (target x chunk x mode): 6 profiles (QD 1/32/128 x 1 and 8 jobs, 4 GiB each, 600 s, bssplit 4k-1M, crc32c with state save,
trim companion, discard and write-zeroes by `trimcheck.py`), then detach, attach, `fio --verify_only` of every profile, detach.
For every cell and every profile: fio exit 0 with zero job errors (zero verify failures), `trimcheck` clean; guest fsck clean after the
runs and after `verify_only` (ext4, xfs); `lunix fsck --deep` clean after each detach; every cell produced its result record. About
68 min per cell. **Zero failures.**

| target | chunk | mode | peak store GiB (after a profile) | store after the runs / after verify_only and detach GiB | live data GiB (fsck) | store / live (fsck categories) | GC passes (after p0 -> after p5) |
|---|---|---|---|---|---|---|---|
| raw | 64 K | integrity | 63 | 63.2 / 55.9 | 25.8 | 2.43 | 46 -> 123 |
| ext4 | 64 K | integrity | 63 | 63.5 / 56.5 | 30.8 | 2.04 | 32 -> 174 |
| xfs | 64 K | integrity | 63 | 64.0 / 62.2 | 31.1 | 2.04 | 34 -> 188 |
| raw | 64 K | encrypted | 64 | 64.3 / 55.7 | 25.6 | 2.49 | 46 -> 140 |
| ext4 | 64 K | encrypted | 64 | 64.3 / 63.7 | 30.8 | 2.07 | 34 -> 150 |
| xfs | 64 K | encrypted | 64 | 64.0 / 59.4 | 31.1 | 2.04 | 37 -> 184 |
| raw | 16 K | integrity | 43 | 43.7 / 35.1 | 25.5 | 1.69 | 37 -> 127 |
| ext4 | 16 K | integrity | 51 | 51.8 / 44.0 | 29.9 | 1.71 | 32 -> 108 |
| xfs | 16 K | integrity | 60 | 60.6 / 55.2 | 30.9 | 1.94 | 32 -> 102 |
| raw | 16 K | encrypted | 43 | 42.8 / 34.8 | 25.5 | 1.66 | 36 -> 118 |
| ext4 | 16 K | encrypted | 54 | 54.6 / 48.1 | 30.7 | 1.76 | 31 -> 105 |
| xfs | 16 K | encrypted | 59 | 59.3 / 50.5 | 30.7 | 1.91 | 30 -> 96 |

Every cell stayed under the 80 GiB store cap and the plan's 100 GiB (the volume's store never passed 64 GiB; the first run without
GC filled 245 GB, the second, with the old GC schedule, passed 80 GiB after two profiles). The collector state and the space
ledger after every profile are in `raw/m15-fv3-*/raw/*/gc-status-p*.json` and `status-p*.json`.
**The store bound is not shown by these cells:** the cells end under write load and do not wait out a final pass and grace, so
"store after the runs" is about 1.7-2.5 x live data, above the 1.3 x of the M17.B oracle (which applies after the final pass and grace).
The settled figure comes from the ratio cells below (nothing left to collect) and from a final pass in the next fio-verify rerun
(a `lx gc` and a 100 s wait before the last detach, not in this script yet).

The earlier stall, kept for the record: the first fio-verify run (build without the collector, main `fe151c2`) stalled in profile 5 at
07:45Z on a **full volume with no collector in the build**: space used 245.5 GB against a limit of 245.1 GB, one waiter in the space gate,
128 requests in flight, every daemon thread waiting; the disk fell to 49 GB free and M15 held more than 100 GiB. It was cleaned up
(the device deleted after its daemon was gone, W1a) and nothing was left behind. The rerun on `6d16fbf` stopped at the cap in profile 1 (a pass
waited out the 60 s grace in the middle); `p1/gcrate` fixed that.

## Not done / pending, in order

1. fio verify: a final GC pass and settle before the last detach, to show the 1.3 x store bound at the end of a cell (the cells above end under load).
2. Crash points: the 13 failed cells, the 25 UNREACHED variants and the 13 new points, on the merged build.
3. xfstests: the ext4 environment error, then both ext4 sides; the sector-size check for xfs/129, 234, 253.
4. Ratios and kill loops are done; a GC pass per ratio cell for the store bound is part of the rerun with the merged build.
