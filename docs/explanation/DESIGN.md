# Lunix Design (draft v0, 2026-11-11)

This is the system spec. Rationale lives in [DECISIONS.md](DECISIONS.md)
(ADR-n) and evidence in [research/](research/). Terms are defined in
[GLOSSARY.md](GLOSSARY.md). All ADRs are *proposed*. Where this spec and an
accepted ADR disagree, the ADR wins and this file is fixed in the same
commit.

## 1. Overview

```
             applications / ext4 / xfs / databases
                          │  block I/O
                   /dev/ublkbN  (kernel ublk_drv)
                          │  io_uring URING_CMD per queue
 ┌────────────────────────┴──────────────────────────────────────────┐
 │ lunixd (one per host)                                             │
 │  queue threads (pinned, libublk) ── journal ── dirty overlay      │
 │        │ reads: overlay → map → local store → backend             │
 │  destage workers: zero │ hash │ dedup │ compress │ encrypt │ pack │
 │  local store: journal │ segments │ cache slab │ map packs │ index │
 │  sync engine (tokio): commits, lease, uploads, mirrors, GC, warm  │
 │  control API (unix socket) ◄── lunix CLI, lunix-csi node plugin   │
 └────────────────────────┬──────────────────────────────────────────┘
                          │ S3 API (authority + mirrors)
        bucket A (authority, CAS)   bucket B (mirror)   bucket C ...
```

A **volume** is a thin block device of a given size. Its data path is:

1. **Admission.** A write is journaled locally and acknowledged.
2. **Destage.** Zero-detection, hashing, compression and encryption run
   later, and the results are packed into **segments** (ADR-4).
3. **Map update.** The LBA → chunk **map** (a Merkle radix tree) is
   updated (ADR-8).
4. **Commit.** A **commit** records a crash-consistent cut (ADR-3, ADR-9).
5. **Replication.** With backends attached, segments, map packs and
   commits are replicated to the authority and then to the mirrors
   (ADR-15). The local store becomes an evicting cache.

## 2. Volume model

- **Volume:** `uuid` and a human name registered on the host. Fixed at
  creation: block size 4096, chunk size (default 64 KiB, ADR-5),
  compression policy, encryption on/off, and family. Size is mutable
  (§14).
- **Family:** the key domain (ADR-13): a volume plus its snapshots and,
  post-v1, its clones. `family_id`, keyring (if encrypted).
  - **Dedup domain:** the volume plus its frozen lineage bases (ADR-25).
- **Lineage:** `volume.lineage` lists the parent volumes and base commits
  a clone reads through. It is always empty in v1; the format is frozen
  for post-v1 thin clones (ADR-25).
- **Backends:** an ordered set of S3 locations `s3://bucket/prefix`. Each
  has a role (`authority` | `mirror`), a state (§12), a cursor, a
  credentials reference and a cost weight. Zero backends means a
  **local-only** volume.
- **Mode:** `local` | `async{T,B,B_max}` | `sync` (ADR-10), changeable
  at runtime.
- **Space policy:** `free_floor` (bytes or % of the host FS), `on_full =
  wait|error`, `full_timeout`, `cache_max` (bytes). The local store never
  exceeds `min(cache_max, fs_free - free_floor)`.
- **Owner:** the host that holds the lease. Only the owner writes,
  destages, commits and garbage-collects.

## 3. The ublk device

