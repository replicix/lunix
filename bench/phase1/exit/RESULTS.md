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
| 3b crash points | 79 points, 3 hits each (rerun on `03b858e`) plus scenarios | 211 PASS, 0 FAIL; 69 of 79 reached by the workload, the other 10 reached by scenarios (10 of 10 clean) |
| 3d tamper | 6 cells (2 chunk sizes x segment, commit, map) | **PASS**: fsck exit 2 with an authentication code, attach refused or reads fail, 0 wrong bytes |
| finding | write stall after kill -9 and recovery (journal ring, nothing moves) | **open, see Finding** |
| ADR-32 T7 (M15) | full-volume cell and cache_max cells | full-volume cell PASS (see 4'); fio-verify `cache_max` cells not yet run |
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
| xfs 64 KiB | 751 / 23 | 751 / 23 | 728 | 23 | **0** | 0 |
| xfs 16 KiB | 751 / 23 | 751 / 23 | 728 | 23 | **0** | 0 |

- **ext4: gate met.** Against the 4096-byte-sector reference, every test passes on Lunix that passes on the reference, the 14 failures are
  the same 14 on both sides (host environment: quota tools on tmpfs, no `newring`, statfs permission, stripe geometry, ...; and ext4/054,
  which fails on the reference too once the sector size is 4096: `mkfs.ext4 -b 1024` needs a 512-byte device), and the 7 tests that did
  not run on Lunix against the 512-byte reference (1 KiB blocks, 512-byte alignment) do not run on the 4096-byte reference either.
  The uutils error is absent. 16 KiB and 64 KiB chunks give the same result.
- **xfs/129, 234, 253** fail on the 4096-byte-sector reference exactly as on Lunix (`xfs_mdrestore` on a 4096-byte-sector device): explained,
  not Lunix defects. The 26 xfs tests that do not run on Lunix against the 512-byte reference (`block size 1024 cannot be smaller than sector
  size 4096`, ...) also do not run on the 4096-byte one (751 ran on both).
- **xfs: gate met.** The 7 tests that looked Lunix-only (generic/294, 321, 634, 635, xfs/103, 132, 148) came from an environment mismatch of the
  first comparison: that Lunix run was made before the GNU coreutils PATH fix, the 4096-byte reference after it. Their failing text was the Rust
  coreutils' (`mknod: File exists (os error 17)`, `touch: Unable to parse date`, `ls: general io error: ... (os error 117)`, `ln: ... Permission denied`
  instead of `Operation not permitted`, and `touch: ... Structure needs cleaning` instead of `Input/output error` in xfs/132, whose expected
  output also differs by tool). Rerun with the fix (`xt4-l64-xfs`, `xt4-l16-xfs`): **0 Lunix-only failures, 0 stalls, the uutils messages absent**
  (23 failures on both sides, the same 23).

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

**The 10 points the churn workload never executes (count 0) were reached by dedicated scenarios, all with the daemon or `lunix-fsck` killed at
the point and the volume recovered: 10 of 10 reached, 0 failures.**

