# Local store/cache design, crash consistency and the S3 sync protocol for Lunix

> **Corrections (2026-11-11):** several facts in this note were checked against primary sources in [2026-11-11-fact-check.md](2026-11-11-fact-check.md) (ublk flag kernel versions, provider conditional-write support, prices, latencies, LSVD numbers). Where they disagree, the fact-check wins.

Date: 2026-11-11
Author: research agent (Sonnet)
Question: How should Lunix lay out its local store, make it crash-consistent,
and protocol its write-back to S3 (segments, commit/root pointer, fencing,
sync/async modes), serve reads (cache, eviction, readahead, prefetch), collect
garbage, and behave when the local FS is full? What do S3 economics and
provider limits (especially conditional writes) imply? Alignment with
Constellation's existing S3 commit machinery is wanted where sensible.

Method note: web search was used for the conditional-write matrix; the rest
rests on primary-source knowledge (kernel docs, papers, vendor docs) recalled
from memory and on a read of Constellation's DESIGN/DECISIONS and crate
headers. Items I could not re-verify in this session are in "Open questions /
UNVERIFIED". Prices and latencies are order-of-magnitude, to be re-checked
before they drive a decision.

## Summary

- **Contract**: Lunix is a block device with a *volatile write cache*. Writes
  acked before a completed FLUSH (or FUA write) must survive any crash;
  writes after it may be lost, but never reordered into the middle of earlier
  ones. The S3 image at sync point N is exactly "the device state after
  applying the first n writes of Lunix's own serialization order", where n is
  a flush boundary (or any synthesized cut: a prefix is always a legal
  post-power-loss state). Prefix, not arbitrary subset; overlapping writes
  resolved by sequence number.
- **Sequence numbers everywhere**: assign a global monotonic `seq` to each
  write at dequeue time. The local journal, the map, the S3 commit and
  recovery all key on `seq`. A sync point is just a `seq` cut plus a CoW
  snapshot of the map at that cut.
- **Local layout**: (a)+(b) hybrid, not a general KV and not one big file.
  An append-only checksummed **journal** (O_DIRECT, preallocated and
  zero-filled, `fdatasync` only on FLUSH, group-committed) for acked-but-
  unsettled writes, plus **local segment files** in the *same format as S3
  segments* for settled/cached chunks. LBA->chunk map is a Constellation-style
  **prolly tree (`mtree`)** over chunk index, giving O(1) snapshots, O(diff)
  deltas and lazy demand-loading on a cold mount. Hash->location index is an
  in-memory table rebuilt from segment footers (fjall/redb only as a fallback
  for very large volumes).
- **Chunk granularity**: default 64 KiB logical chunk (configurable 16 KiB-1
  MiB), 4 KiB-granular journal. Map RAM about 130 MiB per TiB (LBA
  map) plus about 0.8 GiB/TiB for a full hash index at 64 KiB; 4 KiB
  granularity would be 16x worse and is rejected for the settled map.
- **S3 object model**: pack compressed chunks into **~32 MiB immutable
  segments** (8-64 MiB range) with a footer index; per-chunk objects are
  economically wrong at 64 KiB (16M PUTs/TiB = about $80 vs about $0.16). Commits
  are **create-only chain objects** `commits/<seq:016x>` written with
  `If-None-Match: *`, exactly Constellation's pattern, **never overwritten
  keys** (R2 and GCS rate-limit same-key rewrites to ~1/s).
- **Fencing**: epoch + create-only slot. A new owner CAS-claims the epoch
  (lease object, `If-Match`/`If-None-Match`), then publishes an epoch-marker
  commit at the next free slot; a zombie can upload harmless immutable
  segments but cannot publish, because its slot is taken. Safety never relies
  on clocks (Constellation ADR-12).
- **Conditional writes are mandatory for the arbiter backend, optional for
  mirrors.** Supported (documented): AWS S3 (If-None-Match Aug 2024,
  If-Match Nov 2024), MinIO, R2, OVH (docs, June 2026), Scaleway, CoreWeave,
  Exoscale. GCS needs `x-goog-if-generation-match: 0` (not the S3 header).
  Backblaze B2 reportedly *rejects* `If-None-Match`; Ceph RGW, Wasabi, Hetzner
  unconfirmed. So: preflight-probe every backend; a backend failing the probe
  can be a mirror target but never the arbiter.
- **Sync mode** = FLUSH waits for one PUT. Collapse segment+commit into one
  **WAL object** (data + commit record, created `If-None-Match`) so a flush is
  1 RTT (S3 Standard ~20-60 ms p50, p99 100-300+ ms; Express One Zone
  single-digit ms). Group-commit flushes: one in-flight PUT per volume caps at
  about 1/RTT commits/s; at 100 PUT/s the request bill alone is about $1,300/month on
  Standard. **Async mode** = pipelined cuts every T seconds or B dirty bytes,
  with hard backpressure at the data-loss budget.
- **Cache**: S3-FIFO (or SIEVE) for clean chunks; dirty/unsynced and
  in-flight data are *outside* the evictable set. Sequential detection,
  doubling readahead, coalesced ranged GETs (merge gaps below ~1 MiB), hedged
  reads after ~p95 latency with a budget. Whole-segment GET only when most
  of the segment is wanted.
- **GC**: segment-level, mark from retained commits' map roots, live-ratio
  compaction with LFS cost-benefit scoring, two-phase condemn -> grace ->
  recheck delete fenced on the lease. Because the single writer owns both
  commit and GC, the dedup-vs-delete race is removed by running GC inside the
  lease holder serialized with commits; keep Constellation's horizon only for
  opt-in cross-volume dedup.
- **ENOSPC**: stall first, error later (dm-thin model). Reserve space at ack
  time, keep a metadata/journal reserve, evict clean cache before stalling,
  stall with a configurable timeout (default 60 s like dm-thin), then fail
  writes with ENOSPC (reads keep working). Never turn a stall into a silent
  drop. The block layer carries only a status code; filesystems treat
  ENOSPC and EIO the same way, so "stall" is the safer default for guest FS
  integrity.

## 1. Block semantics and the consistency contract

### 1.1 What the kernel/filesystems actually rely on

