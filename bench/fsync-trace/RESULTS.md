# fsync-trace: what the block device sees when file systems fsync

Host: AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root.
Kernel `7.0.0 (Ubuntu)`, Ubuntu 26.04 LTS. Tools: mkfs.ext4/xfs/btrfs from the distro, python3 3.x, cc; load generators are our own (`spikes/fsync-trace/`), no fio.
All file systems created with default mkfs/mount options (ext4: `lazy_itable_init=0,lazy_journal_init=0` only, so the lazy-init thread does not pollute the trace). No packages were installed.

## Method and deviations
- **Device: `scsi_debug`, not brd.** brd is bio-based and write-through (`write_cache=write through`, no `mq/`): no `block_rq_*` events and file systems never send flushes to it. `scsi_debug dev_size_mb=4096 dpofua=1 delay=0 ndelay=0` is blk-mq, reports write-back cache and FUA (`queue/fua=1`), as a ublk device with volatile cache + FUA would. Loaded and removed by this task. Loop was not used because it does not advertise FUA. 4 GiB RAM-backed.
- Tracing: private tracefs instance, events `block_bio_queue` (what the FS asked: PREFLUSH/FUA flags), `block_rq_issue`/`block_rq_complete` (what the driver sees), filtered to the device's dev_t, mono clock, read via `trace_pipe` pinned to CPUs 8-15. Global tracing state untouched.
- Load: all on CPUs 8-15, 20 s per run, 3 reps (s2p: 45 s; s4/s4b/s6 have a 4th 6 s run from a late pass to capture sequences; it is included in those means). Kernel writeback threads are not pinned (sysctls untouched).
- **Streaming writer** (s2*, s3e): buffered 1 MiB `pwrite`s at 200 MB/s (s2a, s3e) or 1 GB/s (s2b; achieved ~1.0 GB/s) cycling over a pre-written 1 GiB file (pure overwrite, no allocation), never fsync. It asks for write-behind with `sync_file_range(SYNC_FILE_RANGE_WRITE)` 8 MiB behind the writer. **Without it nothing reached the device inside a 20 s window** (default dirty thresholds/30 s expiry; seen in a trial run), so s2a/s2b are "steady writeback" and s2p is the unaided kernel (45 s run, bursty). Streams are overwrites, so ext4/xfs put no allocation metadata in the journal.
- Flush points = bios with PREFLUSH or FUA at `block_bio_queue`. Bytes = completed non-FUA write requests between consecutive points (FUA writes are durable on their own and excluded). Attribution via `filefrag -v -b512` extents after the run; "other" = journal, metadata, unmapped. **btrfs attribution is unreliable** (CoW: the final map is not the map at write time, so btrfs `small`/`stream` columns are wrong or empty; only btrfs totals and rates are meaningful). For s6 the "small/db" column is the database file and "stream/wal" the WAL.
- At the device, a PREFLUSH|FUA bio is split by the block layer into an empty FLUSH request followed by a FUA write (the device has FUA, so no post-flush). Empty FLUSH rwbs shows as `FF` in rq events, FUA writes as `WF..`, PREFLUSH bio as leading `F` (`FWFSM` = PREFLUSH+FUA+SYNC+META).
- Scenario 5 sequences come from steady-state cycles of the s1 run (and s4/s6); in the `n/a` cells there were no PREFLUSH bios.
- A bare `sync` in my script blocked on another agent's stuck ublk file system (D state); replaced by `sync -f <my mount>`. That one `sync` process (started by this task, ~01:16) is still stuck in D state on the other agent's device and could not be removed; it holds nothing of mine and will exit when that device unblocks. The affected runs were repeated.

## Scenarios
s1 4 KiB append+fsync 50/s; s2a/s2b s1 + stream 200 MB/s / 1 GB/s; s2p s1 + unaided buffered stream 200 MB/s; s3 fdatasync alone; s3e fdatasync + 200 MB/s stream; s4 `O_DIRECT|O_DSYNC` 4 KiB random overwrite of a preallocated, zero-filled 64 MiB file, 200/s; s4b `O_DIRECT|O_DSYNC` appends (allocating) 200/s; s6 SQLite WAL, `synchronous=FULL`, one INSERT/txn, 100/s (python3 sqlite 3.46.1).

