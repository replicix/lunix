# FUA and FLUSH semantics: can Lunix persist a FUA write alone?

Date: 2026-11-12. Status: research note, no decision. Feeds a later review of
ADR-3 / ADR-27 and DESIGN §3.1, §8.1, §8.3, §8.6.

Method and limits. Kernel and file-system claims come from reading source
fetched from `raw.githubusercontent.com/torvalds/linux/master` on 2026-11-12
(the `Makefile` there says 7.4.0-rc2), plus tagged versions where a version
matters. Nothing here was measured: no blktrace or ublk trace was run, so the
"requests the device sees" tables are derived from code reading, not observed.
A follow-up trace is listed in the last section. Claims I could not confirm
from a primary source are marked UNVERIFIED. Normative NVMe and SCSI text is
paywalled or not reachable by my tools, so those are secondary (marked).

## Summary

1. **The contract is narrow.** PREFLUSH promises that writes *completed
   before the flush was issued* are durable when it completes. FUA promises
   that *this request's data* is durable when it completes. Neither says
   anything about writes that were not completed first, and nothing says a
   volatile cache persists in FIFO order. The block layer states the
   file system must supply its own ordering; devices and the block layer may
   reorder at will.
2. **ublk never sees PREFLUSH on a write.** `blk_insert_flush()` clears
   `REQ_PREFLUSH` before the driver; a PREFLUSH|FUA write arrives as a
   `REQ_OP_FLUSH` request, completed first, then a `WRITE` with the FUA flag.
   Lunix therefore cannot tell a "FUA without PREFLUSH" write from the second
   half of a PREFLUSH|FUA pair by looking at the request. It can only observe
   whether a FLUSH finished before it.
3. **ext4 and xfs almost never send a bare FUA.** Journal commits (jbd2
   commit block, xfs checkpoint commit, btrfs superblock) are PREFLUSH|FUA,
   i.e. FLUSH then WRITE+FUA at the device. The jbd2 tail update and the
   ext4 superblock write are FUA after a separate FLUSH. The one common
   bare-FUA source is `O_DIRECT|O_DSYNC` overwrite of already-written blocks
   (iomap "write-through", since Linux 4.18; on ext4 since 5.5).
4. **File systems do not rely on prefix order of unflushed writes.** jbd2
   and the xfs log use FLUSH/FUA as the only ordering points and carry
   checksums so recovery detects a missing or torn tail. jbd2's
   `journal_async_commit` mode drops the pre-commit flush and relies on those
   checksums, which is direct evidence that arbitrary subsets of unflushed
   writes are an expected state. I found no mainstream file system or
   database in scope that depends on prefix order between flush points.
5. **Databases mostly send FLUSH, not FUA.** PostgreSQL (`fdatasync` default
   on Linux), InnoDB (`O_DIRECT` + `fsync`), RocksDB, SQLite, bbolt, etcd and
   Redis all use buffered or `O_DIRECT` writes plus `fdatasync`/`fsync`, which
   reach the device as writes plus FLUSH (and a jbd2/xfs commit when metadata
   changed). A bare FUA arises only with `O_DIRECT|O_DSYNC`, which none of
   them use by default. QEMU `cache=directsync` is the main producer.