- Since Linux 2.6.37 there are no barriers; the only ordering primitives are
  `REQ_PREFLUSH` (flush the device's volatile cache *before* this bio, i.e.
  everything completed earlier is durable) and `REQ_FUA` (this write itself
  is durable on completion). A device that declares no volatile cache gets
  neither flag and must make every acked write durable. (Kernel doc:
  `Documentation/block/writeback_cache_control.rst`.)
- Ordering between *concurrent* in-flight writes is not guaranteed by the
  block layer; software waits for completion before issuing a dependent write,
  and uses PREFLUSH|FUA to order durability. Therefore a device is correct if
  (1) completed-before-flush-issued writes are durable at flush completion,
  (2) FUA writes are durable at completion, (3) a single logical block write
  is atomic with respect to crash (no torn sectors).
- ext4/jbd2: writes journal blocks, then the commit block with
  PREFLUSH|FUA (or flush before/after when no FUA); checksum-protected commit
  blocks (`journal_checksum`/async commit) let recovery detect a torn
  transaction. xfs: log writes with PREFLUSH/FUA, checkpoint before metadata
  writeback. btrfs: CoW + superblock written with FUA/flush after the tree
  is on disk. All three tolerate *any subset* of post-last-flush writes being
  lost, which means a prefix cut is always safe.
- Databases: Postgres `fsync`/`fdatasync` WAL (or `open_datasync` = O_DSYNC,
  which becomes a FUA write), `full_page_writes` to survive torn 8 KiB pages
  over 4 KiB atomic sectors. InnoDB with `O_DIRECT` + doublewrite buffer +
  `fsync`. They need only: flush semantics + 512 B/4 KiB write atomicity.
- Advertise `logical_block_size = physical = 4096` (or 512 for legacy
  guests: decide, 4 KiB is best for the map) so sub-sector tearing is never
  possible. Multi-block writes may be partially applied after crash per the
  contract; the journal makes them *effectively atomic per request* (one
  checksummed record), which is stronger than required and free.

### 1.2 ublk specifics (verify against `include/uapi/linux/ublk_cmd.h` on 7.0)

- Declare `UBLK_ATTR_VOLATILE_CACHE` so the block layer sends
  `UBLK_IO_OP_FLUSH`; declare `UBLK_ATTR_FUA` to receive FUA writes.
  Without VOLATILE_CACHE, no flushes are sent and the daemon would owe
  durability on every ack, which kills the write-back model.
- Handle `UBLK_IO_OP_DISCARD` and `UBLK_IO_OP_WRITE_ZEROES`. Contract we
  provide: after a discard or write-zeroes the range reads as zero
  (deterministic; some tools such as `mkfs` + lazy init and `blkdiscard -z`
  assume it). Implementation: map entries become the "zero" sentinel; zero
  chunks are never stored. Partial-chunk discard turns into RMW with zero fill
  only when the surviving part is non-zero.
- Use `UBLK_F_USER_RECOVERY` (reissue variant): a daemon crash/upgrade
  then does not surface as EIO to the guest filesystem; in-flight requests are
  re-issued after the new daemon replays the journal. This makes the *ack
  rule* important: ack a write only after its journal record is in the device
  (O_DIRECT completion) so a daemon SIGKILL cannot lose acked writes.
- Error mapping: the ublk result is a negative errno; I believe the driver maps
  `-ENOSPC` -> `BLK_STS_NOSPC`, `-EAGAIN` -> `BLK_STS_AGAIN`, others ->
  `BLK_STS_IOERR` (UNVERIFIED; grep `ublk_drv.c` for `ublk_..._res`/`errno_to_blk`).

### 1.3 Defining "S3 holds a crash-consistent image as of sync point N"

Let the daemon assign every write/discard/zeroes request a strictly increasing
`seq` when dequeued from the ublk queue (single atomic counter across queues).
Let `A(t)` be the set of requests acked before time `t`. Define the
*serialization* `S = (r_1, r_2, ...)` ordered by seq. A **cut** is an index
`n`. The image `I(n)` = initial state with `r_1..r_n` applied in seq order
(later seq wins on overlap).

Sync point `N` is *valid* iff:
1. **Prefix closure**: `I(N)` includes every request with `seq <= n_N` and none
   with `seq > n_N`.
2. **Ack closure**: for every FLUSH that completed (was acked) before sync
   point `N` was *published*, all requests acked before that FLUSH was issued
   have `seq <= n_N`. (Satisfied automatically by choosing `n_N` >= the
   high-water seq of the last completed flush; any larger prefix is also legal
   because it is a state a power loss could have produced after an in-order
   cache drain.)
3. **FUA closure**: any request with FUA acked before publication has
   `seq <= n_N`.
4. **Atomic publication**: the commit that names `I(N)` becomes visible in one
   conditional S3 write; before it, the previous image is still the current
   one. A reader mounting after any crash sees exactly one complete `I(n)`.

Requests in flight at the cut (dequeued, not acked) fall on either side of the
cut by their seq; reads issued by the guest after those writes complete see
them via the local state, consistent with seq order. The cut in sync mode is
the seq at FLUSH arrival (flush waits until `I(n)` is published). In async
mode it is "now" (current high-water seq whose journal records are all
complete), no guest flush needed. This is crash-consistent, not
application-consistent (no `fsfreeze`); an optional `freeze-and-sync` control
command can add the latter for CSI VolumeSnapshots.

Failure mode this prevents: uploading chunks "as they are dirty" in LBA order
yields a *non-prefix* mix (new journal commit block without its data blocks)
and an unmountable ext4/xfs. The CoW-map-at-cut approach yields a state
indistinguishable from a power loss.

## 2. Local layout

### 2.1 Options compared

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| (a) log-structured journal + block map (LSVD / bcache-ish) | sequential writes, natural seq cuts, tiny write amp, same shape as S3 segments | needs map persistence, GC of local log | **chosen core** |
| (b) chunk files in a dir tree | trivially simple, per-chunk eviction = unlink | one file per 64 KiB chunk = inode + dir-entry + journal cost, fsync per file, slow `unlink`, millions of files | no (fine for >=4 MiB chunks like Constellation, not here) |
| (c) one big preallocated file + allocator | no FS metadata overhead | in-place updates break prefix cuts, needs its own crash-safe allocator and torn-write handling, sparse-file/thin-FS surprises | no as primary store |
| (d) embedded KV (fjall/RocksDB/redb/sled) | crash-safe, off the shelf | double logging (KV WAL + our journal), write amp, fsync per commit (redb), sled unmaintained, RocksDB is C++ | only for the optional hash index |