## Rates (mean over reps; /s)
Bio columns are what the FS submitted; rq columns what the driver saw. "FUA rq w/o preceding FLUSH rq" is the total count per run (a FUA write not directly preceded by a FLUSH request).

| scen | fs | reps | PF+FUA bios/s | PF-only bios/s | FUA-w/o-PF bios/s | FLUSH rq/s | FUA wr rq/s | FUA rq w/o preceding FLUSH rq | write MB/s |
|---|---|---|---|---|---|---|---|---|---|
| s1 | btrfs | 3 | 0.0 | 50.0 | 50.0 | 50.0 | 50.0 | 0 | 7.5 |
| s1 | ext4 | 3 | 50.0 | 0.0 | 0.1 | 50.0 | 50.0 | 1 | 1.3 |
| s1 | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 0 | 0.3 |
| s2a | btrfs | 3 | 0.0 | 50.0 | 50.0 | 50.0 | 50.0 | 5 | 205.1 |
| s2a | ext4 | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 2 | 200.9 |
| s2a | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 2 | 199.9 |
| s2b | btrfs | 3 | 0.0 | 50.1 | 50.1 | 50.1 | 50.1 | 20 | 838.4 |
| s2b | ext4 | 3 | 49.9 | 0.0 | 0.1 | 49.8 | 49.7 | 7 | 992.6 |
| s2b | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 49.9 | 11 | 997.2 |
| s2p | ext4 | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 2 | 53.6 |
| s2p | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 0 | 24.2 |
| s3 | btrfs | 3 | 0.0 | 50.0 | 50.0 | 50.0 | 50.0 | 0 | 7.5 |
| s3 | ext4 | 3 | 50.0 | 0.0 | 0.1 | 50.0 | 50.0 | 1 | 1.3 |
| s3 | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 0 | 0.3 |
| s3e | btrfs | 3 | 0.0 | 50.0 | 50.0 | 50.0 | 50.0 | 3 | 205.0 |
| s3e | ext4 | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 2 | 201.0 |
| s3e | xfs | 3 | 50.0 | 0.0 | 0.0 | 50.0 | 50.0 | 0 | 199.9 |
| s4 | btrfs | 4 | 0.0 | 199.3 | 197.6 | 199.0 | 196.8 | 1 | 29.6 |
| s4 | ext4 | 4 | 0.2 | 0.0 | 200.0 | 0.2 | 200.2 | 3300 | 0.8 |
| s4 | xfs | 4 | 0.0 | 0.0 | 200.0 | 0.0 | 200.0 | 3300 | 0.8 |
| s4b | btrfs | 4 | 0.0 | 199.3 | 197.2 | 199.3 | 197.2 | 0 | 30.1 |
| s4b | ext4 | 4 | 199.9 | 0.1 | 0.2 | 199.9 | 200.0 | 2 | 6.6 |
| s4b | xfs | 4 | 200.0 | 0.0 | 0.0 | 200.0 | 200.0 | 0 | 1.2 |
| s6 | btrfs | 4 | 0.0 | 100.7 | 100.5 | 100.7 | 100.5 | 0 | 14.9 |
| s6 | ext4 | 4 | 59.3 | 41.4 | 0.0 | 100.6 | 59.3 | 0 | 2.3 |
| s6 | xfs | 4 | 59.3 | 41.4 | 0.0 | 100.6 | 59.3 | 0 | 1.0 |

## Bytes completed since the previous flush point (any PREFLUSH or FUA bio)