| point(s) | scenario (script) | result |
|---|---|---|
| `gc.urgent.after_mark` | a cap (3G) too small for the working set plus its garbage, `on_full = wait`; the urgent pass runs when a write meets no space (`crashpoints.sh SCENARIO=full`) | crashed after 42 s, recovery clean (a first attempt with a 1.5G cap left no room even for the journal ring and the headroom; the guest ext4 got ENOSPC on its journal, which was the scenario's fault) |
| `store.leftover.before_unlink`, `.after_unlink`, `.after_dirsync` | the preparing crash at `destage.after_segment_append` leaves an `.open` segment; the restart carries a second crash at the point (`SCENARIO=prep`) | 3 of 3 reached, recovery clean |
| `store.segment_delete.before_unlink`, `.after_unlink`, `.after_dirsync` | reached only by `lunix-fsck --repair --delete-orphans` (not by the open sweep, which uses `store.delete.*`): a real orphan from a daemon killed at `destage.after_segment_seal`, repaired offline by a crashpoints build of `lunix-fsck` killed at the point; the repair runs again (`fsck-repair.sh`) | 3 of 3 reached, the repeated repair finishes and `fsck --deep` is clean (exit 0) |
| `store.set_aside.before_rename`, `.after_rename`, `.after_dirsync` | a damaged tip (one flipped byte), `lunix-fsck --repair --truncate-tip` killed at the point (`fsck-repair.sh`) | 3 of 3 reached; before_rename: the same command again completes; after_rename and after_dirsync: the same command again **refuses** (`repair_refused: --truncate-tip: the chain below the previous commit does not verify`; it would drop a second commit and changes nothing), the plain `--repair` then completes (rewrites the checkpoint that still names the set-aside commit); `fsck --deep` exit 1 (warnings only), the volume attaches |
| `ublk.after_update_size` | `lunix resize` 10 s into the load, daemon killed at the point (`SCENARIO=resize`) | crashed after 13 s, recovery clean |

Note for the repair protocol (not a defect): after a crash of `--truncate-tip` that already renamed the commit, rerun `lunix fsck --repair`
without `--truncate-tip`.

**The bar (data byte-exact, tampering detected in integrity mode).** Byte-exactness: fio verify, the fsyncers and the deep fsck in every cell. Tamper
detection: the new tamper cell, below.

## 3d tamper cell (integrity mode, both chunk sizes, `tamper.sh`)

After a 2 GiB incompressible sequential write (every chunk live) and the detach, `lunix fsck --deep` is clean; then one byte (XOR 0x01) in the middle of
the largest segment (variant segment), of the tip commit (commit) or of the newest map pack (map) is flipped. Required: `fsck --deep` exits 2 with an
authentication finding; attach is refused, or the attached device returns read errors for the damaged blocks and **no block with other bytes**
(SHA-256 of every MiB of the first 2 GiB against the reference).

| chunk | variant | fsck --deep after the flip | attach / read | wrong bytes |
|---|---|---|---|---|
| 64 K | segment | exit 2: `extent_auth`, `seg_hash` | attached; 1 MiB block unreadable (EIO) | **0** |
| 64 K | commit | exit 2: `tip_auth` | attach refused: `tip commit ... does not authenticate` | 0 |
| 64 K | map | exit 2: `extent_auth`, `seg_hash` | attached; every block unreadable (the tip's map root is damaged) | 0 |
| 16 K | segment | exit 2: `extent_auth`, `seg_hash` | attached; 1 block unreadable | 0 |
| 16 K | commit | exit 2: `tip_auth` | attach refused | 0 |
| 16 K | map | exit 2: `extent_auth`, `map_node_auth`, `seg_hash` | attached; 8 blocks unreadable | 0 |

**All six cells meet the bar** (without the crash: `raw/m15-tamper-nokill` plus `raw/m15-tamper-map`, where the largest map pack, picked first, held only
superseded nodes: fsck found the damage but no read touched it, 0 wrong bytes; the newest pack is on the read path and every read failed). With the
crash (a kill -9 8 s into the write and a recovery before the flip; `raw/m15-tamper`, `m15-tamper2`, `m15-tamper3`) every cell that finished its
write passed the same checks; **but the write itself stalled after the recovery in 3 of 6 (first run), 2 of 6 (second) and 1 (third): see "Finding".**

## Finding: the write stalls after a kill -9 and recovery (journal ring wait, then EIO or a hang)

Repro: `tests/exit/tamper.sh` (it kills the daemon 8 s into a 2 GiB, 1 MiB, QD 8 sequential incompressible write on an integrity volume,
4 queues, `journal_max` 512 MiB, restarts it, and lets fio finish). In roughly a third to a half of the cells fio makes **no progress after the
recovery** and fails with EIO after `full_timeout` (60 s): `write failed: journal ring full ... last_commit_error="other"`; in the third run one
cell did not end at all (900 s; the daemon aborted when `gdb` attached to capture stacks, so the stack capture of that run must not attach
again to a live daemon, or the script must expect that).
Evidence (`raw/m15-tamper2/raw/tamper_c64k_segment/status-trace.txt`, `raw/m15-tamper3/raw/tamper_c64k_commit/stall-*`): `bytes_written`, the tip
(`tip` 17 or 18), `commits` and `journal_used` (134 217 728 = 128 MiB = one lane's two ring files) are **constant for 60 s**; `dirty_leaves` 0; the
space ledger is far from its limit (1.2 GB of 17 GB); every daemon thread is idle: `lunix-coord` waits in `Condvar::wait_timeout` at
`engine.rs:425`, the journal flusher in its condvar, the queue threads in `io_uring_enter`. `Coord::wanted` asks for a cut when records are
dirty, or `journal.fill().ratio() > journal_fill_trigger` (aggregate, here 0.25), or the `journal_full` flag is set: a **single** lane whose ring is
full of old (replayed) records, with nothing new issued because writes are parked on that ring, satisfies none of them. That is the hypothesis; the
status trace and stacks are in the raw directories. Not in the crash-loop cells before (their writers are rate limited, not saturating one lane).

## 4' ADR-32 T7, the M15 parts (research note 2026-11-18 section 9.4)

Implemented in the harness (`fio-verify.sh`, `fullvol.sh`), run on `03b858e` plus the merged main of 2026-11-19 (T3 to T5 of ADR-32 are not all merged: the
status fields `delayed_ms`, `delayed_writes`, `delay_max_ms`, `enospc`, `throttle.state` are read when present and the cell says so when they are not):

- every fio-verify cell sets `cache_max` (default 48G for the 32 GiB volumes; run the 4 GiB-profile raw cell also with `CACHE_MAX=24G`), samples the
  store directory every 10 s and requires it to stay under cache_max + headroom + journal ring + 1 GiB, no ENOSPC (job errors, the `enospc`
  counter), fio's longest completion under `full_timeout`, no hung-task line; per-cell `delayed_ms`, `delayed_writes`, `delay_max_ms`;
- the full-volume cell (`fullvol.sh`, run `fullvol4`/`fullvol5`): cap 8G below a 16 GiB incompressible working set. Results: fio saw ENOSPC, the longest write
  completion 5.0 s (full_timeout 5 s + 5 s allowed), no hung-task line, the ledger at the limit (the throttle state field is absent in this build), `blkdiscard` of
  half the device brought writes back after 42 s, SIGTERM with writes parked exited in 2-4 s (quiesce_timeout_ms 30 s), kill -9 with writes parked,
  `space.on_full = "error"` written to volume.json, restart: fio ended with errors, the device answered (a write failed with ENOSPC at once, reads worked), **no forced device
  deletion**, `lunix fsck --deep` clean. The "writes are parked at the cap" check passed in 2 of 3 runs (inside the band writes are only paced, at 4 MiB/s near the hard
  mark; whether they park within 400 s varies). What the guest file systems do with the error: **ext4** aborts its journal and remounts read-only (`dd: Read-only file
  system`, `Buffer I/O error`, `failed to convert unwritten extents`; guest fsck rc 4 = needs repair), **xfs** returns ENOSPC to the writer and stays mounted
  read-write (guest fsck clean). Both recorded in `raw/m15-fullvol5/raw/fullvol/fs-*`.

**M16 parts (§9.5) that need the M16 scripts, not done here:** (1) the open path costs nothing: 4 KiB random-write and 1 MiB ingest cells before and after
on the same host (a before/after pair of the Part A cells); (2) a §20 cell is valid only if not throttled: `delayed_ms` and `delayed_writes` zero for the cell
or it is reported "throttled; not a §20 measurement", and each write cell sets `cache_max` to its garbage room (about 70 GiB for the 4 KiB random-write cell, about
170 GiB for a 60 s rewrite at 3 GB/s); (3) one sustained cell, 4 KiB random write for 15 min on a 4 GiB working set with a 48 GiB cap, reporting steady IOPS,
p99, p99.9, max latency, the store's maximum, GC write amplification and time per gate state, compared with `(cap - live store) / tau`; (4) the §20 sentence
(a decision for the maintainer, not a script).

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