Prior art: **LSVD** (Hajkazemi et al., "Beating the I/O bottleneck: a case for
log-structured virtual disks", EuroSys 2022) is exactly this design: local
SSD write log + object-store segments + prefix-consistent S3 image; it
shows 8-32 MiB-class objects and a checkpointed extent map. bcache/dm-writecache/
dm-cache solve the local-cache half; ZFS ZIL/SLOG is the same
fast-journal-then-settle split.

### 2.2 Recommended on-disk structure

```
<dir>/
  vol.meta            # volume id, chunk size, device size, epoch, backend list (atomic rename)
  journal/NNNNNNNN.j  # 64-256 MiB, preallocated + zero-filled, O_DIRECT
  seg/<id>.seg        # settled local segments, S3-segment format, append then immutable
  map/ckpt-<seq>      # map checkpoint (mtree root + node pack refs) ; map nodes in seg-like packs
  idx/                # optional hash-index checkpoint
```

Journal record (4 KiB aligned): `magic | seq | lba | len | flags(zero/discard/
fua) | payload crc32c/xxh3 | header crc` then payload (compressed or raw, 4 KiB
padded). Recovery = scan from the last checkpoint, accept records while
`seq` is contiguous and checksums verify; stop at the first gap/bad record
(prefix property: a gap can only hold unacked-before-flush writes, because a
FLUSH ack requires all earlier writes completed).

Rules:
- **Ack rule**: ack a write when its journal record's O_DIRECT write CQE
  arrives (survives daemon crash; not necessarily host power loss — that is
  what FLUSH is for).
- **FLUSH**: wait until all writes with seq <= flush_seq have completed, then
  one `fdatasync` on the journal (group commit across all pending flushes and
  FUA writes), then ack. With O_DIRECT on a *fully written* (zero-filled, not
  merely `fallocate`d) file, `fdatasync` has no metadata to journal and costs
  one device FLUSH. `fallocate`d unwritten extents cost an extent-conversion
  transaction on first write: zero-fill once at creation.
- **FUA write**: treat as write + the next group-commit fdatasync (or
  `O_DSYNC` fd on io_uring for that write; xfs/ext4 dio use a FUA bio when
  no metadata is dirty).
- io_uring: use `IOSQE_IO_LINK` for write->fdatasync pairs only where
  unrelated writes need no ordering; use explicit completion accounting for
  the "all earlier writes done" condition (linking does not order against
  other submissions; `IOSQE_IO_DRAIN` is a blunt alternative). Registered
  buffers/files, SQPOLL optional.
- Costs (order of magnitude, UNVERIFIED on this hardware, measure): device
  FLUSH ~20-50 us on NVMe with power-loss protection (often a no-op),
  0.3-3 ms on consumer NVMe, 1-10+ ms SATA SSD, ~10 ms HDD. Hence group
  commit and never `fsync` per write. ext4 `data=ordered` vs xfs behave
  the same for this access pattern; btrfs should be avoided for the journal
  (CoW + `fsync` tree log) — document "ext4/xfs recommended".
- **Settling**: a background task consumes journal records <= the last
  published cut (and later), hashes (BLAKE3, already in Constellation's
  workspace), zero-detects, dedups against the hash index, compresses,
  appends new chunks to the open *local segment*, updates the map, and
  advances a journal truncation point. Journal space is reclaimed by deleting
  whole journal files once everything in them is settled *and* a map
  checkpoint covers it. Reads of fresh data hit an in-memory ring/page index
  of the journal before settling.
- Chunk RMW: a 4 KiB write into a 64 KiB chunk is held as an overlay extent
  until settled; settling merges overlays into the base chunk (read from
  cache/S3 if absent). This is the main cost of coarse chunks: cold RMW needs
  a ranged GET. Mitigation: coalesce overlays for a window before settling;
  and a "sub-chunk dirty bitmap" lets whole-chunk writes (the common FS
  case) skip the read.

### 2.3 Map persistence and memory

- Key = chunk index (u64), value = chunk id/ref (hash prefix + segment id +
  offset/len, or "zero"). **Use `mtree`** (prolly tree, canonical shape,
  `diff` O(delta), root hash = snapshot, lazy nodes via `NodeStore`): a sync
  point is a root hash; the S3 map snapshot is "upload new nodes since last
  root" (packed); a cold mount reads the root and demand-loads nodes - no
  multi-hundred-MiB map download before first I/O. In RAM keep a hot node
  cache plus the full overlay of unsettled changes.
- Local persistence: checkpoint = mtree root + dirty node packs written
  periodically (e.g. every 64 MiB of journal or 30 s) and `fdatasync`ed, then
  journal replay from checkpoint seq. A lost/corrupt checkpoint falls back
  to the previous one (keep 2) or to a full replay.
- Sizes (unique-chunk upper bounds, ref ~16 B with a shared chunk table):

| Granularity | Entries per TiB | Flat LBA map (8 B) | Hash index (~48 B/unique) |
|---|---|---|---|
| 4 KiB | 268M | 2 GiB | 12.9 GiB |
| 16 KiB | 67M | 512 MiB | 3.2 GiB |
| 64 KiB | 16.8M | 128 MiB | 0.8 GiB |
| 256 KiB | 4.2M | 32 MiB | 200 MiB |
| 1 MiB | 1.05M | 8 MiB | 50 MiB |

  Run-length compression of sequentially written regions (consecutive
  chunk ids in one segment collapse to an extent) typically cuts the LBA map
  by 5-50x; the hash index is only needed for chunks you want to dedup
  *against*, so it can be a bounded cache (recent N chunks + bloom filter),
  trading dedup ratio for RAM. Default 64 KiB; the 4 KiB journal granularity
  is *not* mirrored in the settled map.
- Compression (zstd-1/lz4) happens at settle, off the ack path; the journal
  stores raw (or lz4 if CPU is free) so write latency is flat.

## 3. Write-back to S3

### 3.1 Segment packing

- Segment = header | compressed chunk bodies (each with its own checksum) |
  footer index (hash, offset, len, flags per chunk) | footer length + magic.
  Size target **32 MiB** (8-64). Rationale: PUT cost per GB falls linearly
  with size, ranged-GET granularity is chunk-sized anyway, and 8-16 MiB
  ranges are AWS's recommended GET unit. A single PUT is fine up to 5 GiB;
  multipart (parts >= 5 MiB, <= 10,000 parts) only for pipelining segments
  above ~64 MiB, so default to single PUT for simplicity and cheaper
  failure retry.
- Name: `segs/<volume>/<segid>` where segid is a random/ULID, not content
  hash (segments are not deduped; chunks are). Keys sharded by high random
  bits to spread load across S3 partitions (AWS partitions on key string;
  see Constellation DESIGN §2).
- Sync-mode small flushes cannot wait to fill 32 MiB: write a **WAL
  object** `wal/<seq>` (data+commit record, may be tens of KiB) and let
  background compaction fold WAL objects into normal segments; the WAL
  objects are garbage once superseded by a retained commit's segment.
- Segment upload concurrency: reuse Constellation's `upload-concurrency`
  (AIMD goodput search; RTT-dominated paths need many in flight).

### 3.2 Commit chain and root pointer

Reuse Constellation's primitive (`commits/<seq:016x>`, CAS-created, zero-padded
hex so LIST-with-offset is a seek; `store-s3/src/cas.rs` conditional-PUT error
classification incl. 409/412 and "our own write landed" verify-by-body).