KiB completed (non-FUA writes) between consecutive flush points; p50 / p90 / max (p50,p90 = median over reps, max = max over reps)
| scen | fs | total | small/db | stream/wal | other (journal+meta) |
|---|---|---|---|---|---|
| s1 | btrfs | 0.0 / 164.0 / 260.0 | 0.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 160.0 / 256.0 |
| s1 | ext4 | 20.0 / 24.0 / 24.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 16.0 / 20.0 / 20.0 |
| s1 | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s2a | btrfs | 1024.0 / 4228.0 / 6916.0 | 0.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 4096.0 | 64.0 / 4224.0 / 6912.0 |
| s2a | ext4 | 4116.0 / 4116.0 / 4124.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 16.0 / 16.0 / 24.0 |
| s2a | xfs | 4100.0 / 4100.0 / 4100.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 0.0 / 0.0 / 0.0 |
| s2b | btrfs | 1024.0 / 16452.0 / 28856.0 | 0.0 / 4.0 / 788.0 | 0.0 / 15388.0 / 17444.0 | 32.0 / 16420.0 / 24368.0 |
| s2b | ext4 | 19476.0 / 19484.0 / 38936.0 | 4.0 / 4.0 / 8.0 | 19456.0 / 19456.0 / 38912.0 | 16.0 / 24.0 / 24.0 |
| s2b | xfs | 19460.0 / 19460.0 / 38916.0 | 4.0 / 4.0 / 4.0 | 19456.0 / 19456.0 / 38912.0 | 0.0 / 0.0 / 0.0 |
| s2p | ext4 | 24.0 / 4124.0 / 12316.0 | 4.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 12288.0 | 20.0 / 24.0 / 36.0 |
| s2p | xfs | 4.0 / 4.0 / 8524.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 8520.0 | 0.0 / 0.0 / 62.5 |
| s3 | btrfs | 0.0 / 164.0 / 260.0 | 0.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 160.0 / 256.0 |
| s3 | ext4 | 20.0 / 24.0 / 32.0 | 4.0 / 4.0 / 8.0 | 0.0 / 0.0 / 0.0 | 16.0 / 20.0 / 28.0 |
| s3 | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s3e | btrfs | 72.0 / 4228.0 / 8676.0 | 0.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 4496.0 | 64.0 / 4224.0 / 8368.0 |
| s3e | ext4 | 4120.0 / 4120.0 / 4124.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 20.0 / 20.0 / 24.0 |
| s3e | xfs | 4100.0 / 4100.0 / 4100.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 0.0 / 0.0 / 0.0 |
| s4 | btrfs | 34.0 / 164.0 / 1760.0 | 0.0 / 52.0 / 264.0 | 0.0 / 0.0 / 0.0 | 16.0 / 162.0 / 1760.0 |
| s4 | ext4 | 0.0 / 0.0 / 8.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 8.0 |
| s4 | xfs | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s4b | btrfs | 34.0 / 164.0 / 800.0 | 0.0 / 4.0 / 80.0 | 0.0 / 0.0 / 0.0 | 32.0 / 164.0 / 800.0 |
| s4b | ext4 | 28.0 / 28.0 / 84.0 | 4.0 / 4.0 / 8.0 | 0.0 / 0.0 / 0.0 | 24.0 / 24.0 / 80.0 |
| s4b | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s6 | btrfs | 32.0 / 168.0 / 776.0 | 0.0 / 0.0 / 4.0 | 0.0 / 0.0 / 16.0 | 32.0 / 168.0 / 776.0 |
| s6 | ext4 | 8.0 / 28.0 / 224.0 | 0.0 / 0.0 / 204.0 | 8.0 / 8.0 / 20.0 | 0.0 / 20.0 / 32.0 |
| s6 | xfs | 8.0 / 8.0 / 204.0 | 0.0 / 0.0 / 204.0 | 8.0 / 8.0 / 20.0 | 0.0 / 0.0 / 4.0 |

## Bytes completed since the previous PREFLUSH (FUA-only bios ignored)