- **Device creation:** `ADD_DEV` with the flags of ADR-17, then
  `SET_PARAMS`, then `START_DEV`. `nr_hw_queues` is 4-16 (default:
  `min(8, cpus/4)`) and QD is 128. There is one pinned thread per queue.
  Buffers are preallocated and mlocked. The daemon runs with
  `PR_SET_IO_FLUSHER` and in its own mount namespace (libublk issue #50).
- **Parameters:**
  - `BASIC`:
    - `logical_bs = physical_bs = 4096`, `io_opt` = chunk size,
      `io_min` = 4096;
    - `max_sectors` = 1 MiB;
    - `attrs = VOLATILE_CACHE | FUA` (plus `READ_ONLY` for snapshot
      mounts);
    - `dev_sectors` = size.
  - `DISCARD`: `discard_granularity = 4096`, a large
    `max_discard_sectors`, `max_write_zeroes_sectors`.
- **Device naming:** `/dev/ublkbN` plus the udev symlink
  `/dev/lunix/<name>`. Partition scanning is off (`NO_AUTO_PART_SCAN`).
- **Recovery:** after a crash or an upgrade, the daemon finds devices in
  the QUIESCED or recovering state and runs `START_USER_RECOVERY`, then
  reattaches the queues. `USER_RECOVERY_REISSUE` replays in-flight
  requests. This is safe because writes are idempotent at the block level
  and the journal deduplicates by `seq`. A per-volume `on_hang` watchdog
  can switch a wedged device to fail-I/O.

### 3.1 Operations

| op | handling (queue thread) | completes when |
|---|---|---|
| READ | overlay → map → local store → backend miss (§10) | data copied to the request buffer |
| WRITE | reserve journal space; `seq = next()`; append record (O_DIRECT, io_uring); update overlay | journal write completed (volatile) |
| WRITE+FUA | as WRITE, then join the next flush group | the flush group is durable at the mode's level |
| FLUSH | join the flush group: one `fdatasync` of the journal; sync mode: also a DELTA commit (§8.3) | the group is durable |
| DISCARD / WRITE_ZEROES | journal a hole record; overlay marks a hole | journal write completed |

The queue thread never blocks. Anything that waits (space, S3, a cold
RMW) parks the tag and is resumed by a completion.

## 4. Local store

Directory (any POSIX FS; btrfs CoW and compression discouraged for the
journal):

```
<dir>/<volume-uuid>/
  volume.json        local copy of volume config + backend set + cursors
  LOCK               process lock (stale-lock takeover, Constellation ADR-29 pattern)
  journal/           ring of preallocated, zero-filled 64 MiB files
  segments/          <seg:016x>.seg — own segments (same bytes as on S3)
  cache/             slab file(s) of fetched extents (S3-FIFO)
  map/               map-node packs (own + fetched)
  index/             chunk index LSM runs (chunk_id → segment, offset, clen)
  heat/              access sampling for warm-start manifests
  state/             checkpoint: overlay snapshot, destage cursor, last commit
```

### 4.1 Journal

- The journal is a ring of 64 MiB files opened O_DIRECT. Records are
  4 KiB-aligned: `{magic, seq, epoch, lba, len, kind(write|hole), crc32c}`
  followed by the data.
- Writes are batched per queue into io_uring submissions. FLUSH issues a
  group-committed `fdatasync`, or a linked WRITE→FSYNC batch if a
  benchmark shows that is faster.
- **Recovery:**
  1. Scan from the checkpoint's `journal_tail`.
  2. Accept records with valid CRCs in contiguous `seq` order.
  3. Stop at the first gap or torn record. A torn tail is legal, because
     those writes were never flushed.
- **Reclaim:** journal space is freed once every record in a file has
  been destaged into a segment whose `fdatasync` is done, *and* the
  owning map change is checkpointed.
- **Retention after destage:** in async and sync modes, records also wait
  until the containing cut is committed to the authority. A DELTA commit
  can then be rebuilt from the journal.

### 4.2 Dirty overlay

- An in-memory, LBA-sharded interval map (`lba → journal location`, or
  hole) at 4 KiB granularity, holding not-yet-destaged data.
- Reads consult the overlay first. Destage removes entries once the chunk
  is in the map.
- The overlay is persisted only implicitly, by the journal plus the
  checkpoint.

### 4.3 Space accounting

`used = journal_reserved + segments + cache + map + index`.

- **Admission:** a write reserves its journal bytes. If the reservation
  would cross `min(cache_max, fs_free - free_floor)`, the space ladder
  runs:
  1. Evict clean cache.
  2. Evict cold synced segments.
  3. Compact (local-only).
  4. Throttle destage and upload.
  5. Apply `on_full`: `wait` parks the request, until `full_timeout` if
     one is set, then ENOSPC; `error` returns ENOSPC immediately (ublk
     maps it to `BLK_STS_NOSPC`).
- A reserve of 2 × the segment size, plus the commit and map headroom, is
  never handed to writes.

## 5. Destage pipeline

The pipeline runs on the worker pool. Its input is a batch of overlay
extents grouped by chunk; it prefers chunks whose newest record is older
than the async interval, so overwrites are absorbed.

1. **Assemble the chunk.**
   - If the chunk is fully covered by the overlay, use that data.
   - If it is partially covered, read the base chunk (local store or
     backend) and patch it. A cold RMW is counted in metrics; the base
     fetch is prioritised over readahead.
2. **Zero scan:** OR-accumulate over 64 B lanes. An all-zero chunk
   becomes a hole and the pipeline stops.
3. **Hash:** BLAKE3 (keyed if the family is encrypted) → `chunk_id`.
4. **Dedup:**
   - Look up `chunk_id` in the chunk index (in-memory cuckoo or xxh3
     table, then LSM).
   - On a hit, the map entry points to the existing location and the
     pipeline stops.
   - The index is authoritative only for chunks reachable from retained
     commits. GC removes entries (§11).
5. **Compress:** `auto` = LZ4 probe on 4 KiB; raw if the saving is under
   6%; else zstd-1 with a per-worker context (ADR-12).
6. **Encrypt (optional):** AES-256-GCM. The extent key is derived from
   `chunk_id` plus a random per-extent salt, so each key is used once
   (ADR-11).
7. **Pack:** append the extent to the open segment, in LBA order within
   a batch. Seal the segment at the target size or at a commit cut:
   write the footer, `fdatasync`, rename into `segments/`.
8. **Map update:** set the leaf entry to `{chunk_id, hint}`, mark the
   leaf dirty and insert the chunk into the index.

Throughput budget per core (measured, i9-14900K):

| Stage | Throughput |
|---|---|
| zero scan | ~100 GB/s |
| BLAKE3 at 64 KiB | 5.8 GB/s |
| AES-GCM | 11 GB/s |
| LZ4 probe | ~cheap |
| zstd-1 | 0.4-2 GB/s, data-dependent: the bottleneck |

About 8-16 workers sustain 5 GB/s of ingest, and the pipeline degrades
to LZ4 or raw under backpressure.

## 6. Segments and on-bucket format

Segment (`segments/<seg:016x>`, immutable, target 16 MiB):

```
[extent]* [footer] [trailer]
extent  = CCH1 header {magic, ver, codec, level, ulen} + chunk_id(32) + clen + [salt(24) if encrypted] + payload(+16 B GCM tag)
footer  = sorted [chunk_id, offset, clen, ulen, codec] (zstd-compressed) + seg-level BLAKE3 + seq range
trailer = footer offset + length + magic
```

- Segment numbers are allocated by the owner, are monotonic, and are
  never reused: `seg = (epoch << 40) | counter`, so a zombie can never
  collide.
- Map packs use the same container with node payloads: `map/<pack:016x>`.

### 6.1 Bucket layout

```
<prefix>/lunix/v1/<volume-uuid>/
  volume          create-only: format version, uuid, family, lineage[] (empty in v1),
                  block/chunk size, codec policy, id_len, crypto params,
                  wrapped key slots (MAC'd)
  lease           CAS object: epoch, owner{host,cluster}, deadline, released
  head            advisory: last known commit seq (plain PUT; never trusted)
  commits/<seq:016x>       create-only FULL | DELTA | EPOCH | CONFIG | TRANSFER commits
  sync/<backend-id>/progress   advisory backfill/cursor progress (owner-only plain PUT)
  segments/<seg:016x>      data segments
  map/<pack:016x>          map-node packs
  heat/<seq:016x>          warm-start manifests (ADR-23)
  snapshots/<name>         create-only pins {seq, created, consistency, label}
  readers/<host>-<id>      time-travel mount pins {seq, deadline}, renewed while mounted
  holds/<child-uuid>       clone base pins {base_seq}; none in v1, but GC honours them (ADR-25)
  gc/                      condemned lists, evidence journal
<prefix>/lunix/v1/families/<family-id>/keyring   (shared by clones)
```

## 7. Block map

- **Shape:** a radix tree over chunk index `ci = lba / chunk_size`.
  Fanout is 512. Depth is `ceil(log512(chunks))`. At 64 KiB chunks that is
  3 levels (leaf + 2 interior) up to 8 TiB, and 4 levels up to 4 PiB.
- **Entries:**
  - Leaf entry: `hole` | `{id_prefix, hint{seg, off, clen}}`.
    - `id_prefix` is the first `id_len` bytes of `chunk_id`, where
      `id_len` ∈ {16 (default), 32, 0 = lean}, fixed per volume (ADR-8).
    - Reads verify the prefix against the full `chunk_id` in the
      extent header.
  - Leaf encoding (ADR-8):
    - A segment dictionary. Each entry is `(lineage_idx: u8, seg)`.
      `lineage_idx` is always 0 in v1; post-v1 it lets a clone's hint
      point into a parent's segments (ADR-25).
    - Runs of chunks contiguous in one segment, each stored as `(seg
      index, start offset, count)`.
    - Per-entry `clen` packed into fixed bit width.
    - The id prefix as a fixed-width column.
    - A restart point every 16 entries with the running offset.
    - Holes as runs.
    - No outer compression.
  - Leaf size and lookup cost:
    - A leaf is about 9-11 KiB at 16 B ids: 294-352 MiB per TiB, against
      768 MiB for plain 48 B rows.
    - A point lookup takes 121-181 ns on the encoded leaf.
  - Interior entry: `{child_hash, child_location{pack, off, len}}`.
  - Absent subtree: the empty-node hash.
- **Node identity:** node hash = BLAKE3 of the canonical encoding (keyed
  if encrypted). Leaves are stored in their bit-packed encoding without
  compression. Interior nodes are zstd-compressed. If the family is
  encrypted, both are encrypted with ADR-11's scheme keyed by the node
  hash.
- **Hot path:**
  - Map nodes are paged from `map/`, from the local pack store or from
    the backend.
  - An in-memory node cache is bounded by `map_cache_max`. It holds
    leaves in their encoded form, with a small decoded tier for the
    hottest leaves (2.4 ns lookup).
  - At 16 B ids, 1 GiB of cache covers about 3 TiB of LBA.
  - The interior nodes are about 1.6 MiB per TiB and always stay
    resident.
  - Modified leaves are held in a mutable copy-on-write overlay keyed by
    leaf index.
- **Seal (on a FULL commit):**
  1. Serialise dirty leaves.
  2. Recompute hashes up the paths.
  3. Write the new nodes into one map pack.
  4. The new root goes into the commit.
- **Cost:** `dirty_leaves × ~10 KiB` (encoded) plus a few upper nodes.
  Scattered-write workloads dirty many leaves. That is bounded by the
  async interval, measured in phase 0, and if needed mitigated by
  map-delta records in DELTA commits.
- **Hints:**
  - A hint makes a read 1 GET.
  - When compaction moves an extent, the current map is rewritten lazily:
    the compactor emits `moved{chunk_id → new hint}`, applied to leaves
    reachable from the head on the next seal.
  - A stale hint in a retained snapshot fails verification: either the
    extent header's `chunk_id` does not start with the leaf's
    `id_prefix`, or the segment is gone. The read then falls back to the
    chunk index, which is searched by prefix and confirmed against the
    full id.
  - Lean volumes (`id_len = 0`) would instead need forwarding records
    keyed by `(seg, off)`. That needs its own ADR before it is enabled.
- **Chunk index:**
  - `chunk_id → (seg, off, clen)` is kept as LSM sorted runs locally,
    persisted to the bucket inside FULL commits as index packs.
  - It can always be rebuilt from segment footers, which is `fsck
    --rebuild-index`.

## 8. Commits, cuts and modes

### 8.1 Cuts

- A global atomic `seq` is assigned at write admission, across all queues.
- To cut at `c`:
  1. Pick `c` = the current `seq` value.
  2. Wait until every write with `seq ≤ c` has reached the journal; that
     is only the in-flight ones.
  3. Destage up to `c` into sealed segments and the map, using MVCC on
     the overlay: writes with `seq > c` stay out of this map version.
  4. Seal the map.
- A cut never stalls new writes; it only waits for in-flight ones. Every
  completed FLUSH names a `seq`, so "the state at the last FLUSH before
  time t" is well defined (CDP at commit granularity).

### 8.2 FULL commit

```
commit { kind: FULL, seq, epoch, parent_seq, cut_seq, size, map_root, map_packs[],
         new_segments[], index_packs[], chain_floor, heat?, created_at, mac }
```

Upload order:

1. All new segments and packs are uploaded, using parallel PUTs with
   AIMD concurrency from `upload-concurrency`.
2. Then `commits/<seq>` is written with `If-None-Match: *`.
3. Then, best effort, `head` is updated.

In async mode a FULL commit is cut every `T`/`B`. Unsynced bytes above
`B_max` throttle admission.

### 8.3 DELTA commit (sync mode)

- On FLUSH or FUA, all flush waiters are gathered into one group.
- `cut = max seq` of that group. The journal records `(last_commit_cut,
  cut]` are framed, compressed and encrypted into
  `commits/<seq>` (kind DELTA, `If-None-Match: *`).
- One PUT completes every waiter in the group. While it is in flight the
  next group accumulates, with one PUT outstanding per volume.
- **Size cap:** if the group's data is over the DELTA cap (default
  8 MiB), the bulk goes as a segment first and the DELTA references it.