```
<prefix>/<vol>/
  vol.json                 # immutable creation params (chunk size, enc, uuid)
  epochs/<epoch:016x>      # create-only ownership claims (If-None-Match)
  commits/<seq:016x>       # create-only; the chain
  head                     # OPTIONAL non-authoritative hint (plain PUT; not for correctness)
  segs/<..>  wal/<seq>     # immutable data
  maps/<root-hash>/...     # mtree node packs (immutable, content-addressed)
  snaps/<name>.json        # named snapshots (pointers to a commit seq)
  gc/condemned.json
```

Commit body: `{epoch, seq, parent_hash, journal_cut_seq, device_size, chunk_size,
map_root, new_segments[], new_map_packs[], created_at, writer_id, crc}`.
Publish order: (1) PUT segments + map packs (parallel, plain or
`If-None-Match`); (2) PUT `commits/<seq>` with `If-None-Match: *`. Visibility
= existence of the commit object. Readers find head by LIST `commits/` from a
hint and follow to the end (or read `head` hint and probe `seq+1`).
Why create-only chain rather than "overwrite one root object with If-Match":
(i) works on any provider with create-if-absent (the weakest common
primitive), (ii) no same-key overwrite rate limits (R2 and GCS ~1 write/s
per key), (iii) the history is the snapshot list and the fencing slot at the
same time, (iv) no ETag aliasing issues across mirrored copies.

### 3.3 Epoch fencing for the single writer

- Mount: read chain tail (seq k, epoch e). Claim epoch: `PUT epochs/<e+1>`
  with `If-None-Match: *`; losing = someone else is mounting -> refuse or
  wait. Then publish an **epoch-marker commit** at slot k+1 (create-only). A
  previous owner that is still running (paused VM, partitioned node) can still
  upload segments but its next commit targets slot k+1 and fails with 412;
  it must then self-fence (stop acking flushes, fail I/O with EIO or
  detach). This is Constellation's takeover-marker approach and needs no clock
  for safety.
- Liveness/lease: optional `lease.json` (If-Match renewal, ~60 s TTL like
  Constellation) so a crashed owner is taken over automatically after the TTL
  and a graceful unmount releases immediately. Safety stays CAS-based; the
  TTL only decides *when* to attempt takeover. Forced takeover is an
  explicit admin flag (data loss budget = writes after the old owner's last
  published commit).
- CSI: a pod move = unmount (flush + final commit + release lease) at the old
  node, mount at the new; for node death use TTL or `--force`.
- Without conditional writes (backend fails the probe): single-writer by
  *policy* only; emit a loud warning, require `--unsafe-no-fencing`, and use
  `x-amz-version-id`/listing heuristics only for detection. Such a backend can
  still be a mirror.

### 3.4 Async mode: sync interval and backpressure

- Parameters: `sync_interval` T (seconds) and `max_unsynced` B (bytes of
  journal not yet covered by a published commit), plus `max_unsynced_age`.
  Cut triggers: T elapsed, or dirty >= B/2, or explicit `sync`, or unmount.
- Pipeline: the cut is a CoW mtree snapshot (O(1)), so writes continue; cut
  N+1 may upload while N is still in flight, but commits publish strictly in
  order (parent hash chain), so a failed N blocks N+1's publication but not
  its data upload.
- Backpressure: soft threshold at ~75% of B: inject delay proportional to
  overshoot (token bucket sized by measured upload goodput); hard limit
  at B: stall new writes (not reads, not flushes of already-covered data).
  Expose RPO metrics (`unsynced_bytes`, `oldest_unsynced_age`,
  `last_commit_seq/age`). If S3 is unreachable: continue to B then stall (or
  switch to `on_s3_down = fail|stall|degrade-to-local-only` configurable;
  local-only means the RPO budget is knowingly exceeded and alerts fire).
- Upload ordering within a cut: segments for the cut are independent; order
  by LBA locality is irrelevant for correctness, but pack by *LBA locality*
  where possible to make later sequential reads cheap.

### 3.5 Sync mode latency

Each FLUSH -> publish a cut covering all writes so far. Cost = 1 round trip
if data+commit fit in one WAL object (create-only PUT), else segment PUT
then commit PUT (2 sequential RTTs; parallelize segments, commit last).
Order-of-magnitude PUT latencies for small objects (UNVERIFIED, measure per
region; AWS says "100-200 ms" for small requests in its performance
guidelines but real same-region Standard is usually faster):

| Backend | p50 small PUT | p99 | Notes |
|---|---|---|---|
| S3 Standard same-region | ~15-40 ms | 100-300+ ms | tail from first-byte jitter, retries |
| S3 Express One Zone | ~3-8 ms | ~15-30 ms | single AZ; not portable; conditional writes behaviour must be checked |
| Cloudflare R2 | ~30-100 ms | 200-500 ms | strongly consistent; same-key write limit ~1/s |
| OVH (Constellation measurement) | ~200 ms observed round (poll p50 ~210 ms) | 250-350 ms | from `DECISIONS.md` §campaign results, EU->Milan |
| MinIO local | ~1-5 ms | <20 ms | depends on disks/erasure |