KiB completed between consecutive PREFLUSH-bearing bios only (FUA-only bios ignored); p50 / p90 / max
| scen | fs | total | small/db | stream/wal | other (journal+meta) |
|---|---|---|---|---|---|
| s1 | btrfs | 164.0 / 164.0 / 260.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 160.0 / 160.0 / 256.0 |
| s1 | ext4 | 20.0 / 24.0 / 24.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 16.0 / 20.0 / 20.0 |
| s1 | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s2a | btrfs | 4164.0 / 4228.0 / 6916.0 | 4.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 4096.0 | 4160.0 / 4224.0 / 6912.0 |
| s2a | ext4 | 4116.0 / 4116.0 / 4124.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 16.0 / 16.0 / 24.0 |
| s2a | xfs | 4100.0 / 4100.0 / 4100.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 0.0 / 0.0 / 0.0 |
| s2b | btrfs | 16452.0 / 17476.0 / 28856.0 | 4.0 / 4.0 / 788.0 | 32.0 / 16416.0 / 17444.0 | 16416.0 / 16448.0 / 24368.0 |
| s2b | ext4 | 19476.0 / 19484.0 / 38936.0 | 4.0 / 4.0 / 8.0 | 19456.0 / 19456.0 / 38912.0 | 16.0 / 24.0 / 24.0 |
| s2b | xfs | 19460.0 / 19460.0 / 38916.0 | 4.0 / 4.0 / 4.0 | 19456.0 / 19456.0 / 38912.0 | 0.0 / 0.0 / 0.0 |
| s2p | ext4 | 24.0 / 4124.0 / 12316.0 | 4.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 12288.0 | 20.0 / 24.0 / 36.0 |
| s2p | xfs | 4.0 / 4.0 / 8524.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 8520.0 | 0.0 / 0.0 / 62.5 |
| s3 | btrfs | 164.0 / 164.0 / 260.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 160.0 / 160.0 / 256.0 |
| s3 | ext4 | 20.0 / 24.0 / 32.0 | 4.0 / 4.0 / 8.0 | 0.0 / 0.0 / 0.0 | 16.0 / 20.0 / 28.0 |
| s3 | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s3e | btrfs | 4164.0 / 4228.0 / 8692.0 | 4.0 / 4.0 / 4.0 | 0.0 / 4096.0 / 4496.0 | 4160.0 / 4224.0 / 8368.0 |
| s3e | ext4 | 4120.0 / 4120.0 / 4124.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 20.0 / 20.0 / 24.0 |
| s3e | xfs | 4100.0 / 4100.0 / 4100.0 | 4.0 / 4.0 / 4.0 | 4096.0 / 4096.0 / 4096.0 | 0.0 / 0.0 / 0.0 |
| s4 | btrfs | 164.0 / 164.0 / 1760.0 | 4.0 / 64.0 / 264.0 | 0.0 / 0.0 / 0.0 | 128.0 / 164.0 / 1760.0 |
| s4 | ext4 | 8.0 / 8.0 / 8.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 | 8.0 / 8.0 / 8.0 |
| s4 | xfs | n/a (no PREFLUSH) | | | |
| s4b | btrfs | 164.0 / 164.0 / 800.0 | 4.0 / 4.0 / 80.0 | 0.0 / 0.0 / 0.0 | 160.0 / 164.0 / 800.0 |
| s4b | ext4 | 28.0 / 28.0 / 84.0 | 4.0 / 4.0 / 8.0 | 0.0 / 0.0 / 0.0 | 24.0 / 24.0 / 80.0 |
| s4b | xfs | 4.0 / 4.0 / 4.0 | 4.0 / 4.0 / 4.0 | 0.0 / 0.0 / 0.0 | 0.0 / 0.0 / 0.0 |
| s6 | btrfs | 168.0 / 168.0 / 776.0 | 0.0 / 0.0 / 4.0 | 0.0 / 0.0 / 16.0 | 168.0 / 168.0 / 776.0 |
| s6 | ext4 | 8.0 / 28.0 / 224.0 | 0.0 / 0.0 / 204.0 | 8.0 / 8.0 / 20.0 | 0.0 / 20.0 / 32.0 |
| s6 | xfs | 8.0 / 8.0 / 204.0 | 0.0 / 0.0 / 204.0 | 8.0 / 8.0 / 20.0 | 0.0 / 0.0 / 4.0 |