- **Folding:** FULL commits are cut in the background on the async
  cadence. Mount replays DELTAs on top of the last FULL.

### 8.4 Mount (attach) from a bucket

1. Read `volume` and verify its MAC.
2. Find the tip: start from the `head` hint, LIST `commits/` from it,
   and probe forward until a 404.
3. **Find the authority** (ADR-24):
   - Read the latest CONFIG at or below the tip.
   - Go to the backend it names as the authority. If that backend's tip
     is a TRANSFER, follow `TRANSFER.to`.
   - Repeat until a tip is not a TRANSFER. Continue on that backend.
   - Any InSync backend may serve data reads during and after the
     attach.
4. Load the newest FULL ≤ tip and replay subsequent DELTAs, in order,
   up to the first gap.
5. Acquire the lease: CAS `lease`, `epoch+1`. If the lease is held and
   not released, wait for the deadline (or `--steal`).
6. Write an EPOCH commit at tip + 1. If that write gets 412, another
   writer appended: re-read, replay, retry. If the 412 slot holds a
   TRANSFER, follow it (step 3).
7. Start the device. Map nodes are faulted lazily, and prefetch follows
   the heat map (§10.4).

### 8.5 Lease and fencing

- **Lease:** `{epoch, owner, deadline, released}`, renewed every 30 s via
  `If-Match` (TTL 90 s, configurable).