Implications: group commit (all FLUSHes arriving while a commit is in flight
ride the next one) bounds commit rate at about 1/RTT per pipeline slot; allow 2-4
overlapping WAL PUTs only if each has a distinct slot and publishes in order
(commit slot k+1 cannot exist without k; since create-only slots are unordered
in S3, enforce by chaining: PUT k+1 only after k succeeded, or accept
sparse-slot-with-parent-check in recovery - prefer sequential, group-
committed). Cost: 100 commits/s x 86400 x 30 = 259M PUTs/month = about $1,300 at
$0.005/1000. Offer `sync_min_interval` (e.g. 10 ms) to cap spend, and warn at
create time. Hedge slow sync-mode PUTs? Hedging a create-only PUT is safe
(second one gets 412 if first landed - verify body, as in `cas.rs`).

### 3.6 Mirrors and live backend changes

- Define one **arbiter** backend (conditional-write capable) that owns the
  chain; mirrors receive byte-identical objects (segments, map packs,
  commits) under the same keys, copied by the daemon (or server-side copy
  where same vendor). A mirror is "at seq M" when `commits/<M>` and all it
  references exist there; commit objects are copied *last*.
- Ack policy per volume: `sync_quorum = arbiter` (default), or all. Async
  mirror lag is tracked per backend; eviction of local data requires
  durability on the arbiter at least (config: all backends).
- Attach: snapshot cut N0 -> upload full live set (walk mtree, copy segments
  that contain live chunks; consider compacting-while-copying to drop dead
  weight) -> then the delta commits. Initial sync is paced; volume remains
  writable throughout.
- Graceful removal/migration (US -> Asia): add mirror, wait for it to reach
  head, **arbiter handover** = new arbiter creates `epochs/<e+1>` with a
  marker commit referencing the old chain head, old arbiter becomes mirror
  or is dropped. Cross-vendor with different conditional semantics: only
  the arbiter needs them.
- Cross-region copy egress is the real cost; prefer reading source objects
  from the *local cache* when possible (the local FS likely already holds
  recent data) and rate-limit.

## 4. Read path and cache

### 4.1 Layers

Guest read -> (1) unsettled overlay/journal index -> (2) RAM chunk cache
(decompressed hot) -> (3) local segment files (compressed) -> (4) S3 ranged
GET from segment by (segment id, offset, len) from the map entry, verified by
the chunk hash/AEAD tag. Single-flight per chunk: concurrent misses share one
fetch. Hash-verify every S3 fill (end-to-end integrity).

### 4.2 Eviction policy

Candidates and what the literature says (S3-FIFO: Yang et al., SOSP 2023;
SIEVE: Zhang et al., NSDI 2024; ARC: Megiddo & Modha 2003, patent expired
~2023 per my recollection; CLOCK-Pro: Jiang 2005; W-TinyLFU: Einziger 2017
(Caffeine); 2Q: Johnson & Shasha 1994; LIRS: Jiang & Zhang 2002):

- Block I/O traces (MSR, CloudPhysics, Tencent/Alibaba block traces) are
  scan- and one-hit-wonder-heavy. S3-FIFO's small FIFO (~10%) + main FIFO +
  ghost queue filters one-hit wonders and is competitive with or better than
  ARC/LIRS/TinyLFU on those traces at lower cost, with lock-friendly queues.
  SIEVE is simpler, comparable on web traces, weaker on scans. LIRS is
  excellent on loops/scans but more complex; ARC is solid but has overhead
  and adapts slowly after scans; W-TinyLFU needs a frequency sketch (good
  admission, more moving parts). 2Q is a precursor of S3-FIFO.
- **Choice: S3-FIFO** on chunk granularity. It also aligns with the
  storage layout: FIFO-order eviction frees local segments in roughly
  write order, simplifying local segment reclamation (compact or
  hole-punch `FALLOC_FL_PUNCH_HOLE` dead extents for fine-grained
  reclaim).
- **Pinned/non-evictable**: (a) journal and unsettled overlays, (b) any chunk
  whose segment is not yet committed on the required backends (unsynced),
  (c) chunks being read/uploaded (refcounted), (d) in async mode, anything
  newer than the last published commit. These sit in separate accounting; if
  pinned bytes approach the quota, that is the ENOSPC path (section 7), not
  eviction. A purely local volume (no backend) has *nothing* evictable.
- **Admission**: new fills enter the small queue; prefetched-but-unused
  data enters tagged `prefetched` and is dropped first (prevents readahead
  from polluting); detected long sequential scans (> N MiB run) bypass the
  main queue unless re-hit. Writes: freshly settled chunks are warm (inserted
  into main only if likely to be re-read; default small queue).

### 4.3 Readahead and prefetch

- The guest already does file-level readahead (Linux `read_ahead_kb`
  default 128 KiB; adaptive on-demand readahead with the `PG_readahead`
  async trigger marker, window doubling up to `max_sectors`/ra limit).
  Lunix sees the resulting block requests, which carry less context. So run
  a block-level stream detector: track ~16-64 streams keyed by (last end
  LBA, direction); on N consecutive adjacent hits start prefetch at 1-2
  chunks, double up to a cap (e.g. 8-32 MiB or bandwidth x latency), with an
  async trigger at ~half the window (Linux-style) so the pipe never
  drains. Cancel on stream break.
- Set `ublk` `max_sectors`/optimal IO size large (>= 1 MiB) so guest
  readahead is not chopped.
- S3 fetch plan for a miss set: group misses by segment; **coalesce** ranges
  whose gap < `G` where `G ~= bandwidth x first-byte latency` (single
  stream ~50-100 MB/s x 20-30 ms ~ 1-3 MiB; use 1 MiB default) because
  reading the gap is cheaper than a second request; split ranges above 8-16
  MiB into parallel ranged GETs (AWS best practice) up to the AIMD
  concurrency from `upload-concurrency` (reuse it for downloads).
- Whole segment vs ranged: ranged if wanted_bytes/segment_size < ~0.3 or
  latency matters; whole-segment GET when promoting a cold volume (e.g.
  "warm-up"/pin mode) or when most of a segment is live and wanted -
  cheaper per byte (one request) and populates cache with locality.