6. **Other virtual block devices:** most promote FUA to a full flush (loop
   via the block layer, dm-writecache, dm-cache, QEMU file-posix without
   `RWF_DSYNC`, bcache journal). Two persist the write alone: Linux LIO
   FILEIO (`vfs_fsync_range` on the write's range) and QEMU's
   linux-aio/io_uring path (`RWF_DSYNC`). Ceph krbd and librbd side-step it
   (write-through until first flush; flush writes all dirty data). LSVD
   documents prefix consistency and a per-barrier model; I found no FUA
   handling in the LSVD paper or the DIS kernel code.
7. **Crash images.** Persisting a FUA write alone and committing older
   writes later yields intermediate images of the form
   `flushed prefix + {FUA write}`. By the contract in point 1 that is a legal
   post-power-loss state. It is not a *cut* in ADR-3's sense, so
   restore points, mirrors and time-travel mounts (DESIGN §8.6) would expose
   non-cut images that are legal as crash images but break the "every commit
   is a cut" statement and the CDP wording of §8.1. Replay must be by highest
   `seq` per block (including discard and zero records), not by commit
   order.
8. **Little to gain on the common path.** Under jbd2 and xfs the FUA write
   follows a FLUSH that has just made everything earlier durable, so
   "everything completed before it" is usually just the writes issued in
   between. The fast path only pays for bare-FUA workloads
   (`O_DIRECT|O_DSYNC`, mixed with a large unflushed backlog) and for ordinary
   commits whose FLUSH and FUA are serialised by the block layer regardless.

## 1. Block-layer contract

### 1.1 Kernel documentation

Source: `Documentation/block/writeback_cache_control.rst`
(<https://docs.kernel.org/block/writeback_cache_control.html>; text read from
master, 2026-11-12). Exact promises:

- **PREFLUSH:** "will make sure the volatile cache of the storage device has
  been flushed before the actual I/O operation is started. This explicitly
  guarantees that previously completed write requests are on non-volatile
  storage before the flagged bio starts." On an empty bio it is a pure
  cache flush. The scope is *completed* writes; writes in flight when the
  flush is issued are not covered.
- **FUA:** "will make sure that I/O completion for this request is only
  signaled after the data has been committed to non-volatile storage." The
  scope is the request's own data.
- **PREFLUSH|FUA:** both may be set on one bio. By composition: earlier
  completed writes durable, then this write durable.
- Devices without a volatile cache: the block layer completes empty
  PREFLUSH before the driver and strips PREFLUSH/FUA from writes.
- blk-mq drivers with `BLK_FEAT_WRITE_CACHE`: "REQ_OP_WRITE | REQ_PREFLUSH
  requests with a payload are automatically turned into a sequence of a
  REQ_OP_FLUSH request followed by the actual write". With `BLK_FEAT_FUA`
  the FUA bit is passed on; without it "a REQ_OP_FLUSH request is sent by
  the block layer after the completion of the write request".
- The document never says what a volatile cache may lose or in what order.
  The contract is purely "flushed/FUA'd data survives"; everything else
  is unspecified, i.e. any subset of the rest may survive.

Implementation, `block/blk-flush.c` (master):

- `blk_insert_flush()` computes a policy of DATA / PREFLUSH / POSTFLUSH
  steps. It then does `rq->cmd_flags &= ~REQ_PREFLUSH` and, if the queue
  lacks FUA, `&= ~REQ_FUA`. **The driver never sees PREFLUSH on a request
  with data.** A PREFLUSH|FUA write on a FUA-capable queue becomes a
  `REQ_OP_FLUSH` request (executed first, step PREFLUSH) and then the data
  request with `REQ_FUA`.
- The header comment says flushes are "double buffered", at most one flush
  in progress (C1), and a flush is deferred while any request is executing
  the DATA step of its sequence (C2), except that C3 bounds starvation "in
  the unlikely case where there are continuous stream of FUA (without
  PREFLUSH) requests". One `REQ_OP_FLUSH` can serve several pending
  PREFLUSH requests.
- Without `BLK_FEAT_FUA` (Lunix advertising only `VOLATILE_CACHE`), FUA
  becomes a trailing flush of everything. Lunix advertises both
  (DESIGN §3, ADR-3), so a FUA write is delivered as `UBLK_IO_F_FUA` on a
  WRITE (`drivers/block/ublk_drv.c`, `UBLK_IO_F_FUA` set when
  `req->cmd_flags & REQ_FUA`; features set at `BLK_FEAT_WRITE_CACHE` and
  `BLK_FEAT_FUA` from the `UBLK_ATTR_*` bits).

### 1.2 Ordering and what a volatile cache may lose

- `Documentation/block/blk-mq.rst`
  (<https://www.kernel.org/doc/html/latest/block/blk-mq.html>): "Neither the
  block layer nor the device protocols guarantee the order of completion of
  requests." The file system must handle ordering.
- LWN, "The end of block barriers" (<https://lwn.net/Articles/400541/>):
  barriers no longer imply ordering; "The block layer can then reorder
  requests at will" (as summarised by my fetch tool; the page text was not
  read in full). A journaling file system writes the journal, waits for
  completion, then sends the commit record with flush and FUA.
- Empirical: Zheng et al., FAST '13 (<https://www.usenix.org/conference/fast13/understanding-robustness-ssds-under-power-fault>):
  of 15 SSDs tested, only two (same model) and one enterprise HDD behaved
  per the expected semantics under power fault; 8 showed "unserializable
  writes" (on-disk result matches no serial order of the writes), 3 shorn
  writes. This is search-result level evidence; I did not read the paper.
  It supports that real devices persist non-prefix subsets.
- Chidambaram et al., "Optimistic Crash Consistency" (SOSP '13,
  <https://research.cs.wisc.edu/wind/Publications/optfs-sosp13.html>): file
  systems conflate ordering and durability; ordering writes costs a flush.
  Read only via search snippet.

### 1.3 NVMe and SCSI

All secondary; the normative documents were not read.

- **NVMe** (NVM Command Set, Write command, FUA bit; Base spec, Flush
  command). Quoted by <https://evan-jones.appspot.com/durability-nvme.html>
  (NVMe 2.0, Command Set §3.2.6, Figure 63): with FUA the controller "shall
  write that data and metadata, if any, to non-volatile media before
  indicating command completion". Flush (Base spec §7.1): applies to
  commands "completed by the controller prior to the submission of the Flush
  command". Same article: FUA writes have no ordering with each other, and
  FUA is described as acting on the command's own data. No ordering of
  persistence among cached writes is promised. Torn power-fail writes above
  the Atomic Write Unit Power Fail size are permitted (from memory of the
  spec, UNVERIFIED).
- **SCSI / SBC**: vendor manuals reproduce the rule: with FUA=1 "the data
  must be written on the medium before GOOD status is returned"; with FUA=0
  GOOD may be returned "before it is written to the medium"
  (Maxtor/Seagate manuals via search results; SBC-4 text UNVERIFIED).
  SYNCHRONIZE CACHE flushes the cache for a range or all blocks. FUA is only
  meaningful with write cache enable (Seagate note, secondary).
- **ATA**: `WRITE DMA FUA EXT` and `FLUSH CACHE EXT`; same split. UNVERIFIED
  (not fetched).

Conclusion for Q1: for all three protocols FUA is per-command durability and
flush is per-completed-command durability. A device may therefore persist any
subset of unflushed, un-FUA'd writes, in any order, on power loss, and that
is what the empirical studies observe.

## 2. What ext4 and xfs actually send

Notation: `W` plain write, `W+FUA` write with `UBLK_IO_F_FUA`, `FLUSH`
`UBLK_IO_OP_FLUSH`. "PREFLUSH|FUA" is what the file system submits; ublk
sees `FLUSH` then `W+FUA`, serialised by the block layer (§1.1).

### 2.1 Buffered fsync / fdatasync

- **ext4** (`fs/ext4/fsync.c`, `ext4_sync_file()`): `file_write_and_wait_range()`
  (plain, `REQ_SYNC` writes of the file's dirty pages), then, with a journal,
  `ext4_fsync_journal()`:
  - If `journal->j_flags & JBD2_BARRIER` and
    `!jbd2_trans_will_send_data_barrier(journal, commit_tid)` then
    `needs_barrier = true` and `blkdev_issue_flush()` follows the commit wait
    (`issue_flush` label). The barrier is skipped when the commit that is
    being waited on will itself send the flush.
  - `ext4_fc_commit()` waits for the transaction (or does a fast commit).
    When no transaction is needed (fdatasync of an overwrite with no
    metadata change), the result is `W...` then one `FLUSH`.
  - When a jbd2 commit happens, `fs/jbd2/commit.c`
    `journal_submit_commit_record()`: `write_flags` get
    `REQ_PREFLUSH | REQ_FUA` if `JBD2_BARRIER` is set and the
    `async_commit` feature is **not**. Descriptor and metadata blocks are
    written first with `JBD2_JOURNAL_REQ_FLAGS` = `REQ_META | REQ_SYNC |
    REQ_IDLE` (`include/linux/jbd2.h`), no flush. In `data=ordered` mode the
    file data is already written and waited on before the commit starts.
    Device view: `W`(data) `W`(journal blocks) `FLUSH` `W+FUA`(commit block).
  - `journal_async_commit` (`jbd2_has_feature_async_commit`): the commit block
    is written *without* PREFLUSH/FUA before descriptors are waited on, and a
    `blkdev_issue_flush(journal->j_dev)` follows the commit-record wait
    (`commit.c`, "if async_commit && BARRIER"). `Documentation/admin-guide/ext4.rst`:
    "Commit block can be written to disk without waiting for descriptor
    blocks ... This will enable 'journal_checksum' internally." Recovery
    uses checksums (`fs/jbd2/recovery.c`, `jbd2_commit_block_csum_verify`,
    descriptor and tag checksums) to reject a commit whose blocks did not all
    arrive.
  - `fast_commit` (off by default; UNVERIFIED default): `fs/ext4/fast_commit.c`
    sets `REQ_FUA | REQ_PREFLUSH` on the tail block when `barrier` is on.
  - `barrier=0` / `nobarrier`: none of the above flags are used
    (`ext4.rst`: "disables" write barriers). Out of scope for Lunix, which
    advertises a cache.
- **xfs** (`fs/xfs/xfs_file.c`, `xfs_file_fsync()`): `file_write_and_wait_range()`,
  then `xfs_log_force_inode()` if the inode has a log item with fsync-relevant
  changes, else `blkdev_issue_flush()` on the file's data device ("If the log
  force was a no-op, we may still need to flush the file data target cache
  here. This can happen for fdatasync/O_DSYNC when no metadata needed to be
  committed"). So an overwrite fdatasync is `W...` `FLUSH`; one that logged
  metadata is a log force.
- **xfs log writes** (`fs/xfs/xfs_log.c`, `xfs_log_cil.c`): in
  `xlog_write_iclog()` `XLOG_ICL_NEED_FLUSH` or `NEED_FLUSH_LOG` gives
  `REQ_PREFLUSH`, `XLOG_ICL_NEED_FUA` gives `REQ_FUA`.
  - `xfs_log_cil.c`: the iclog holding the checkpoint's *start* record gets
    `NEED_FLUSH` ("Flush the write cache before writing the start record so
    that the metadata we are about to overwrite in the log and the data that
    new allocations in this context refer to are persisted"). The *commit*
    iclog always gets `NEED_FUA` ("must be written to stable storage"), plus
    `NEED_FLUSH_LOG` if the checkpoint spans several iclogs.
  - `xlog_force_iclog()` and the force paths set `NEED_FLUSH | NEED_FUA`.
  - Net effect: every checkpoint commit is PREFLUSH|FUA, i.e. `FLUSH`
    then `W+FUA`, **whether or not an fsync caused it**. Background
    checkpoints (CIL size or timer) also emit them. Intermediate iclogs of a
    large checkpoint are plain `W`.
  - `xlog_flush_data_caches()`: with a separate log or realtime device the
    other devices get an empty PREFLUSH bio in parallel; not relevant to a
    single Lunix volume.

### 2.2 jbd2 tail and superblock writes (bare FUA after a flush)

- `fs/jbd2/checkpoint.c` (`jbd2_cleanup_journal_tail`): "We need to make sure
  that any blocks that were recently written out ... are flushed out before
  we drop the transactions from the journal": `blkdev_issue_flush(j_fs_dev)`
  under `JBD2_BARRIER`, then `__jbd2_update_log_tail()`.
- `fs/jbd2/journal.c` `__jbd2_update_log_tail()`: "We cannot afford for write
  to remain in drive's caches since as soon as we update j_tail, next
  transaction can start reusing journal space and if we lose sb update during
  power failure we'd replay old transaction with possibly newly overwritten
  data": `jbd2_journal_update_sb_log_tail(..., REQ_FUA)`. Device view:
  `FLUSH` then `W+FUA`(journal superblock), with no PREFLUSH on the FUA
  write itself. Here the FUA write's own ordering needs come from the
  preceding FLUSH, not from FUA.
- `fs/ext4/super.c` `ext4_commit_super()`-family: `REQ_OP_WRITE | REQ_SYNC |
  (barrier ? REQ_FUA : 0)`: a bare FUA superblock write.
- Journal wipe/mount paths use PREFLUSH|FUA (`jbd2_mark_journal_empty`).

### 2.3 Direct I/O and `O_DSYNC`

`fs/iomap/direct-io.c` (master), shared by ext4 (since 5.5) and xfs:

- In `__iomap_dio_rw()`: for `iocb_is_dsync(iocb)` set `IOMAP_DIO_NEED_SYNC`;
  if the write is not `IOCB_SYNC` (i.e. `O_DSYNC`/`RWF_DSYNC`, not `O_SYNC`)
  also set `IOMAP_DIO_WRITE_THROUGH`. Comment: "requires either FUA writes
  through the device's write cache, or a normal write to a device without a
  volatile write cache."
- In `iomap_dio_bio_iter()`: `REQ_FUA` is added only if
  `WRITE_THROUGH` is set and `!need_completion_work` and
  `!(iomap->flags & IOMAP_F_DIRTY)` and
  `(!bdev_write_cache() || bdev_fua())`. Otherwise `WRITE_THROUGH` is
  cleared for the whole dio, and `iomap_dio_complete` keeps `NEED_SYNC`,
  so `generic_write_sync()` runs a full `fdatasync` at completion
  (`FLUSH`, or a journal/log commit if metadata changed). The code comment
  notes one request can map to several extents, and if any is not a pure
  overwrite the whole request takes the full-sync path.
- `need_completion_work` becomes true for: shared/COW extents, `IOMAP_F_NEW`
  (newly allocated), unwritten extents (needs conversion), and writes that
  extend i_size (`COMP_WORK`).
- `IOMAP_F_DIRTY` is set by the file system when the inode has uncommitted
  datasync-relevant metadata or the write reaches past EOF: ext4
  (`ext4_set_iomap`: `ext4_inode_datasync_dirty(inode) || offset + length >
  i_size_read(inode)`), xfs (`xfs_iomap.c`: `ili_datasync_seq` non-zero, or
  write past EOF).
- Version history (tag greps): the `IOMAP_DIO_WRITE_FUA` optimisation is
  absent in `fs/iomap.c` at v4.17 and present at v4.18, so it arrived in 4.18
  (xfs). ext4 used its own direct I/O before; `fs/ext4/file.c` calls
  `iomap_dio_rw` at v5.5 and not at v5.4, so ext4 gets it from 5.5. The flag
  was renamed `IOMAP_DIO_WRITE_THROUGH` and the bdev-without-cache case added
  by v6.6. Whether 4.18 is exactly the merge window is UNVERIFIED (I did not
  read the commit log).
- io_uring/AIO: the comment states iomap always completes `O_[D]SYNC` writes
  synchronously, so the same logic applies.
- Cases:
  - **Overwrite of previously written, allocated blocks inside i_size, inode
    not datasync-dirty:** `W+FUA`, nothing else. No FLUSH.
  - **Overwrite of preallocated (unwritten) blocks:** `W`, then conversion
    transaction and full fdatasync: `W`, journal `W`s, `FLUSH`, `W+FUA`.
    Pre-allocated-but-never-written files therefore do not get the FUA path
    on first write.
  - **Append or allocating write:** same as above (`IOMAP_F_NEW`, past EOF).
  - **`O_SYNC` direct write:** not write-through; `W` then full fsync.
- **Buffered `O_DSYNC` write:** `generic_write_sync` after the copy: the same
  as fdatasync of the written range (`W...` then `FLUSH` or commit).

### 2.4 `sync_file_range`

`sync_file_range(2)` (<https://man7.org/linux/man-pages/man2/sync_file_range.2.html>)
starts or waits for writeback of a range. The man page: "None of these
operations writes out the file's metadata ... unless the application is
strictly performing overwrites of already-instantiated disk blocks, there are
no guarantees that the data will be available after a crash." It issues plain
writes and no device flush. RocksDB uses it for `bytes_per_sync`
(`env/io_posix.cc`) as a pacing hint, not as durability. So Lunix sees only
`W`.

### 2.5 btrfs (read lightly)

`fs/btrfs/disk-io.c`: `write_dev_flush()` submits an empty
`REQ_OP_WRITE | REQ_SYNC | REQ_PREFLUSH` bio per device, and the first
superblock copy is written with `REQ_FUA` unless `nobarrier`
("We FUA only the first super block. The others we allow to go down lazy").
So a btrfs transaction commit is `W`... `FLUSH` `W+FUA`(super copy 0)
`W`(other copies, no FUA). The fsync log path (`tree-log.c`) I did not read:
UNVERIFIED. Other copies written lazily are a non-prefix state by design.

### 2.6 Table: workload to requests the device sees

`W*` = zero or more plain writes of the file's data/metadata. Commit =
jbd2 or xfs commit as above. Derived from source, not traced.

| workload | ext4 (journal, `data=ordered`) | xfs |
|---|---|---|
| buffered `write` + `fsync`, file size/blocks changed | `W*`, journal `W*`, `FLUSH`, `W+FUA` (commit block) | `W*`, log `W*`, `FLUSH`, `W+FUA` (commit iclog); for a one-iclog checkpoint this is one PREFLUSH\|FUA write |
| buffered overwrite, `fdatasync`, no metadata change | `W*`, `FLUSH` | `W*`, `FLUSH` |
| buffered overwrite, `fsync`, only mtime changed | `W*`, commit (`FLUSH`, `W+FUA`) because fsync, not fdatasync, needs the timestamp | same, via log force |
| buffered append + `fdatasync` | as first row (i_size is datasync metadata) | as first row |
| `O_DIRECT\|O_DSYNC` overwrite of written blocks, inode clean | `W+FUA` only | `W+FUA` only |
| `O_DIRECT\|O_DSYNC` overwrite of unwritten (preallocated) blocks | `W`, conversion commit: `FLUSH`, `W+FUA` | same |
| `O_DIRECT\|O_DSYNC` append / allocating write | `W`, i_size/alloc commit: `FLUSH`, `W+FUA` | same |
| `O_DIRECT\|O_SYNC` write | `W`, then full fsync: `FLUSH` (or commit) | same |
| `O_DIRECT` write + `fdatasync` | `W`, `FLUSH` (or commit if metadata) | same |
| `sync_file_range` | `W` only | `W` only |
| background journal/log commit (5 s jbd2 timer, xfs CIL push) | commit: `FLUSH`, `W+FUA` | checkpoint: `FLUSH`, `W+FUA` |
| jbd2 checkpoint, tail advance | `FLUSH`, then `W+FUA` (journal sb); no data in the FUA write's preceding flush other than checkpointed metadata | n/a (xfs tail is in log records) |
| ext4 superblock update (feature/state) | `W+FUA` bare (`ext4_commit_super`) | n/a |
| ext4 with `journal_async_commit` | commit block `W` without flags, then `FLUSH` after the wait | n/a |
| btrfs transaction commit (for comparison) | n/a | n/a; `FLUSH`, `W+FUA` (super 0), `W` (other supers) |

The bare-FUA rows are only the `O_DIRECT|O_DSYNC` overwrite and the ext4
superblock write. Everything else is `FLUSH` then `W+FUA`.

## 3. Do file systems rely on prefix order of unflushed writes?

**jbd2.** The documented protocol (LWN above; code in `commit.c`): write
descriptors and metadata (plain), wait for completion, then the commit block
with PREFLUSH|FUA. The flush makes every completed journal write durable
before the commit block, which in turn is durable on completion. Recovery
(`recovery.c`) accepts a transaction when it finds its commit block; there is
no assumption that earlier unflushed writes persisted in order, because the
flush ordered them. For `journal_async_commit` the pre-commit flush is
removed and replaced by checksums: recovery verifies descriptor, tag and
commit checksums and discards a transaction whose blocks are missing. That
mode exists only because the file system accepts "commit block persisted,
earlier blocks not". Tail advance (§2.2) is explicitly protected with FLUSH
+ FUA because the file system assumes the superblock write may linger in the
cache otherwise. All of this is a file system that assumes arbitrary subsets
of unflushed writes.

**xfs log.** Records carry CRCs; `xfs_log_recover.c` has
`xlog_verify_head()` ("Detect and trim torn writes from the head of the
log", "Storage without sector atomicity guarantees can result in torn
writes") and `xlog_find_head()`, and treats CRC failure near the head as a
torn write. Checkpoint ordering is by PREFLUSH on the start record and FUA
on the commit record (§2.1). No prefix assumption. (Only the headers and
comments were read, not the whole recovery logic.)

**btrfs.** Superblock copy 0 is FUA, the rest "lazy"; transaction commit
flushes first. Copy-on-write plus checksums; no prefix assumption visible in
what I read.

**Databases.**
- PostgreSQL: `full_page_writes` ("writes the entire content of each disk
  page to WAL during the first modification of that page after a
  checkpoint", on by default) exists to survive torn/partial pages; WAL
  records are CRC'd and replay stops at the first invalid record. Data
  files are written lazily and fsynced at checkpoints, so PostgreSQL
  already assumes arbitrary subsets of data-file writes between fsyncs.
  <https://www.postgresql.org/docs/current/runtime-config-wal.html>.
- InnoDB: doublewrite buffer (default `ON`) repairs torn pages; redo log
  has checksums. <https://dev.mysql.com/doc/refman/8.4/en/innodb-parameters.html>.
- SQLite WAL: "The WAL must be synced to persistent storage prior to moving
  content from the WAL into the database and the database file must be
  synced prior to resetting the WAL." (<https://www.sqlite.org/wal.html>).
  Ordering comes from syncs, not from write order.
- bbolt: write data pages, `fdatasync`, write meta page, `fdatasync`
  (`tx.go`: `tx.write()` then `tx.writeMeta()`, each followed by
  `fdatasync` unless `NoSync`). Two meta pages with checksums.
- etcd WAL: CRC-chained records, `fdatasync` per batch
  (`server/storage/wal/wal.go`, `sync()` uses `fileutil.Fdatasync`).
- Redis AOF: appended file; recovery truncates a bad tail
  (`redis-check-aof`; recovery behaviour UNVERIFIED, not read).

**Would anything break on an arbitrary subset?** Within these, no. The
protocols above are designed for it. Residual risks that are about *any*
non-prefix state, not about FUA specifically:
- A file system or application with a latent bug that only real devices
  with small caches never expose (they persist nearly in order). Lunix
  would expose non-prefix states reliably instead of rarely. I know of no
  specific instance in scope (UNVERIFIED as an absence claim).
- Software without barriers that depends on FIFO persistence: ext2, FAT,
  `nobarrier` mounts, and ext3-era assumptions. These are already unsafe on
  volatile-cache hardware.
- Mount-time `fsck`-less assumptions: none found.

## 4. Databases: what reaches the block device

All rows derived from docs/source, not traced. "On ext4/xfs" assumes the
default journal mode and a pre-written (zero-filled) log or data file unless
noted. A bare FUA needs `O_DIRECT|O_DSYNC` or `RWF_DSYNC` with O_DIRECT; none
of these use it by default.

| system | durability call | request pattern | bare FUA? |
|---|---|---|---|
| PostgreSQL WAL | `wal_sync_method=fdatasync` default on Linux and FreeBSD (docs: "fdatasync is the default on Linux and FreeBSD"). `open_datasync` is available. WAL files pre-zeroed (`wal_init_zero` on) and recycled (`wal_recycle` on). `synchronous_commit=on`. | per commit group: `W*` (WAL pages), `FLUSH`. A segment switch (new/renamed file, metadata) adds a journal/log commit (`FLUSH`, `W+FUA`) | no |
| PostgreSQL data files | `fsync` of each file at checkpoint | burst of `W`, then one `FLUSH` per file; fdatasync-style flush each | no |
| PostgreSQL with `debug_io_direct=wal` + `open_datasync` | `O_DIRECT\|O_DSYNC` | `W+FUA` per WAL write on written blocks. Docs: "intended for developer testing only", "currently reduces performance" (<https://www.postgresql.org/docs/current/runtime-config-developer.html>, PG 18 docs) | yes, but not a production setting |
| InnoDB | `innodb_flush_method` default "O_DIRECT if supported, otherwise fsync": `O_DIRECT` to open data files, `fsync()` to flush data and log files. `O_DIRECT_NO_FSYNC` skips fsync after each write but still fsyncs on create/extend/close. `O_DSYNC` mode: "O_SYNC to open and flush the log files, and fsync() to flush the data files" | redo log: `W`, `FLUSH` per commit group (`innodb_flush_log_at_trx_commit=1` default). Data: `W` with doublewrite then `FLUSH`. Redo/data on different devices with `O_DIRECT_NO_FSYNC` risks data loss per the docs | no (O_SYNC is not write-through in iomap, §2.3) |
| RocksDB | `use_fsync=false` default: `fdatasync`; "Direct I/O for WAL and Manifest files is not supported yet" (<https://github.com/facebook/rocksdb/wiki/Direct-IO>); `sync_file_range` for `bytes_per_sync` (pacing) | WAL: `W`, `FLUSH` per synced write batch. SST: `W`s paced by `sync_file_range`, final `fdatasync` | no |
| SQLite WAL | `synchronous=FULL`: sync WAL each commit; `NORMAL`: sync only at checkpoint; checkpoint syncs WAL, then DB (<https://www.sqlite.org/wal.html>). Underlying call `fsync`/`fdatasync` (UNVERIFIED which on Linux) | `W` + `FLUSH` per commit (FULL); two or three `FLUSH`es per checkpoint | no |
| etcd / bbolt | etcd WAL `fdatasync` per batch into preallocated 64 MB segments (`SegmentSizeBytes`, `fileutil.Preallocate`); bbolt two `fdatasync`s per commit (data pages, then meta) | etcd: `W`, `FLUSH` per batch; if segments are `fallocate`d unwritten extents the first write to each range includes extent conversion, i.e. `FLUSH`, `W+FUA` commit (UNVERIFIED whether `Preallocate` zero-fills). bbolt: `W*`, `FLUSH`, `W`(meta 4 KiB), `FLUSH` | no |
| Redis AOF | `appendfsync always`: `redis_fsync` which "is defined as fdatasync() for Linux in order to avoid flushing metadata" (`src/aof.c`); `everysec`: background fdatasync | `W` (append), `FLUSH`; appends change i_size, so each fdatasync carries a journal commit: `FLUSH`, `W+FUA` | no |

**Relative frequency (derived).** Bare FUA: effectively zero for default
configurations of all systems above. FLUSH: one or more per commit group.
`FLUSH` followed by `W+FUA` (journal commit): one per fsync that changes
metadata (appends, allocation, mtime for fsync), plus the file system's own
5 s / CIL timer commits. A trace of PostgreSQL pgbench and fio on ext4 and
xfs would give real ratios; not done.

**Other producers of `O_DIRECT|O_DSYNC`.** QEMU `cache=directsync` opens
the image `O_DIRECT|O_DSYNC` (documented behaviour; not re-verified here), so
a VM image file on a file system on a Lunix device produces bare FUA for each
guest write that is a pure overwrite. `fio --direct=1 --sync=1` and
`dd oflag=direct,dsync` are the benchmarking sources. Ceph BlueStore and
similar userspace stores use `O_DIRECT` plus explicit flush (UNVERIFIED).

## 5. How other virtual block devices handle FUA

| system | FUA handling | source |
|---|---|---|
| Linux loop | advertises `BLK_FEAT_WRITE_CACHE` if backing file has `fsync`, no `BLK_FEAT_FUA`; so the block layer turns FUA into write + full FLUSH; `REQ_OP_FLUSH` → `vfs_fsync(file)` of the entire backing file | `drivers/block/loop.c` (lines setting features, `lo_req_flush`) |
| Linux nbd client | sets `BLK_FEAT_FUA` iff the server sets `NBD_FLAG_SEND_FUA`, and forwards `NBD_CMD_FLAG_FUA` | `drivers/block/nbd.c` |
| NBD protocol | server "MUST NOT reply to a command that has `NBD_CMD_FLAG_FUA` set ... until the data (if any) written by that command is persisted"; FLUSH covers writes the server replied to before the flush. NBD says it is modelled on the kernel bio flags | `doc/proto.md` in <https://github.com/NetworkBlockDevice/nbd> |
| dm-writecache | FUA write → `writecache_flush(wc)`, which commits all uncommitted entries (everything written to the cache since the last commit); PREFLUSH likewise. Autocommit after N blocks or time | `drivers/md/dm-writecache.c` (`REQ_FUA` branch in `writecache_map`) |
| dm-cache | comment: "dm core turns FUA requests into a separate payload and FLUSH req"; then `issue_after_commit()` forces a metadata commit | `drivers/md/dm-cache-target.c` |
| bcache | `flush_journal = op_is_flush(bio->bi_opf)` where `op_is_flush` = `REQ_FUA \| REQ_PREFLUSH`; the data bio's flags are stripped and the journal entry is written with PREFLUSH. The journal is a sequential log, so the FUA write is persisted together with the earlier keys in the open journal entry (a prefix), my reading, not verified | `drivers/md/bcache/request.c`, `include/linux/blk_types.h` |
| QEMU `file-posix` | `bs->supported_write_flags = BDRV_REQ_FUA` unless linux-aio/io_uring lack FUA support; with it, linux-aio and io_uring use `RWF_DSYNC` per write (persists that write alone, subject to the host file system); otherwise write then `bdrv_co_flush` (`fdatasync` of the whole fd) | `block/file-posix.c`, `block/linux-aio.c`, `block/io_uring.c` (QEMU master, VERSION 11.1.90); which release added `laio_has_fua` UNVERIFIED |
| QEMU virtio-blk | FUA reaches the guest as `VIRTIO_BLK_F_REQ_FLAGS_OUT_FUA` bit; otherwise the guest sees only flush | `hw/block/virtio-blk.c` |
| QEMU cache modes | `BDRV_O_NO_FLUSH` (`cache=unsafe`) drops FUA; other modes keep it | `block/io.c` (`bdrv_driver_pwritev`) |
| LIO target FILEIO | after the write, `vfs_fsync_range(file, start, end, 1)` over just that range (the code comment says "Perform implicit vfs_fsync_range() ... for SCSI WRITEs with Forced Unit Access (FUA) set. Allow this to happen independent of WCE=0"). **Persists the write alone.** | `drivers/target/target_core_file.c` |
| LIO target IBLOCK | passes `REQ_FUA` to the backing bdev | `drivers/target/target_core_iblock.c` |
| Ceph librbd | RBD cache: "Prior to receiving a flush request, the cache behaves like a write-through cache", `rbd_cache_writethrough_until_flush`, "When the OS sends a barrier or a flush request, all dirty data is written to the OSDs". FUA not mentioned. Flush is everything | <https://docs.ceph.com/en/latest/rbd/rbd-config-ref/> |
| Ceph krbd | no `BLK_FEAT_WRITE_CACHE` in `drivers/block/rbd.c` (grep found none): the kernel treats it as no volatile cache, so every write is acked after the OSD commit and flush/FUA are stripped by the block layer | `drivers/block/rbd.c` |
| LSVD (EuroSys '22) | claims prefix consistency and "commit barrier" semantic: a barrier is sent to the cache SSD and "when it completes, all preceding LSVD writes will be durable". The paper does not discuss FUA. DIS (`github.com/asch/dis`, `kernel/dm-disbd.c`) references only `REQ_OP_FLUSH`; how FUA writes are treated is UNVERIFIED | paper <https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf> §2.2, §3.2 |
| ZeroFS (from existing prior-art note) | NBD FLUSH/FUA replies only after data is durable; mechanism not stated | `research/2026-11-11-prior-art.md` |
| iSCSI initiator / other targets (tgt, SPDK) | not checked; UNVERIFIED | |

Answer for Q5: **only LIO FILEIO and QEMU with `RWF_DSYNC` persist a FUA
write alone**, and both do so on top of a file system that has its own
mechanism for single-range durability (`fsync_range`, `O_DSYNC`). The cache
devices (dm-writecache, dm-cache, bcache) treat FUA as "commit everything in
the cache", which is their implementation of a prefix. None of them stores
the FUA write apart from the rest and later fills in older writes, and none
mounts intermediate states as images. That feature of Lunix has no direct
precedent in what I read.

## 6. Consequences for crash images

Setting. ADR-3: a cut at `c` is the state after every write with `seq <= c`;
every commit is a cut. DESIGN §8.3 DELTA: `(last_commit_cut, cut]` records.
Proposal: on a FUA write `X` (seq `x`) with no unfinished FLUSH ahead of it,
commit `X` alone as a DELTA, later commit the older unflushed writes; replay
by highest `seq` per block gives the final state.

**Is each intermediate image legal?** Define F = the last completed FLUSH's
cut. Let U = writes completed after F and before the FUA completes.
Intermediate image I1 = (all writes `<= F`) + `X`. Required of a legal
post-power-loss image by the contract (§1): all writes completed before any
completed FLUSH are present; every completed FUA write is present; any subset
of the rest may be present. I1 contains the first two and an arbitrary
(here: empty) subset of U. So I1 is legal for the same reason a real disk
that destaged only `X` is legal. A later commit adding `U`'s writes gives
`<= F` + `X` + subset of U, still legal, and the final image after all of U
is a cut. Overlap is safe: at any block both written by `x` and by an older
`u` in U, highest `seq` wins, so the older write cannot overwrite `X`'s data
(it would only be a legal outcome if `u` were persisted *before* `X`, which
replay preserves).

**Conditions for that to hold (implementation constraints, not blockers):**
1. **Highest `seq` wins across commits, not commit order.** A later DELTA
   with older seqs must not overwrite newer data in the map. This applies to
   discard, write-zeroes and hole records as well, which are ordered by `seq`
   (DESIGN §3.1, §4.1). If any code path applies "newest commit wins", the
   later commit with `seq 8` would clobber `seq 10`.
2. **A FLUSH must cover `X` and everything before it.** A FLUSH that arrives
   after the FUA commit still has to include all writes `<= its cut`; the
   FUA commit is only a subset of that. Commit chain stays linear (one PUT in
   flight, §8.3), so a FUA-alone DELTA and a FLUSH DELTA are serialised.
3. **Cut bookkeeping.** `last_commit_cut` is no longer a single number: after
   a FUA-alone commit the set of committed seqs is `<= F` plus `{x}`. DELTA
   framing `(last_commit_cut, cut]` must change to an explicit seq set or
   range list, and FULL commits' `cut_seq` must mean "all `<= c` present".
4. **FUA must not be satisfied by an image that lacks something the FLUSH
   before it promised.** Because the block layer completes the FLUSH first
   (§1.1), `X` is dispatched only after F is durable on S3. No hazard.
5. **A FUA write that spans a partially-persisted range** (a multi-block
   write) is committed whole, which is stronger than disks and fine.
6. **Mixed with `write_ack = s3` (write-through).** Every write is already
   durable; FUA-alone is a no-op.

**Does exposing every commit as a mountable image (DESIGN §8.6) change the
answer?** The legality argument is about what a *guest* may see after a
crash, and a mirror or time-travel mount is exactly "mount an image as if
after a crash". So legality is unchanged: a mounted I1 is replayed by the
file system's journal like any crash. What changes is that §8.6's other
properties assumed every commit is a cut:
- "Every completed FLUSH names a seq, so 'the state at the last FLUSH
  before time t' is well defined" (§8.1). That sentence is still true of
  *FLUSH* restore points; a FUA-alone commit adds restore points that are
  not FLUSH points.
- Restore points "one per flush group" (§8.6 table) would include non-cut
  images. A user doing `lunix rollback` or `mount-at --time` to such a point
  gets an image that no *single instant* of the guest's own view ever
  equalled (guest saw `8`,`9` then `10`; image has `10` only). It is legal
  as a crash image but is not what a snapshot with `fs-frozen` or a clean
  `crash` snapshot (a cut) promises. Marking commits as `cut` vs `non-cut`
  and defaulting `--time` to the newest cut would keep the old semantics.
- Mirrors and `TRANSFER` copy commits in order; a mirror that stops after a
  FUA-alone commit is in the I1 state. Legal for the same reason; a failover
  to such a mirror is exactly a power-loss-with-hardware-cache failover.
- Snapshots created by `lunix snapshot` force a cut and FULL (§8.6); not
  affected.
- Retention, GC and `chain_floor` need to understand commits that are not
  contiguous in `seq`.

**Illegal states to avoid.** Any image missing a write `<= F`; any image
where an older write overwrote a newer one at the same block; any image
that contains a FLUSH's cut partially (all of a flush group's writes must be
in one commit, since the FLUSH acknowledges them together). A FUA-alone
commit that omits older *flushed* writes would be illegal; the design above
does not.

## 7. Implications for Lunix

No choice made here. The options, each with its risks:

**Option A: keep the prefix cut for FLUSH and FUA (status quo).**
- Behaviour: FUA waits for a commit of everything completed before it.
- Cost: a bare FUA (`O_DIRECT|O_DSYNC` overwrite) waits behind the whole
  unflushed backlog, up to the 8 MiB DELTA cap or a segment PUT, instead of
  a small PUT. For jbd2/xfs `FLUSH`-then-`W+FUA` pairs the second commit is
  already minimal because the FLUSH just persisted the backlog (§2.6, §3).
- Risk: none for correctness. Matches ADR-3.

**Option B: FUA-only fast path, applied to every FUA write.**
- Behaviour: commit `X` alone as a DELTA (plus the set of seqs already
  committed); older writes follow in later commits.
- Benefit: bounded latency for bare FUA; no benefit for the common
  FLUSH+FUA pairs.
- Risks: needs constraints 1 to 3 of §6 (seq-ordered replay for all record
  kinds, explicit seq sets instead of `(last_cut, cut]`, changed commit
  chain, GC/retention). Introduces non-cut commits, so §8.1 CDP wording,
  §8.6 restore-point table and `mount-at --time` need a cut/non-cut marker.
  ADR-3's sentence "Every commit on S3 is a cut" and "arbitrary subsets at
  sync points" rejection need an amendment: the contract is legal, but the
  reason given for rejection ("ordering matters to journaling file
  systems") is not supported by the jbd2 and xfs code in §3 (they order with
  FLUSH/FUA and checksums). More states exposed means more exposure to
  latent bugs in guest software that real small-cache disks rarely trigger.

**Option C: FUA-only fast path only when no FLUSH is outstanding and the
unflushed backlog exceeds a threshold.**
- Behaviour: take the cheap path only when the prefix cut would be large;
  otherwise prefix cut.
- Benefit: limits non-cut images to the case where they matter for latency.
- Risks: all of B's implementation risks, plus a heuristic and a second code
  path to test. Non-cut images still exist, so §8.6 changes still needed.

**Option D: the FUA-alone commit is internal only, never published as a
restore point.**
- Behaviour: persist `X` for durability (as a journal-like object under the
  same commit chain, or a side object `fua/<seq>`), but mount, mirror and
  `mount-at` only see cuts; recovery on the owner replays side objects on
  top of the last cut by `seq`.
- Benefit: restores §8.6 invariants (every published image is a cut);
  recovery of the owner still gets `X`.
- Risks: new object type, recovery and GC rules, a mirror or failover that
  cannot see the side object loses a FUA-acknowledged write (breaks ADR-26
  copy invariant unless mirrors also replay side objects). Complexity close
  to B.

**Option E: make bare-FUA latency a separate problem.**
- Behaviour: recommend that `O_DIRECT|O_DSYNC` users use `write_ack = s3`
  or accept the prefix cost; document it. In sync mode, a FUA that arrives
  with an in-flight DELTA joins the next flush group as today.
- Risks: leaves bare-FUA performance as is (S3 PUT RTT + backlog).

**Independent observations for the reviewer**
- The block layer serialises FLUSH then FUA for PREFLUSH|FUA (§1.1), so each
  jbd2/xfs commit costs two serial durable steps at Lunix (two PUT round
  trips in sync mode) under any option. Reducing that needs a different
  mechanism (e.g. completing FLUSH only when the following FUA is known
  durable is not possible; Lunix cannot see the pairing, only the
  order).
- Lunix cannot distinguish "FUA without PREFLUSH" from "FUA after the FLUSH
  of the same pair" by flags. Any fast path must be defined by state (is a
  FLUSH outstanding or complete since the last write?), not by request
  flags.
- If Lunix did **not** advertise `BLK_FEAT_FUA`, the kernel would convert
  every FUA into write + FLUSH and iomap's O_DSYNC optimisation would switch
  off (`bdev_fua()` false → `WRITE_THROUGH` cleared). That removes bare FUA
  entirely at the cost of a full flush per `O_DSYNC` write. ADR-3 currently
  advertises FUA.
- A trace would settle the frequency question: run ext4 and xfs on a ublk
  null target (or `blktrace` on a loop device with `BLK_FEAT_FUA` and
  `WRITE_CACHE`) while running pgbench, `fio --direct=1 --sync=1`, sqlite
  WAL inserts, `etcd` bench and a RocksDB `db_bench --sync`, counting `W`,
  `FLUSH`, `W+FUA` and the gap between FLUSH and its following FUA. Not
  done.
- ADR-3 rejection text should be revisited whichever option is chosen: the
  evidence in §3 says journaling file systems order with FLUSH/FUA points,
  not with write order.

## Sources

Local copies of the kernel files were fetched to a scratch directory and are
not part of the repo.

- Kernel (master, 2026-11-12): `Documentation/block/writeback_cache_control.rst`,
  `block/blk-flush.c`, `drivers/block/ublk_drv.c`, `include/uapi/linux/ublk_cmd.h`,
  `fs/iomap/direct-io.c` (also tags v4.17, v4.18, v6.1, v6.6),
  `fs/ext4/fsync.c`, `fs/ext4/file.c` (tags v5.4, v5.5), `fs/ext4/inode.c`,
  `fs/ext4/super.c`, `fs/ext4/fast_commit.c`, `fs/jbd2/commit.c`,
  `fs/jbd2/journal.c`, `fs/jbd2/checkpoint.c`, `fs/jbd2/recovery.c`,
  `fs/xfs/xfs_file.c`, `fs/xfs/xfs_log.c`, `fs/xfs/xfs_log_cil.c`,
  `fs/xfs/xfs_log_recover.c`, `fs/xfs/xfs_iomap.c`, `fs/btrfs/disk-io.c`,
  `drivers/block/{loop,nbd,rbd,virtio_blk}.c`, `drivers/md/dm-writecache.c`,
  `drivers/md/dm-cache-target.c`, `drivers/md/bcache/request.c`,
  `drivers/target/target_core_{file,iblock}.c`,
  `Documentation/admin-guide/ext4.rst`. Base URL
  `https://raw.githubusercontent.com/torvalds/linux/<ref>/<path>`.
- QEMU (gitlab.com/qemu-project/qemu, master, VERSION 11.1.90): `block/io.c`,
  `block/file-posix.c`, `block/linux-aio.c`, `block/io_uring.c`,
  `hw/block/virtio-blk.c`.
- Databases: PostgreSQL `runtime-config-wal.html` and
  `runtime-config-developer.html` (current docs); MySQL 8.4
  `innodb-parameters.html`; SQLite `wal.html`; RocksDB `include/rocksdb/options.h`,
  `env/io_posix.cc` and wiki "Direct-IO"; bbolt `tx.go`, `bolt_linux.go`;
  etcd `server/storage/wal/wal.go`; Redis `src/aof.c`.
- Other: NBD `doc/proto.md`; Ceph `rbd-config-ref`; LSVD paper (EuroSys '22,
  DOI 10.1145/3492321.3524271); <https://github.com/asch/dis>;
  <https://lwn.net/Articles/400541/>; Evan Jones on NVMe durability;
  Zheng et al. FAST '13; Chidambaram et al. SOSP '13; `man 2 sync_file_range`.