- **Losing the lease:** if renewal fails for longer than the TTL minus
  skew, the owner **self-fences**. Writes park and the device goes
  read-only after `fence_grace`.
- **Commits as the final fence:** commits are the real fence. A zombie's
  next commit slot is already taken by the EPOCH commit, so its 412 shows
  it a higher epoch and it stops. A zombie's writes after its last commit
  are lost, which is correct: they were never committed.
- **Graceful detach:** final cut, FULL commit, then `lease.released =
  true` (CAS).

### 8.6 Snapshots, restore points, time travel, rollback (v1, ADR-25)

- **Snapshot** (`lunix snapshot NAME SNAP [--freeze MOUNTPOINT]`):
  1. With `--freeze` (or CSI `snapshot.freeze=true`), run `fsfreeze -f`.
  2. Force a cut, then a FULL commit `c`.
  3. Create `snapshots/SNAP` (`If-None-Match`) as `{seq: c, created,
     consistency: crash|fs-frozen, label}`.
  4. Unfreeze.
  - The cost is one cut: no data is copied, and nothing is copied later.
  - Pins are copied to mirrors after their commit, and survive TRANSFER.
- **Restore points:**
  - Every retained commit is a restore point.
  - A DELTA target is materialised by replaying DELTAs on top of the
    preceding FULL, in memory.
  - Granularity:

    | Mode | Restore points |
    |---|---|
    | async | one per commit, every `T`/`B` (idle volumes create none) |
    | sync | one per flush group |