- **Locality problem**: log-structured segments are ordered by write time,
  not LBA; a file written randomly but read sequentially fragments. Mitigations:
  GC/compaction rewrites live chunks in LBA order ("re-sequencing"), mtree
  keyed by chunk index lets the planner see physical adjacency and prefetch
  *physically* adjacent chunks of the same segment as a second heuristic.
- **Hedging**: for reads on the critical path, if a ranged GET exceeds the
  recent p95-p99 for its size bucket, issue one duplicate (ideally to a
  different mirror/endpoint if available - cheap tail cutting across the
  mirror set); cap hedges at ~5% of requests via a token bucket; cancel the
  loser (Dean & Barroso, "The Tail at Scale"). Never hedge writes
  except create-only commits (idempotent by body check).
- Cold mount elsewhere: serve I/O immediately with lazy mtree + ranged GETs;
  background warm-up prefetches by recency recorded in the commit
  (optional "hot set hint" object listing most-recently-used chunk ranges at
  last unmount) - cheap, high value for pod migration.

## 5. Garbage collection

- **What can die**: segments/WAL objects/map packs unreachable from retained
  commits and named snapshots; plus *partially dead* segments (overwrites,
  discards, truncate/shrink).
- **Counting**: do not keep exact per-chunk refcounts on S3 (dedup + CoW
  snapshots make decrements non-local and racy). Keep an *approximate*
  per-segment live-bytes counter in the daemon, updated from map diffs at
  each cut (cheap with mtree `diff`) - used only to pick compaction
  candidates. **Exact liveness = mark** from retained roots (walk mtree of
  each retained commit/snapshot; set of referenced (segment, chunk)). Mark
  cost is O(map) but runs rarely and incrementally (diff vs last mark).
  Constellation does the same (mtree reachability walk; `mark.rs`,
  `mtree_gc.rs`).
- **Compaction policy** (LFS cleaning, Rosenblum & Ousterhout 1992):
  score = `(1-u) x age / (1+u)` (cost-benefit; `u` = live fraction, age =
  time since segment last had a death or its newest chunk). S3 cost model
  differs from disks: reading live bytes is a GET + egress (free in-region,
  not cross-region), the new segment a PUT, deletes free; the benefit is
  $/GB-month on dead bytes. Default: compact when `u < 0.5` and age > 1
  h (async mode) / lower bar when storage cost dominates; throttle in
  background; prefer compacting segments already in local cache (no GET).
  Cap write amplification. Also re-sequence by LBA while compacting.
  Mind retention-class minimums (IA 30 d, Glacier etc.): never put segments
  in minimum-duration classes unless the volume is archival.
- **Snapshot retention**: named snapshot = pointer to commit seq (and thus
  root). Retention policy keeps last K commits or last D days at cut
  granularity, plus all named snapshots; clones/branches add roots.
  Thinning (hourly->daily) as in Constellation `snapsched`/`snapexpire`.
- **Safe deletion with in-flight readers**: readers = the owner (knows its
  own working set), read-only followers/snapshot mounts, and mirror
  catch-up copies. Protocol (Constellation's): (1) compute candidate set
  from marks, (2) CAS-publish `gc/condemned.json` (fenced on the GC lease),
  (3) wait >= lease TTL + max follower map-staleness (follower must refresh
  its root at least that often or drop to re-validate), (4) re-mark, remove
  from condemned anything now referenced, (5) delete, renewing the lease
  every N deletes and aborting on lost CAS. A follower that hits a 404 on a
  segment re-reads the head commit and retries (a legitimately GC'd
  object means its root is stale) instead of erroring.
- **Dedup-vs-delete race**: Constellation needs a 7-day horizon because
  any node can dedup-hit a chunk concurrently with GC. For Lunix the
  single writer both commits and GCs, so run GC *as the lease holder,
  serialized with commit publication*: the writer never dedups against a
  condemned chunk (it checks the condemned set before using a dedup hit).
  Cross-volume dedup breaks single-writer ownership; make it opt-in and
  then apply a horizon + condemned handshake exactly as Constellation.
  Also keep orphan sweep: segments uploaded but never referenced
  (crashed before commit) are removed after horizon (older than the oldest
  possible in-flight upload, e.g. 24 h).
- **LIST consistency**: S3 has been strongly consistent for LIST/GET since
  Dec 2020; R2, MinIO, GCS, Backblaze are also strongly consistent in
  current documentation (UNVERIFIED per provider). Correctness never
  depends on LIST: reachability comes from commits/maps; LIST only
  feeds the orphan sweep and head discovery (with the `head` hint + probing
  `seq+1`, `seq+2` as a double check). If a backend has stale LIST, the
  orphan sweep simply lags.

## 6. S3 economics and limits

### 6.1 Request limits and sizes

- AWS: 3,500 PUT/COPY/POST/DELETE and 5,500 GET/HEAD per second per
  partitioned prefix, auto-splitting (transient 503 SlowDown; retry with
  backoff). Prefix = arbitrary key string, so random-hash key spreading works
  at any depth. Lunix's request rate is orders lower; the practical limits
  are latency and the per-request cost.
- Single PUT <= 5 GiB; multipart parts 5 MiB-5 GiB, <= 10,000 parts, object
  <= 5 TiB. Ranged GET best practice 8-16 MiB per range, parallel.
- R2: no per-prefix limit documented but ~1 write/s to the *same key*;
  GCS: ~1 write/s per object name (documented update limit); multipart and
  XML API preconditions differ (below).

### 6.2 Prices (approximate, from memory, UNVERIFIED - re-check)

| Provider | PUT/Class A | GET/Class B | Storage | Egress |
|---|---|---|---|---|
| AWS S3 Standard (us-east-1) | $5.00/M | $0.40/M | $0.023/GB-mo | $0.09/GB internet; free in-region |
| AWS S3 Express One Zone | ~$1.13/M (2025 cut) + per-GB upload fee | ~$0.03/M + retrieve fee | ~$0.11/GB-mo | free in-AZ |
| Cloudflare R2 | $4.50/M | $0.36/M | $0.015/GB-mo | $0 |
| Backblaze B2 | free uploads | $0.40/M (Class B) | ~$6-7/TB-mo | free up to 3x stored |
| GCS Standard | $5.00/M (Class A) | $0.40/M (Class B) | ~$0.020-0.026/GB-mo | $0.08-0.12/GB |
| OVH Object Storage | low/none on some tiers | low/none | ~EUR 6-7/TB-mo | tiered |
| Wasabi | no request fees | none | ~$7/TB-mo, 90-day min retention | free "fair use" |
| Hetzner Object Storage | no request fees (flat plan) | none | ~EUR 5/TB-mo incl. base | included quota |

Takeaways: at about $5/M PUTs, a 1 TiB volume with 64 KiB objects costs $80 to
write once vs about $0.16 with 32 MiB segments; this is why segments are
non-negotiable. GET cost is small ($0.40/M) so ranged GET per chunk miss is
fine economically; the dominant read cost is latency. Async sync every 30 s
= 2,880 commit PUTs/day = $0.43/month, negligible; sync mode is the expensive
one. Minimum-duration/Wasabi-style retention punishes GC: warn at attach.

### 6.3 Conditional write support matrix (as found; verify with `probe`)

| Provider | create-only (`If-None-Match: *`) | CAS (`If-Match`) | Notes |
|---|---|---|---|
| AWS S3 | yes (Aug 2024) | yes (Nov 2024) on PutObject and CompleteMultipartUpload; CopyObject later (Oct 2025) | 412 on failure; 409 on concurrent conditional write; bucket policy can enforce |
| Cloudflare R2 | yes | yes | custom header for conditional copy |
| MinIO | yes (since 2023) | yes | full RFC 7232 |
| GCS | via `x-goog-if-generation-match: 0` (XML API) or JSON `ifGenerationMatch=0`; S3-style `If-None-Match` on write not per docs | `x-goog-if-generation-match: <gen>` | preconditions unsupported on XML multipart uploads; needs a GCS-specific code path or the JSON API |
| OVH | yes (docs June 2026), `*` only | yes | earlier Proxmox report of 501 - may depend on region/tier/date |
| Scaleway, CoreWeave, Exoscale, Servercore, Selectel | documented yes | mostly yes | |
| Backblaze B2 (S3 API) | reportedly *rejects* the header (Proxmox quirk `skip-if-none-match-header`, Jul 2025) | unknown | UNVERIFIED current status |
| Ceph RGW | not confirmed | not confirmed | check release notes / `rgw_rest_s3.cc` |
| Wasabi, Hetzner | not confirmed | not confirmed | probe |

Hence: `lunix backend add` runs Constellation's `probe_cas_semantics` (create,
re-create must 412/409, If-Match stale must 412, concurrent) and records
capabilities and provider error-code quirks in the backend config.