## Findings
1. **Every fsync is one PREFLUSH|FUA bio** on ext4 and xfs (50/s at 50 fsync/s), reaching the device as one empty FLUSH request then one FUA write, back to back (tens of microseconds). fdatasync is identical to fsync for this append pattern (the size change forces the same journal commit). btrfs uses a different shape: an empty PREFLUSH bio, then a separate FUA-only superblock write, 50/s each, and ~150 KiB of tree-log/metadata writes per fsync (~7.5 MB/s for a 0.2 MB/s workload).
2. **Entanglement is total.** A flush is device-wide: with a 200 MB/s stream, each fsync's flush point covers the ~4.1 MiB the stream wrote since the previous one (4096 of 4100 KiB are stream bytes); at 1 GB/s it is ~19 MiB (p50 19.0 MiB, max 38 MiB). The small file contributes 4 KiB (plus 16-24 KiB of ext4 journal). The flush rate is unchanged (50/s): the stream adds bytes per flush, not flushes. For a Lunix device the flush group therefore carries all data written since the previous flush, not the fsyncing file's data.
3. **Unaided kernel writeback** (s2p): the flush covers whatever the flusher threads had completed; mostly nothing (p50 24 KiB ext4 / 4 KiB xfs), occasionally up to 8-12 MiB bursts. Flush rate still 50/s.
4. **O_DSYNC overwrite of preallocated space (s4): FUA only, no PREFLUSH.** ext4 and xfs: 200 FUA writes/s, 0 FLUSH requests (ext4: 0.2/s PREFLUSH|FUA from occasional journal commits, 4 in 20 s; xfs: none). Every FUA write is alone (no FLUSH before it). So a device that advertises FUA sees 4 KiB FUA writes with nothing else; without FUA support the block layer would turn each into write+flush. btrfs does not behave this way: 200/s PREFLUSH + FUA superblock + ~150 KiB of log-tree writes each.
5. **O_DSYNC allocating append (s4b):** back to the fsync shape on ext4/xfs: one PREFLUSH|FUA per write (200/s), ext4 with 24 KiB journal per write, xfs 4 KiB data only (xfs log record 1.5-2 KiB, `n=3` sectors, rides in the FUA write).
6. **SQLite WAL (s6):** ~1 FLUSH request per commit (100.6/s for 100 commits/s) on ext4 and xfs, 59% as PREFLUSH|FUA (when the WAL file grew, i.e. metadata commit) and 41% as plain PREFLUSH with no FUA (data-only `blkdev_issue_flush`, empty flush bio). So an empty flush with no accompanying FUA write is common for WAL-style appends into preallocated space. Per flush: ~8 KiB WAL, occasionally a 204 KiB database checkpoint.
7. FUA-without-PREFLUSH exists only in s4 (ext4/xfs) at 200/s; every other ext4/xfs case has ~0 (ext4 s1/s3: 0.1/s, a handful per run, background journal activity).

## Representative sequence (one fsync cycle, s1; times relative, bio = FS submit, iss/cmp = driver)
ext4 (data=ordered, 4 KiB data, 5 journal blocks, commit):
```
bio WS    4K data      -> iss WS (small file)
bio WSM x5 (journal descriptor+metadata, merged) -> iss WSM 20 KiB
bio FWFSM 4K (commit block: PREFLUSH|FUA|SYNC|META)
iss FF (empty FLUSH) -> cmp FF
iss WFSM (FUA commit write) -> cmp
```
xfs: `bio WS 4K data; bio FWFSM 1.5 KiB (log write, PREFLUSH|FUA); iss FF; cmp FF; iss WFSM; cmp`. No separate metadata writes.
btrfs: `WS 4K data; WSM 2x80 KiB (log tree copies, DUP profile); bio FWS (empty PREFLUSH); iss FF; cmp; bio WFSM 4K superblock (FUA only); cmp`.
Full text of one cycle per case: `raw/<fs>-<scen>-1.seq.txt` (s4/s4b/s6: `-9`).

## Raw files
`raw/all.csv` (every run), `raw/*.summary.csv`, `raw/*.rwbs.txt` (distinct flag strings seen), `raw/*.gen` (generator output), `raw/*.seq.txt`, `raw/*-1.flushes.csv` (per flush point, rep 1).

## Proposed decision record
Lunix must expect: (a) fsync-driven FLUSH requests at the fsync rate, each preceded by data writes of all writers since the previous flush (MiB at streaming rates), so flush-group size is bounded by write bandwidth x flush interval, not by the fsyncing file; (b) PREFLUSH|FUA commit writes arrive as an empty FLUSH request followed immediately by a FUA write of 4 KiB or less; (c) O_DSYNC overwrite arrives as FUA alone, without a FLUSH, at the application rate; each FUA must be durable per ADR-3 without implying a flush of other writes; (d) empty FLUSH without FUA is common (SQLite WAL, btrfs). Advertising FUA matters for (c): without it the block layer would turn each FUA write into write+flush (not measured here). These are numbers from a zero-latency RAM device on a single queue; with real flush latency, fsyncs queue behind each other and the bytes-per-flush figures grow.