- **Time-travel mount** (`lunix mount-at NAME --snapshot S | --commit
  seq | --time t`):
  1. Resolve the target commit. `--time` means the newest commit with
     `created ≤ t`.
  2. Create `readers/<host>-<id>` `{seq, deadline}`, renewed every
     `TTL/3`.
  3. Open the map root read-only.
  4. Expose `/dev/lunix/NAME@S` with `UBLK_ATTR_READ_ONLY`.
  - No lease is needed, and any number of hosts can mount at once.
  - It shares the node's cache slab, and reads go to the nearest InSync
    backend.
  - Unmount deletes the reader pin. A crashed reader's pin simply
    expires.
- **Rollback** (`lunix rollback NAME --to S|seq`):
  - Requires the owner, with the device detached (or `--offline`) and
    the journal empty.
  - It writes a FULL commit with `map_root` = the target's root, a new
    `seq` and `parent` = the current tip. The rollback is therefore
    itself a restore point and can be undone.
  - The cache needs no invalidation: entries are keyed by content.
- **Diff and export:**
  - `lunix diff NAME A B` walks the two map roots, skipping equal
    subtrees, and prints the changed LBA ranges.
  - `lunix export NAME --snapshot S -o image.raw` streams a raw image
    (holes stay sparse).
  - An incremental export stream format is post-v1.

### 8.7 Retention

- **Policy:** `lunix retain NAME last:10,within:24h,every:1h:48,every:1d:30`.
  - The tiers are clock-free in the sense of Constellation plan 32:
    thinning is decided from commit `created` order and seq, never by
    deleting on a timer that could race a writer.
- **GC roots:**
  - retained commits;
  - snapshot pins, reader pins and holds;
  - the tip's FULL plus its DELTAs.
- **Chain floor:**
  - Each FULL records `chain_floor`, the lowest retained seq.
  - Commit objects below `chain_floor` that are not pinned may be
    deleted. Mount and tip discovery never look below it.
  - EPOCH, CONFIG and TRANSFER commits are kept while a TRANSFER pointer
    chain needs them (ADR-24).
- **CSI** (§17):
  - CreateSnapshot maps to a pin (with freeze if configured);
    DeleteSnapshot deletes the pin.
  - In v1, restoring a snapshot into a new PVC is a full copy into a new,
    independent volume. That is server-side CopyObject on the same
    provider, else a streamed copy.
  - Thin clone and restore are post-v1 (ADR-25).

## 9. Backends and S3 client

- Built on `object_store` with Constellation's CAS classification and
  provider quirks (`cas.rs`, `classify.rs`): 409/412/404/304, a
  retried-PUT-landed check and redacted errors.
- Retries cap at 30 s per call. Transient failures retry forever in sync
  mode (a "hard mount"), unless `flush_timeout` is set; then FLUSH fails
  with EIO.
- **`lunix doctor s3://…`** probes:
  - `If-None-Match`, `If-Match`, ranged GET, LIST-after-PUT and multipart;
  - 8-way concurrent CAS (Constellation `probe.rs`);
  - clock skew;
  - latency and throughput at 4 KiB, 64 KiB and 16 MiB.

  - bucket versioning state.

  The results decide whether the backend may be the authority. A
  versioned bucket qualifies only if the concurrent-CAS probe passes
  (Ceph RGW tracker 80925).
- Provider adapters: GCS uses generation preconditions
  (`x-goog-if-generation-match`). Tigris needs `X-Tigris-Consistent:
  true`. Express One Zone (directory buckets) is a later accelerator.

## 10. Read path and cache

### 10.1 Lookup

1. Overlay (journal data).
2. Map entry → hole (zero-fill) or `chunk_id`.
3. Local location: own segment, or a cache-slab entry keyed by the
   16 B `id_prefix`. The slab entry holds the full `chunk_id`, which is
   checked.
4. Otherwise a backend miss: a ranged GET of the extent via the hint (or
   the index), verify, decompress, insert into the slab, serve.

### 10.2 Miss handling

- Concurrent misses on the same chunk coalesce.
- Adjacent misses within one segment merge into one ranged GET when the
  gap is under 1 MiB. A whole-segment GET happens only when most of it
  is wanted.
- GETs are hedged at the backend's EWMA p95, within a hedge budget of
  5% extra requests.
- With mirrors, the source is chosen by latency and egress weight
  (Constellation `sources.rs`).

### 10.3 Readahead

- A per-volume stream detector (several concurrent streams) uses a
  doubling window from 2 to 32 chunks, issued as coalesced ranged GETs.
- Prefetch-tagged entries are admitted to S3-FIFO's small queue only.

### 10.4 Warm start (ADR-23)

- `heat/` objects list hot chunk ranges and a first-touch order.
- After a new attach, a background prefetcher pulls them from the
  nearest backend, at lower priority than demand misses.
- `lunix warm <vol> --node` pre-fills a target host's cache before a
  planned move. That host reads from the backend, read-only, without
  taking the lease.

### 10.5 Eviction