## 7. Out of space on the local FS

- A block device can only return a completion status: errno -> `BLK_STS_*`.
  `ENOSPC` maps to `BLK_STS_NOSPC` ("critical space allocation"; dm-thin uses
  it), anything else becomes EIO. Filesystems above do *not* specially treat
  NOSPC from the bdev: ext4 writeback errors set `AS_EIO`/`AS_ENOSPC` on the
  mapping (fsync returns it) and a metadata/journal write failure aborts the
  journal and triggers `errors=remount-ro|panic`; xfs shuts down on log I/O
  error. So returning *any* write error for metadata is catastrophic for the
  guest FS, which is why the default is to stall.
- **dm-thin model** (Documentation/admin-guide/device-mapper/thin-provisioning
  .rst): on no free data blocks, I/O is queued; `no_space_timeout`
  (`/sys/module/dm_thin_pool/parameters/no_space_timeout`, default 60 s) after
  which queued writes error; `error_if_no_space` errors immediately; a
  **low water mark** event wakes userspace to extend the pool; reads and
  discards continue; a metadata-space reserve exists so the pool can always
  commit.
- **Lunix policy**:
  - Config: `free_floor` (bytes or %) + `on_full = stall(timeout) | enospc |
    eio`. Default `stall` with 60 s timeout, then `ENOSPC`; per-volume.
  - Reserve: journal + checkpoint + a compaction/commit reserve are
    *always* carved out of the floor so FLUSH, recovery and map commits
    never need new space. Writes are admitted only if the journal can hold
    them; account at ack time (never ack and then fail to persist).
  - Ladder when approaching the floor: evict clean cache (S3 attached) ->
    force a cut and drain unsynced data to S3 so it becomes evictable ->
    throttle -> stall -> error. No-backend volume: skip straight to
    stall/error since nothing is reclaimable (but `fstrim`/discard in the
    guest frees space: expose that in docs; dedup+zero detection also
    shrinks usage).
  - Low-water event: emit metric/log/webhook + optional control-socket
    notification at e.g. 85% and 95% of (device capacity or free-floor)
    so an operator/CSI can grow the backing FS.
  - Reads never fail due to space; discards and write-zeroes (which free
    space) are always accepted.
  - After the hard error state, writes return `-ENOSPC`; when space
    reappears (floor satisfied again) the device automatically resumes. The
    ublk status is re-evaluated per request (no persistent "failed" latch
    unless configured, to avoid the XFS-shutdown/ext4-remount-ro trap
    persisting longer than necessary - note the guest FS may already have
    gone read-only; document recommending `errors=remount-ro`).
  - ENOSPC from the *host* FS in the middle of a journal append (reserve
    mis-sized, other tenants of the FS) must be treated as the same ladder,
    never as device corruption; EIO from the host FS (real media error)
    is a hard failure: stop acking, flip the device to a failed state and
    surface EIO, since local data may be lost.

## Recommendations for Lunix

1. **Contract**: advertise volatile write cache + FUA + discard +
   write-zeroes, 4 KiB blocks, user-recovery enabled. Global write `seq`;
   all state transitions keyed on it.
2. **Local**: O_DIRECT journal (zero-filled, preallocated, checksummed,
   contiguous-seq recovery) with FLUSH = group-committed `fdatasync`;
   settle off the ack path into S3-format local segments (BLAKE3, zero-skip,
   dedup, zstd). Map = `mtree` keyed by chunk index; checkpoint every
   ~64 MiB of journal. Default chunk 64 KiB. Avoid btrfs for the store.
3. **Sync point** = `(seq cut, mtree root)` via CoW snapshot. Commits are
   a create-only chain `commits/<seq>` with `epoch`, `parent`, `journal_cut_seq`,
   `map_root`; data (segments, map packs) uploaded first; no mutable
   root object (optional `head` hint only).
4. **Segments** 32 MiB default; WAL objects for sync-mode small flushes;
   reuse `upload-concurrency` for upload *and* download concurrency.
5. **Fencing**: epoch objects + epoch-marker commit; optional 60 s lease for
   auto takeover; zombie writers fail at their next commit slot and
   self-fence. Require conditional writes on the arbiter; probe at attach;
   mirrors may be CAS-less.
6. **Modes**: async default (T=30 s, B configurable; soft throttle then hard
   stall); sync mode with 1-RTT WAL commits and group commit; warn about the
   request bill; consider an Express One Zone fast path as an *optional
   accelerator* later (Constellation rejected it as non-portable; here it
   would be a write-ahead tier with a portable fallback, a deliberate
   deviation to evaluate).
7. **Cache**: S3-FIFO, pinned dirty/unsynced set accounted separately,
   prefetch-tagged admission, stream-detector readahead with coalesced
   ranged GETs (gap 1 MiB, 8-16 MiB split), hedging with budget, hash-verify
   fills, warm-up hint on migration.
8. **GC**: mark from retained roots + approximate per-segment liveness for
   candidate choice; cost-benefit compaction (u<0.5 default) re-sequencing
   by LBA; condemn -> grace -> recheck -> delete, fenced on the lease; GC
   executed by the lease holder; horizon only for cross-volume dedup.
9. **Out-of-space**: stall-then-ENOSPC ladder with reserve, as above;
   host-FS EIO is fatal, host-FS ENOSPC is the ladder.
10. **Mirrors**: single arbiter chain, byte-identical objects, commit copied
    last; handover via epoch marker for graceful removal.

## Open questions / UNVERIFIED

- Exact ublk 7.0 attribute names/flags and the errno->`blk_status_t` mapping
  in `ublk_drv.c`; whether ublk supports atomic-write (RWF_ATOMIC) limits.
- All latency figures and prices above are recalled, not freshly measured
  or fetched; R2/OVH numbers especially. Run a probe benchmark per backend
  (1 KiB / 64 KiB / 4 MiB PUT, ranged GET) - Constellation's `bench/` or
  `uploadbench` could be reused.
- Conditional-write status for Backblaze B2, Ceph RGW (which release),
  Wasabi, Hetzner; GCS S3-API header behaviour; OVH regional/tier rollout;
  Express One Zone conditional-write semantics (directory buckets).
- Whether `mtree` fits integer-key block maps efficiently (its key
  encoding is oriented to inode/dentry keys; check node sizing, per-leaf
  aggregate hooks, and whether `NodeStore` supports the lazy-load pattern
  needed for a cold mount) - read `crates/mtree` API in detail.
- Memory figure for the hash index assumes 48 B/entry; real table overhead
  (load factor) and whether to bound the dedup index need prototyping.
- S3-FIFO vs SIEVE vs W-TinyLFU on *block* traces with a segment-FIFO local
  layout: needs trace-driven simulation (MSR/Tencent/Alibaba traces) before
  committing.
- ARC patent expiry date (IBM US 6,996,676) - not verified; irrelevant if
  S3-FIFO is chosen.
- Whether `IOSQE_IO_LINK`ed write->fdatasync on O_DIRECT is faster than the
  explicit group-commit path on this kernel; measure.
- Interaction of dm-crypt/LUKS or overlay layers under the journal: FLUSH
  passthrough must be verified on any stacking.

## Sources

- Local: `/home/bra/cvs/replicix/lunix/docs/explanation/research/BRIEF.md`
- Local: `/home/bra/cvs/replicix/constellation/docs/explanation/DESIGN.md`
  (§1 hard constraints, §2 bucket layout, §14 GC), `DECISIONS.md`
  (ADR-12, ADR-27, OVH/AWS latency campaigns),
  `crates/store-s3/src/cas.rs` (conditional PUT codes), `lease.rs`
  (60 s TTL), `gc.rs`, `mark.rs`, `crates/mtree/src/lib.rs` (prolly tree),
  `crates/upload-concurrency/src/lib.rs` (AIMD), `crates/engine/src/prefetch.rs`
- AWS conditional writes: https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html ;
  https://aws.amazon.com/about-aws/whats-new/2024/08/amazon-s3-conditional-writes ;
  https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes ;
  https://aws.amazon.com/about-aws/whats-new/2025/10/amazon-s3-conditional-write-functionality-copy-operations ;
  https://aws.amazon.com/blogs/storage/building-multi-writer-applications-on-amazon-s3-using-native-controls
- MinIO: https://www.min.io/blog/leading-the-way-minios-conditional-write-feature-for-modern-data-workloads
- R2/MinIO via object_store: https://docs.rs/crate/object_store/0.11.0/source/src/aws/precondition.rs
- OVHcloud: https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/s3-conditional-writes ;
  Proxmox forum report: https://forum.proxmox.com/threads/proxmox-backup-server-s3-backend-%E2%80%93-%E2%80%9Cconditional-object-puts-are-not-supported%E2%80%9D-ovh-object-storage.169520/
- Backblaze quirk (Proxmox patch): https://lists.proxmox.com/pipermail/pbs-devel/2025-July/014234.html
- GCS preconditions: https://docs.cloud.google.com/storage/docs/request-preconditions
- Scaleway: https://www.scaleway.com/en/docs/object-storage/api-cli/using-conditional-writes/ ;
  CoreWeave: https://docs.coreweave.com/products/storage/object-storage/buckets/conditional-requests ;
  Exoscale: https://www.exoscale.com/blog/exoscale-sos-conditional-write/
- Kernel docs (recalled, not re-fetched): `Documentation/block/writeback_cache_control.rst`,
  `Documentation/block/ublk.rst`, `Documentation/admin-guide/device-mapper/thin-provisioning.rst`
- Papers (recalled): LSVD (Hajkazemi et al., EuroSys 2022); S3-FIFO (Yang et al.,
  SOSP 2023); SIEVE (Zhang et al., NSDI 2024); ARC (Megiddo & Modha, FAST 2003);
  LIRS (Jiang & Zhang, SIGMETRICS 2002); CLOCK-Pro (Jiang et al., USENIX ATC 2005);
  TinyLFU (Einziger et al., 2017); 2Q (Johnson & Shasha, VLDB 1994);
  LFS cleaning (Rosenblum & Ousterhout, 1992); The Tail at Scale (Dean & Barroso, CACM 2013)