- **S3-FIFO** over clean cache entries: small FIFO (10%), main FIFO,
  ghost queue.
- Own segments become evictable once committed on the authority and
  cold, as whole files.
- Dirty data, unsynced data, data pinned by an in-flight cut, and the
  journal are never evictable.
- Local-only volumes evict nothing: their segments are the store.

## 11. Garbage collection and compaction

- **Mark:** walk the maps of all GC roots (§8.7: retained commits, the
  tip's FULL plus DELTAs, snapshot, reader and clone-hold pins) and
  accumulate live bytes per segment. Maintain approximate per-segment liveness
  incrementally between full marks.
- **Compact:**
  - Candidates are segments under 50% live, ranked by LFS cost-benefit
    `(1-u)·age/(1+u)`.
  - Live extents are rewritten, grouped by LBA locality, into new
    segments, emitting `moved` records (§7).
  - Compaction runs on the owner under the lease and is rate-limited.
    Its local scratch is bounded.
- **Delete:** condemn, then grace (≥ 2 × lease TTL plus a read grace for
  read-only snapshot mounts and warmers), then re-check, then DELETE.
  Deletes are fenced on the lease epoch, and an evidence journal goes to
  `gc/`. Since the single owner runs both destage and GC, a dedup hit can
  never target a condemned chunk: dedup consults only index entries not
  marked condemned.
- **Space accounting** (ADR-25):
  - For each snapshot and retained commit, Lunix reports:
    - `REFER`: the compressed bytes referenced;
    - `WRITTEN`: bytes changed since the previous retained root, by
      Merkle diff summing leaf `clen`;
    - `USED`: bytes referenced only by this root, which deleting it
      would free.
  - Results are cached in `gc/space/<seq>` and recomputed incrementally
    when retention changes.
  - `lunix gc NAME --dry-run [--retain …]` reports what a policy change
    would reclaim.
- **Ownership:** a volume deletes only segments under its own prefix.
  Data referenced by another volume's clone hold is protected by the
  hold, never by cross-owner refcounts.
- **Mirrors:** GC deletes on mirrors only after the authority has deleted
  and the mirror's cursor is past the commit that dropped the reference.

## 12. Multiple backends

- **Membership lives in the chain (ADR-24).**
  - The backend set, roles, states and the authority are recorded in
    CONFIG commits on the authority's chain: `{config_version,
    authority, backends[{id, url, role, state, since_seq}], policy}`.
  - Every row transition below is one CONFIG commit, written create-only
    on the authority.
  - Mirrors receive the commits as byte-identical copies and need no CAS.
  - Backfill progress and cursors are advisory (`sync/<id>/progress`),
    rebuildable by a LIST/HEAD diff.
  - Credentials are never in S3. Backends are resolved by `id` on each
    host or cluster.

| state | meaning | transitions |
|---|---|---|
| Joining | probe passed; tail replication of new objects starts; backfill copies every object reachable from retained commits, hottest first, restartable from a persisted cursor | → CatchingUp when the backfill is done |
| CatchingUp | only the tail is behind | → InSync when lag ≤ `max_lag` |
| InSync | holds every commit up to tip minus the allowed lag | → Draining |
| Draining | no new writes; waits for the barrier; verifies the survivors | → Removed |
| Removed | config entry kept for audit; data untouched unless `--purge` | — |

- **Invariant:** on each backend, every object a commit names exists
  before that commit object exists there.
- **Copy order:** segments and packs, then commits in `seq` order, then
  `head`.
- **Copy path:** cross-provider copies stream through `lunixd` or a
  `lunix sync` job. Same-provider copies may use server-side CopyObject.
- **Verification:** `lunix verify --backend X` compares commit chains and
  map roots: equal roots mean equal volumes. It then spot-checks a sample
  of segments by hash.
- **Live authority transfer** (`set-authority B`, ADR-24). The pod keeps
  doing I/O throughout, and only commit publication pauses.
  1. Check that B is InSync and authority-capable.
  2. Pause publication. I/O continues into the journal and cache.
  3. Copy A's commits up to tip `t` to B.
  4. Write `TRANSFER{to: B, epoch: e+1}` at `t+1` on A (CAS). This is the
     linearization point.
  5. Copy `t+1` to B, create B's `lease` (epoch `e+1`), then write
     `EPOCH{authority: B}` at `t+2` on B.
  6. Resume publication on B. A becomes a mirror, or is drained through
     CONFIG commits on B.
  - **Cost:** async mode sees nothing; sync mode delays one flush group
    by about 3 PUT round trips.
  - **Crash recovery:** a TRANSFER at A's tip means roll forward; no
    TRANSFER means A is still the authority.
  - **Purge rule:** a former authority is never purged before every
    remaining backend's cursor is past its TRANSFER.
- **Region move with no detach:**
  1. `backend add B` (Joining → … → InSync).
  2. `set-authority B`.
  3. `backend rm A --drain`.
  - The pod sees no detach.
  - Moving the *pod* itself is a separate step (§17); after it, the
    pod's cache warms from B.
- **DR steal on a mirror** (the authority is unreachable): `lunix steal
  --backend B` makes B the authority at its own tip.
  - It needs `--force` and an acknowledged loss window.
  - The steal writes `EPOCH + CONFIG{authority: B}` on B, at B's own
    tip.
  - When the old authority becomes reachable again, a `TRANSFER{to: B,
    reason: steal}` is written at its tip if the slot is free.
    Otherwise its chain beyond the steal point is marked abandoned and
    quarantined for manual export.
  - A partitioned old owner that still reaches the old authority can keep
    committing there. That split is unavoidable without a shared arbiter
    and is documented. Operators fence the old site first, using CSI node
    fencing or a cloud-side kill.
- **Cost guard:** `lunix migrate plan` estimates the bytes, egress cost
  and time before a backend is added.

## 13. Local-only volumes and attaching S3 later

- **Local-only:** a local-only volume is the same store with no backend.
  Commits are written to `state/commits/` locally: the same objects,
  without CAS.
- **Attaching:** `backend add` makes the first backend the authority:
  1. Probe it.
  2. Write `volume`.
  3. Acquire the lease.
  4. Copy segments, packs and commits.
  5. Write the first CONFIG commit, naming this backend as the authority
     (ADR-24).
  6. Switch the mode to async.
  7. Enable eviction once the backend is InSync.

## 14. Resize

- **Grow:** CAS the new size into the next commit, then `UPDATE_SIZE`
  (`set_capacity_and_notify`), then the file system's grow (CSI
  `NodeExpandVolume` or the user).
- **Shrink:** `lunix resize --shrink --discard-tail`. It is refused while
  the tail maps data, unless `--force`. Sequence: commit the new size,
  `UPDATE_SIZE`, then GC drops the tail's chunks.

## 15. Security

- **Keys:**
  - Keys, AEAD and integrity follow ADR-11.
  - Key slots are wrapped in `volume`, and the `volume` object is MAC'd
    to prevent encryption-downgrade attacks. The KMK fingerprint is
    pinned locally on first attach.
  - Keys live in mlocked, zeroized memory, and core dumps are forbidden.
- **Hardening rules carried over from the Constellation audit:**
  - Bounded decode and bounded allocations from decoded counts.
  - Decompression capped by `ulen`.
  - Checked arithmetic on sizes and epochs.
  - Never trust sizes read from the bucket.
  - Redacted S3 errors.
  - Every decoder fuzzed (`fuzz/`, structure-aware).
- **Control socket:** root-only by default, with an optional group. Every
  mutating control call is audit-logged.

## 16. Processes, control API, CLI

- **Processes:**
  - `lunixd` runs as a systemd unit or a privileged DaemonSet.
  - `lunix` is the CLI.
  - `lunix-csi` runs in `--controller` and `--node` modes.
  - `lunix sync` is a job mode for bulk backfill and verify off-node.
- **Control API:** a typed request/response protocol over a Unix socket,
  either reusing Constellation's control framing (decision in phase 0)
  or gRPC.
- **CLI sketch:**

```
lunix volume create NAME --size 200G --dir /data/lunix [--chunk 64K] [--encrypt] [--compress auto]
lunix volume import NAME s3://bucket/prefix/<uuid> --dir /data/lunix
lunix attach NAME            # → /dev/lunix/NAME     lunix detach NAME [--release]
lunix backend add NAME s3://… [--role authority|mirror]   lunix backend rm NAME <id> [--drain]
lunix mode NAME async --interval 30s --max-unsynced 1G | sync | local
lunix space NAME --floor 10% --on-full wait --full-timeout 60s --cache-max 100G
lunix resize NAME 300G       lunix snapshot NAME SNAP [--freeze MNT]   lunix snapshot rm|ls NAME
lunix mount-at NAME --snapshot S|--commit <seq>|--time <t>   (read-only time travel)
lunix rollback NAME --to S|<seq>   lunix retain NAME <policy>   lunix diff NAME A B
lunix export NAME --snapshot S -o image.raw   lunix gc NAME --dry-run
lunix clone SNAP NEW         (post-v1, ADR-25; v1 `restore SNAP NEW` = full copy)
lunix status [NAME]   lunix doctor s3://…   lunix verify NAME   lunix fsck NAME
lunix warm NAME   lunix migrate plan NAME --add s3://… --remove <id>   lunix steal NAME
lunix upgrade                # QUIESCE → exec → recover, no unmount
```

## 17. Kubernetes

ADR-16 and ADR-22 set the shape.

- `lunix-csi` controller:
  - CreateVolume writes `volume` with `If-None-Match`.
  - DeleteVolume moves the volume to trash.
  - CreateSnapshot and DeleteSnapshot go through `lunixd`'s library over
    the backend (pins, §8.6-8.7).
  - Restore-from-snapshot and PVC clone are a full copy in v1; thin
    clones are post-v1 (ADR-25).
  - The controller needs no ublk.
- **NodeStage:** asks the local `lunixd` to attach (lease acquire), waits
  for the device, then formats if the volume is new and mounts for
  Filesystem mode.
- **NodePublish:** bind-mounts the device or the mount.
- **NodeUnstage:** detach with release.
- **NodeExpand:** grows the file system.
- **Topology:** none, apart from an optional primary-backend region
  preference.
- **Configuration:** `LunixBackendSet` lists the buckets and the
  credentials refs. Credential providers are pluggable: static secret,
  IRSA, Pod Identity, GKE and Azure workload identity, IMDS.
- The region-move runbook is in [the CSI note
  §7](research/2026-11-11-csi-multibackend-mobility.md). It becomes a
  how-to guide in phase 7.

## 18. Observability

- **Metrics, per volume:**
  - IOPS and latency histograms per op;
  - journal fill; dirty, unsynced and RPO-age bytes;
  - destage and compression ratio, dedup ratio;
  - cache hit ratio and cold-RMW count;
  - per-backend lag, cursor, EWMA, error classes, request and egress
    counters with a cost estimate;
  - lease epoch and renewal health.
- **Exposure:** metrics go to `/metrics` (Prometheus) on a local port and
  to `lunix status`.
- **Cost advisor:** SHARDS sampling estimates the miss-ratio curve for
  the cache-size advice in a later phase.

## 19. Failure matrix (abridged)

| failure | effect | handling |
|---|---|---|
| `lunixd` crash / kill -9 | I/O stalls | restart, recover the device, replay the journal, reissue requests |
| host power loss | unflushed writes lost (legal) | journal replay to the last valid record |
| local disk lost, async | writes after the last commit lost (≤ T/B) | `lunix volume import` from the bucket on any host |
| local disk lost, sync | nothing acknowledged-by-FLUSH lost | same |
| S3 unreachable, async | commits pause; unsynced grows to `B_max`, then writes throttle | retry; alert on RPO age |
| S3 unreachable, sync | FLUSH waits (hard) or EIO after `flush_timeout` | per-volume policy |
| network partition, owner isolated | lease renewal fails → self-fence → read-only | the new owner steals after TTL; the zombie gets 412 |
| two daemons attach the same volume | the second waits for the lease or fails | the EPOCH commit decides; there is never a double writer |
| local FS full | space ladder → wait or ENOSPC | §4.3 |
| host FS EIO / corruption | volume read-only, alert | re-import from the bucket |
| bucket object corrupt | the fetch fails verification | read from a mirror; scrub repairs |
| backend rollback / substitution | MAC or epoch mismatch detected | refuse to mount; needs an anchor (open question) |
| mirror lags / fails | its state drops to CatchingUp | the authority is unaffected |

## 20. Performance targets (to be validated in phase 0/1)

These are per volume, on one NVMe-backed host with 8 queues and cache
hits. They are targets, not measurements.

| workload | target |
|---|---|
| 4K random read | ≥ 500K IOPS, p99 ≤ 150 µs |
| 4K random write, async/local | ≥ 300K IOPS (bounded by journal NVMe) |
| sequential write ingest | ≥ 3 GB/s with `auto` compression on 8 workers |
| sequential read, hits | ≥ 5 GB/s |
| cold sequential read, same region | ≥ 1 GB/s with readahead and parallel ranged GETs |
| sync-mode fsync latency | ≈ 1 PUT RTT. S3 Standard: one measured run, PUT p50 70 ms / p99 137 ms at 500 KB. Express One Zone: PUT p99 ~7 ms at 4 KB, same AZ ([fact-check §C](research/2026-11-11-fact-check.md)) |

The baseline comes from the phase 0 spikes: libublk null and loop
targets against `null_blk`.

## 21. Testing

- **Unit and property tests:** map, journal and codec, with proptest
  model checks of the overlay and the map against a flat oracle.
- **Model checking:** a Stateright model of the cut, commit and lease
  protocol, including zombies and handoff.
- **Fault-injecting object store:** Constellation's `faulty.rs` and
  `scripted_http.rs` pattern, covering CAS races, torn PUT responses and
  latency.
- **Crash loops:**
  - kill -9 at every journal, seal and commit step;
  - power-cut emulation (dm-flakey or `blkdiscard` of the unflushed
    region);
  - "destroy local, import at commit N, fsck and DB recovery".
- **Workloads:** fio verify, xfstests, and pgbench on ext4 and xfs over
  the device.
- **Two-daemon fencing scenarios** and mirror add/remove under load.
- **Performance gates** in CI on a box runner.

## 22. Open questions

1. **Rollback anchor.** A malicious bucket can serve an old but valid
   chain. Options: a CSI-object or KMS monotonic counter, the maximum
   across mirrors, or a signed epoch in the local state.
2. **Default chunk size.** 64 KiB against 16 KiB; ADR-5 gate.
3. **Map write amplification** under scattered writes. Measure; DELTA-
   style map-delta records are the fallback.
4. **Cache policy** on block traces; ADR-14 gate.
5. **USER_COPY against the copy path, and BATCH_IO gains, on 7.0.**
   Spikes.
6. **`replicix-core` extraction**; ADR-19, the maintainer's decision.
7. **License.** The private repo currently carries MPL-2.0 like
   Constellation. Keep it, or decide before any publication.
8. **Express One Zone as a `sync-fast-wal` tier.** A later ADR.
9. **Cross-volume dedup on the node cache** (golden images) without
   leaking across families.
