# Lunix Design (draft v0, 2026-11-11)

This is the system spec. Rationale lives in [DECISIONS.md](DECISIONS.md)
(ADR-n) and evidence in [research/](research/). Terms are defined in
[GLOSSARY.md](GLOSSARY.md). All ADRs are accepted (2026-11-12); their
follow-ups are listed at the top of DECISIONS.md. Where this spec and an
accepted ADR disagree, the ADR wins and this file is fixed in the same
commit. The phase 1 plan review of 2026-11-12 amended this file and
ADR-4, 6, 7, 8, 9, 11, 14, 16, 19, 24, 26 and 30; the ADRs carry
"Amended 2026-11-12 (phase 1 review)" blocks. The M2 security review and
the maintainer's tamper-resistance decisions of the same day (keyed chunk
ids in `integrity` mode, the local rollback floor, rules R1-R4, OS
randomness) amend ADR-6 and ADR-11 and add ADR-31; they are stated in §5,
§6, §7, §8.4, §10.1 and §15.

Two sequence numbers are used and never mixed. **`write_seq`** numbers
writes (§8.1). **`commit_seq`** numbers commits, by their position in the
chain (§8.2). A bare `seq` in older text means `write_seq` in a write or
cut context and `commit_seq` in a commit context.

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

1. **Admission.** A write is journaled locally (or, with no local tier,
   into a keeper-held RAM buffer; ADR-26) and acknowledged.
2. **Destage.** Zero-detection, hashing, compression and encryption run
   later, and the results are packed into **segments** (ADR-4).
3. **Map update.** The LBA → chunk **map** (a Merkle radix tree) is
   updated (ADR-8).
4. **Commit.** A **commit** records a crash-consistent cut (ADR-3, ADR-9).
5. **Replication.** With backends attached, segments, map packs and
   commits are replicated to the authority and then to the mirrors
   (ADR-15). The local store becomes an evicting cache, a full replica,
   or is dropped entirely (ADR-26, §13).

## 2. Volume model

- **Volume:** `uuid` and a human name registered on the host. Fixed at
  creation: block size 4096, chunk size (default 64 KiB, ADR-5), map
  fanout (`map_fanout`, 256 or 512, default 512; ADR-8), compression policy,
  crypto mode (`encrypted`, `integrity` or `plain`, below) and family. Size
  is mutable (§14).
- **Crypto mode (ADR-11):** fixed at creation, in the descriptor.
  - `encrypted`: AEAD on extents and metadata, keys from a KMK.
  - `integrity` (the default): HMAC on every commit, keyed extent tags, an
    integrity key in the keyring. Contents are not hidden, and nobody who
    lacks the key can write state the volume accepts.
  - `plain`: unkeyed BLAKE3 checksums and tags. It detects corruption only
    and gives no protection against anyone who can write the bucket. It is
    supported everywhere, including volumes with S3 backends, and the CLI
    warns about its limits (§16).
- **Family:** the key domain (ADR-13): a volume plus its snapshots and,
  post-v1, its clones. `family_id`, keyring (key slots for an encrypted
  family, an integrity key for an `integrity` one, nothing for a `plain`
  one; ADR-11).
  - **Dedup domain:** the volume plus its frozen lineage bases (ADR-25).
- **Lineage:** `volume.lineage` lists the parent volumes and base commits
  a clone reads through. It is always empty in v1; the format is frozen
  for post-v1 thin clones (ADR-25).
- **Backends:** an ordered set of S3 locations `s3://bucket/prefix`. Each
  has a role (`authority` | `mirror`), a state (§12), a cursor, a
  credentials reference and a cost weight. Zero backends means a
  **local-only** volume.
- **Local tier** (ADR-26): `store` (a complete local copy), `cache`
  (partial, evictable; needs a backend) or `none` (no local files; needs
  a backend). It is chosen per attach and changeable live (§13).
- **Durability** (ADR-27): `write_ack = local|s3` and `flush = local|s3`,
  changeable at runtime. The presets are `local` (no backend),
  `async{T,B,B_max}` (local/local), `sync` (local/s3) and `write-through`
  (s3/s3). fsync from the file system above arrives as FLUSH/FUA.
- **Space policy:** `free_floor` (bytes or % of the host FS), `on_full =
  wait|error`, `full_timeout`, `cache_max` (bytes). The local store never
  exceeds `min(cache_max, fs_free - free_floor)`.
- **Owner:** the host that holds the lease. Only the owner writes,
  destages, commits and garbage-collects.

## 3. The ublk device

- **Device creation:** `ADD_DEV` with the flags of ADR-17, then
  `SET_PARAMS`, then `START_DEV`. `nr_hw_queues` is 4-16 (default:
  `min(8, cpus/4)`) and QD is 128. There is one pinned thread per queue.
  Buffers are preallocated and mlocked; each is `max_io_buf_bytes` (a
  per-volume attach option, default 1 MiB, minimum 256 KiB), so a volume
  pins `queues × 128 × max_io_buf_bytes` in copy mode. The daemon runs with
  `PR_SET_IO_FLUSHER` and in its own mount namespace (libublk issue #50).
- **Data path mode:** the copy path is the default. `USER_COPY` and
  `BATCH_IO` are opt-in per volume. `USER_COPY` wins at 64 KiB and above
  and loses about 10% at 4 KiB; `BATCH_IO` showed no gain. `BATCH_IO` is
  not used with `QUIESCE` (ADR-17).
- **Control flags:** libublk 0.4.8 cannot set `NO_AUTO_PART_SCAN`,
  `SAFE_STOP_DEV`, `FAIL_IO` or `PER_IO_DAEMON`. The daemon adds the
  device through the control ring itself and libublk attaches
  afterwards (ADR-17).
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
  requests. This is safe because writes are idempotent at the block level:
  a reissued request is re-admitted with a new `write_seq`, and the guest
  cannot have ordered another write after one it has not seen
  acknowledged. The driver retains the parameters, so recovery sends no
  `SET_PARAMS`.
- **Device identity:** ublk has no owner tag on a device, and device ids
  are reused after a reboot. The daemon therefore records the device in
  `state/device` (device id, `boot_id`, queue count and depth, size,
  flags) and recovers a device only if all of these hold:
  1. `GET_DEV_INFO` on the recorded id succeeds and the state is QUIESCED
     or recovering;
  2. the queue count, depth, flags and `dev_sectors` equal the recorded
     values;
  3. `boot_id` equals the current boot.

  A failed check is an error that leaves the device alone. The daemon
  then adds a new device (attach) and never touches the stale one.
- **Fail-I/O:** a per-volume `on_hang` watchdog can switch a wedged
  device to fail-I/O. A device in fail-I/O cannot be recovered; the
  policy is to delete it and remount. `FAIL_IO` and `REISSUE` cannot be
  combined.
- **Planned upgrade:** `QUIESCE_DEV`, then the daemon exits *without*
  `STOP_DEV` (libublk's `run_target()` issues it and deadlocks the
  kernel), then the new binary recovers the device.
- **Detach:** `detach` uses `TRY_STOP_DEV`, which fails with EBUSY while
  the device is mounted. The daemon never stops a device, and never
  exits, while started requests are not being completed. `DEL_DEV` with
  a mounted file system blocks until the file system is unmounted.

### 3.1 Operations

| op | handling (queue thread) | completes when |
|---|---|---|
| READ | overlay → map → local store → backend miss (§10) | data copied to the request buffer |
| WRITE | reserve journal space; `write_seq = next()`; copy the data into the lane's record buffer and append the record (O_DIRECT, io_uring); update overlay | journal write completed (volatile) |
| WRITE+FUA | as WRITE, then join the next flush group | the flush group is durable at the mode's level |
| FLUSH | join the flush group: one `fdatasync` per journal lane with unflushed appends (§4.1); sync mode: also a DELTA commit (§8.3) | the group is durable |
| DISCARD / WRITE_ZEROES | journal a hole record; overlay marks a hole | journal write completed |

The queue thread never blocks. Anything that waits (space, S3, a cold
RMW) parks the tag and is resumed by a completion.

## 4. Local store

The local store is optional: a volume's local tier is `store`, `cache` or
`none` (ADR-26, §13). This section describes the first two.

Directory (any POSIX FS; btrfs CoW and compression discouraged for the
journal):

```
<dir>/<volume-uuid>/
  volume             the exact descriptor bytes (so `backend add` uploads them with the MAC intact)
  volume.json        local-only settings: name, directory, space policy, commit cadence
  LOCK               process lock (stale-lock takeover, Constellation ADR-29 pattern)
  integrity.key      key-file slot credential of an `integrity` volume made without --passphrase (0600; §15)
  journal/           one lane per ublk queue: q<lane>-<ordinal:08x>.j, preallocated zero-filled 64 MiB files
  segments/          <seg:016x>.seg — own segments (same bytes as on S3); <seg:016x>.open while being written
  cache/             slab file(s) of fetched extents (S3-FIFO)
  map/               map-node packs (own + fetched)
  index/             <pack:016x> chunk index LSM runs (chunk_id → segment, offset, clen)
  heat/              access sampling for warm-start manifests
  state/
    commits/         <commit_seq:016x> local-only commits (§13.3)
    checkpoint       a hint only: last commit_seq, per-lane journal tails, id_high (object id high water), counters
    device           ublk device id, boot_id, queue count and depth, size, flags (§3)
    pins             version 2: highest keyring_seq and epoch seen, KMK fingerprint pin, and the rollback
                     floor {max_commit_seq, tip_hash}; raise-only, local, never uploaded (§15, ADR-31)
<dir>/families/<family-id>/keyring     the family keyring (§6.1)
<state_dir>/volumes.json               host registry: name → uuid, directory
```

- The backend set is not in `volume.json`: from the first backend on it
  lives in the commit chain (ADR-24).
- `volumes.json` is replaced atomically and can be rebuilt by scanning
  the volume directories.
- Every local file that must be atomic is written as a temporary file,
  `fdatasync`, rename, then a directory `fsync`.
- The `checkpoint` is a hint with a checksum and a double-slot atomic
  layout. Losing it or finding it stale is harmless (§4.1).

### 4.1 Journal

- **Lanes.** There is one journal lane per ublk queue. A lane owns a ring
  of 64 MiB files (`journal/q<lane>-<ordinal:08x>.j`) opened O_DIRECT, each
  starting with a header `{magic, version, volume uuid, lane id, file
  ordinal, generation}`. `write_seq` is global across lanes, so the lanes
  are merged by `write_seq` at recovery. The ring is bounded by
  `journal_max`, split evenly across lanes; a lane with no free file parks
  its writes until reclaim.
- **Record framing.** A record is `{magic, write_seq, epoch, lba, len,
  kind(write|hole), crc32c}` as an inline 64-byte header, followed by the
  data. The CRC covers header and data. A hole record has `len` and no
  data. The data is copied into the lane's aligned batch buffer (a bounded
  pool; the pool running dry is `Full`, like a lane with no free file), so
  the ublk request buffer is free as soon as the append is queued.
- **Batch packing (decided 2026-11-12).** Records are packed back to back,
  8-byte aligned, inside one lane **batch**; only the batch is padded to a
  4 KiB multiple and written with one O_DIRECT write. Padding is zeros,
  and a zero `magic` means "end of batch: continue at the next 4 KiB
  boundary". Recovery reads records in order, resyncs past a damaged record
  (below), and stops at the first gap in the merged `write_seq` order, so
  a torn batch loses only its unacknowledged tail.
  - Cost: a batch of *n* 4 KiB writes takes `ceil(n · 4160 / 4096)`
    blocks, under 1.6 % plus one block of overhead at n ≥ 32. An isolated
    4 KiB write at queue depth 1 costs two blocks (8 KiB); at that depth
    the flush, not the bytes, sets the latency (P0.5).
  - There is no separate header block.
- **`epoch`** in a record is 0 until a backend exists (local commits
  record `epoch` = 0 as well; §13.3).
- Writes are batched per lane into io_uring submissions. A lane is
  single-threaded and never blocks.
- **Group flush (ADR-4).** A FLUSH is a group across lanes, assembled by a
  flush coordinator that owns its own ring:
  1. It takes `upto` = the highest `write_seq` among the waiting flushes.
  2. It waits until every lane's completed watermark is `≥ upto`.
  3. It issues one `fdatasync` for each lane with unflushed appends, in
     parallel. The group completes when all have.
  - At most one group is in flight; later flushes accumulate into the next
    one. A FLUSH covers the writes completed before its group formed.
  - Linked WRITE→FSYNC chains and `O_DSYNC` per write were measured and
    rejected.
  - The files are preallocated and zero-filled; on btrfs the journal
    directory is `nodatacow`.
- **Recovery:**
  1. Take the `floor`: the `cut_seq` of the newest FULL commit (0 for the
     genesis commit). It is trusted, because the commit is authenticated.
     The checkpoint is not the floor.
  2. Scan every file of every lane. Keep records with a valid file header
     and a valid CRC and `write_seq > floor`. A record from a previous
     pass of the ring is stale and is not kept.
     - **Resync after damage.** On a header or CRC failure at offset `p`
       the reader does not end the lane. It scans forward from `p + 8` in
       8-byte steps to the end of the file for the next header that passes
       magic, `ver`, `gen` (the file's generation), `off` (its own file
       position), the CRC and `write_seq` greater than the last accepted
       one. It accepts that record and continues. The lane ends only when
       no such header exists. Forging an accepted header needs the 64-bit
       `gen`, which no guest write can see; random damage passing `gen`,
       `off` and the CRC is about 2^-160. The cost is reading the rest of
       a 64 MiB file after a hit. `fsck` reports the damaged range.
     - **Stale tail wipe (M3 review, 2026-11-12).** `recover` never writes.
       The first `open_lane` after it (and any `reclaim`) durably zeroes
       every record past the recovery stop point before any append, so a
       second crash cannot bring them back. The wipe is **irreversible**:
       records beyond a gap or a damaged record are gone after it. So the
       daemon logs `ignored_after_gap` and `damaged` (with seq bounds)
       **before** the first `open_lane`, and a mount option chooses
       `on_journal_damage = wipe` (default, availability first) or `refuse`
       (the volume stays offline for `fsck`, which reads but never opens a
       lane). A clean restart writes nothing to the journal.
  3. Merge the lanes by `write_seq` and accept records in contiguous
     order. This global contiguity, not file position, decides what is
     kept, so resync is safe: a damaged record at or below the floor costs
     nothing, and a damaged record above it is a missing `write_seq` and
     stops recovery exactly where step 4 says.
  4. Stop at the first gap or torn record and ignore everything after it.
     A torn tail is legal, because those writes were never flushed. No
     marker of the last flushed `write_seq` exists on disk, and none is
     added; `fsck` reports valid records beyond the first gap as a warning
     with their `write_seq` range.
  - The checkpoint's per-lane tails only say which files may be skipped. A
    lost or stale checkpoint changes the cost of recovery, not its result.
    The tails are used only if the checkpoint's `last_commit_seq` is at
    most the tip's `commit_seq`. A checkpoint from beyond the tip (after
    `fsck --repair --truncate-tip`, §4.4) says that records up to the old
    tip's cut may be skipped, and they must not be.
- **Reclaim:** journal space is freed once every record in a file has
  been destaged into a segment whose `fdatasync` is done, *and* the
  commit that names the owning map change is durable (§8.2).
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
  3. Compact (local-only; a real step in phase 1, §11).
  4. Throttle destage and upload.
  5. Apply `on_full`: `wait` parks the request, until `full_timeout` if
     one is set, then ENOSPC; `error` returns ENOSPC immediately (ublk
     maps it to `BLK_STS_NOSPC`).
- A reserve of 2 × the segment size, plus the commit and map headroom, is
  never handed to writes.

### 4.4 Open and recovery order

`Engine::open` of a local volume runs these steps, then accepts I/O.
Recovery is idempotent: a crash during it is another recovery.

1. Take `LOCK`; read and verify `volume`; unlock the keys; run the attach
   check (ADR-11).
2. Read the tip commit and verify the last chain link (`fsck` verifies the
   chain from the tip down to `chain_floor`). There is no automatic
   fallback: a tip that does not authenticate is a typed mount error, and
   the repair is offline, `lunix fsck --repair --truncate-tip`, which moves
   the damaged object aside, makes the previous commit the tip and zeroes
   both checkpoint slots (the checkpoint describes the old tip).
3. Open the map at the tip's `map_root`; read the checkpoint hint. Use its
   lane tails only if its `last_commit_seq` is at most the tip's
   `commit_seq` (§4.1).
4. Recover the journal with the tip FULL's `cut_seq` as the floor (§4.1);
   rebuild the overlay from the records; set the next `write_seq`.
5. **Delete orphans.** Delete unsealed (`*.open`) and temporary files.
   Delete every sealed segment and map pack whose id is above the highest
   id the chain names, and that no commit names. This is safe, because
   ids are never reused and the journal still holds the data (reclaim
   happens only after the commit is durable). It must run before step 6,
   or a dedup hit could point the map at a segment that no commit lists,
   and a later GC would delete live data. `fsck` reports such files as
   `orphan` only if the volume was closed cleanly.
   - **Object id allocator.** Ids of segments, map packs and index runs
     are never reused (§6). After the sweep the next id is 1 + the maximum
     of the checkpoint's `id_high`, the highest id named by the chain and
     the highest id present on disk **before** the sweep. A truncated tip
     or a sweep that deleted the top ids therefore cannot make the
     allocator hand out an id that a stale index entry still holds.
6. **Catch up the chunk index** (`index.catch_up`) from the footers of the
   segments the chain names that no named index run covers (§8.2).

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
3. **Hash:** BLAKE3 → `chunk_id`, keyed with the family's **addressing
   key** in `encrypted` and `integrity` mode (maintainer, 2026-11-12). The
   key comes from the KMK in `encrypted` mode and from the integrity key in
   `integrity` mode, by its own purpose id (ADR-11). Only `plain` volumes
   use unkeyed BLAKE3. Keying closes the `id_len = 16` collision search in
   `integrity` mode: nobody without the key can compute an id, so a guest
   cannot grind two chunks with equal 16-byte prefixes (ADR-6).
4. **Dedup:**
   - Look up `chunk_id` in the chunk index (in-memory cuckoo or xxh3
     table, then LSM).
   - On a hit, the map entry points to the existing location and the
     pipeline stops. A hit from the in-process table built from verified
     footers is taken as is. A hit taken from a persisted index run is
     first **confirmed**: read the extent header at the hinted location,
     compare all 32 bytes of its `chunk_id` with the new chunk's, and
     check its tag (the AEAD tag or the keyed tag; the unkeyed checksum in
     plain mode). Only then does the map point at it. An unconfirmed hit
     is a miss and the chunk is stored again. Index runs are checksummed,
     not authenticated (§7), so a forged or damaged entry costs
     availability at most, never a lost write.
   - The index is authoritative only for chunks reachable from retained
     commits. GC removes entries (§11).
5. **Compress:** `auto` = LZ4 probe on 4 KiB; raw if the saving is under
   6%; else zstd-1 with a per-worker context (ADR-12).
6. **Encrypt (optional):** AES-256-GCM. The extent key is derived from
   the DEK of the current `dek_gen`, the family, the kind, the encoding
   (codec, level, `clen`), `chunk_id` and a random 192-bit per-extent
   salt, so each key is used once (ADR-11).
   - The salt is drawn inside the seal function on every call. A retry,
     a re-encode (recompress, zstd upgrade, pipeline degrade) and
     compaction all re-seal with a new salt. No code path stores a salt or
     accepts one from the caller.
   - A debug or fuzz build checks every (key, nonce) pair for repeats.
   - On an `integrity` or `plain` volume this step computes the extent's
     BLAKE3-128 tag instead (§6): keyed in `integrity` mode, unkeyed in
     `plain` mode.
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
extent  = CCH1 header {magic, ver, codec, level, ulen} + chunk_id(32) + clen + [dek_gen(4) + salt(24) + nonce(12)] + payload + tag(16)
footer  = sorted [chunk_id, offset, clen, ulen, codec] (zstd-compressed) + seg-level BLAKE3 + write_seq range
trailer = footer offset + length + magic
```

- The bracketed fields exist only when the volume is encrypted. "Encrypted"
  is read from the authenticated `volume` descriptor, never from an
  extent. `dek_gen` is the DEK generation and is bound in the AAD. `nonce`
  is zero in the default profile and holds the module-generated nonce in
  the FIPS profile. The AAD is `ver ‖ codec ‖ level ‖ ulen ‖ kind ‖ dek_gen
  ‖ family_id ‖ id_len ‖ chunk_id`, with no location in it (ADR-11).
- **The tag.** The 16-byte `tag` is present in **every** extent, so there
  is one extent layout for both volume kinds.
  - Encrypted volume: the AEAD tag.
  - Unencrypted volume: BLAKE3-128 (the first 16 bytes of the BLAKE3
    output) over the extent header, `chunk_id`, `clen` and payload, that is,
    every byte of the extent before the tag, taken as one contiguous
    slice. In `integrity` mode it is keyed with a key derived from the
    integrity key in the family keyring. In `plain` mode it is unkeyed: a
    checksum that anyone who can write the bucket can recompute.
  - A read from the local store verifies the tag, the header and the id
    prefix against the map. It does not recompute the chunk id; the
    `verify_reads` option recomputes it. `fsck` verifies every tag. An
    extent fetched from a backend is also verified by recomputing the
    chunk id (ADR-11).
  - **R1, the id expectation (normative).** On an `integrity` or `plain`
    volume the tag covers the extent's own bytes only; it binds neither
    kind nor location, so a tag-valid extent of the family is accepted by
    anything that does not compare the header `chunk_id`. Therefore every
    data read passes the expected id to the extent codec (`OpenExpect`): the
    leaf entry's `id_prefix` (or the full id when it came from the index)
    and the volume's chunk size, and fails on a mismatch. On an encrypted
    volume the comparison happens before the AEAD (the id is in the AAD and
    the key derivation); on an unencrypted one after the tag. A read with
    neither expectation is a **scan** (fsck, rebuild, GC liveness) and must
    set `verify_id`, which recomputes the chunk id from the plaintext. On a
    volume with `id_len = 0` every read is a scan.
- **Field widths.** The stored header keeps `CCH1`'s widths (a 2-byte
  codec and an 8-byte `ulen`). The AAD and the key-derivation `info` are a
  canonical re-encoding that uses the widths of the normative KAT vectors
  (`bench/crypto-kat/raw/kat-vectors.json`): a 1-byte codec, a 1-byte
  level and 4-byte `ulen` and `clen`. They are not the header bytes. This
  is safe, because a chunk is at most 1 MiB and a codec id is at most
  255.
- Segment numbers are allocated by the owner, are monotonic, and are
  never reused: `seg = (epoch << 40) | counter`, so a zombie can never
  collide.
- Map packs use the same container with node payloads: `map/<pack:016x>`.
- **R3, footers and regions (normative).** A footer is used only after
  `MetaCodec::open(Footer, "segments/<seg:016x>", bytes)` (or the `map/`
  name for a pack) succeeds: the AEAD or MAC, or the unkeyed checksum in
  `plain` mode, binds the footer to its container name. Whenever the extent
  region is read whole (`fsck`, compaction, index `catch_up`, a whole-segment
  GET), the segment-level `seg_hash` is recomputed and compared. A
  container whose trailer or footer fails either check is absent, never
  partly used. Location is deliberately unbound in an extent (re-packing,
  mirroring and dedup copy extents verbatim); `seg_hash` and R1 together
  catch a rearranged region and a moved extent.

### 6.1 Bucket layout

```
<prefix>/lunix/v1/<volume-uuid>/
  volume          create-only: format version, uuid, family, lineage[] (empty in v1),
                  block/chunk size, codec policy, id_len, crypto params
                  (mode: encrypted | integrity | plain; profile), KMK fingerprint, descriptor MAC
                  (a keyed MAC; unkeyed BLAKE3 in plain mode).
                  No key slots.
  lease           CAS object, sealed: epoch, owner{host,cluster}, deadline, released,
                  transferred_to? (set best effort after an authority transfer, ADR-24)
  head            advisory: last known commit_seq (plain PUT; never trusted)
  commits/<commit_seq:016x>   create-only FULL | DELTA | EPOCH | CONFIG | TRANSFER commits
  sync/<backend-id>/progress   advisory backfill/cursor progress (owner-only plain PUT)
  segments/<seg:016x>      data segments
  map/<pack:016x>          map-node packs
  index/<pack:016x>        chunk index runs (sorted [chunk_id, seg, off, clen] blocks, a block index and a checksum)
  heat/<commit_seq:016x>   warm-start manifests (ADR-23)
  snapshots/<name>         create-only pins {commit_seq, created, consistency, label}
  readers/<host>-<id>      time-travel mount pins {commit_seq, deadline}, renewed while mounted
  holds/<child-uuid>       clone base pins {base_seq}; none in v1, but GC honours them (ADR-25)
  gc/                      condemned lists, evidence journal; gc/space/<commit_seq:016x> (§11)
  dicts/<id:08x>           create-only trained zstd dictionaries (ADR-30; envelope reserved in phase 1, used from phase 4)
<prefix>/lunix/v1/families/<family-id>/keyring   (shared by clones)
                  mutable, sealed: keyring_seq, key slots, MAC'd body.
                  CAS (If-Match) on the authority only; copied to mirrors
```

- **Sealing (ADR-11):** every object above is sealed except `volume`
  (plaintext fields plus a MAC), `head` and `sync/*/progress` (advisory
  plain PUTs). Commits and DELTA bodies, `lease`, segment footers, `heat/`,
  `gc/`, `readers/`, `holds/` and `snapshots/` bodies use
  XChaCha20-Poly1305 under a per-kind metadata key. Extents and map nodes
  use the extent scheme. Object names stay plaintext.
  - **The AAD object name** is the bucket-relative object path, such as
    `commits/<commit_seq:016x>`, for every kind (the family for the
    keyring). A local-only commit in `state/commits/<commit_seq:016x>` is
    sealed under the name `commits/<commit_seq:016x>`, so its bytes upload
    verbatim when a backend is attached (§13.3).
- **Frozen bodies.**
  - `snapshots/`, `readers/` and `holds/` bodies are exactly the fields
    listed above.
  - `heat/`, `gc/` (including `gc/space/`) and `dicts/` freeze only their
    envelope in phase 1: `{kind id, ver, bytes}`. Their contents are
    versioned by the phase that defines them: `heat/` by phase 4; `gc/`
    by phase 1 (the evidence body, ver 1, is defined by
    `docs/reference/gc.md`: one sealed create-only object per GC pass,
    `gc/<pass:016x>`, body `{ver: 1, bytes}`; the condemned list is local
    state, never uploaded); `gc/space/` by phase 4; `dicts/` holds the
    dictionary blob of `replicix-zdict` (ADR-30).
- **Index runs.** A chunk index run is an immutable file of sorted
  `[chunk_id, seg, off, clen]` blocks with a block index and a checksum. It
  is stored locally in `index/` and, from phase 2, in the bucket under the
  same name.
- **Keyring:** `keyring_seq` rises on every change, and a keyring below
  the highest one seen is rejected. The `keyring` is the one mutable
  object that is not membership state (ADR-24). Mirrors copy it after the
  authority's CAS.

## 7. Block map

- **Shape:** a radix tree over chunk index `ci = lba / chunk_size`.
  The fanout is the volume's `map_fanout`, a descriptor field fixed at
  creation: 256 or 512, default 512. Depth is `ceil(log_fanout(chunks))`.
  At 64 KiB chunks and fanout 512 that is 3 levels (leaf + 2 interior) up
  to 8 TiB, and 4 levels up to 4 PiB. A leaf holds `map_fanout` entries.
  Fanout 256 writes 36-45% fewer bytes per seal at twice the resident
  interior nodes (ADR-8).
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
  - Interior entry: `{child_hash, child_location{pack, off, len,
    lineage_idx}}`. `lineage_idx` (`u8`) is reserved for ADR-25 so that a
    thin clone's interior nodes can point at its parent's map packs and
    share them. It is 0 in v1, and a reader rejects any other value. It
    sits in the unused top byte of `len` (a `len` is at most 65 644, so it
    fits in 24 bits), which adds no bytes to the entry. The node hash
    still covers child hashes only, never locations or `lineage_idx`.
  - Absent subtree: the empty-node hash.
- **Node identity:** node hash = BLAKE3 of the canonical encoding (keyed
  if encrypted). For an interior node the hash covers the child hashes
  only, never the child locations, so identical content gives identical
  roots on any backend. Leaves are stored in their bit-packed encoding.
  Interior nodes are stored raw. Both go through the extent codec with
  codec 0 (raw): there is no zstd on nodes and no codec byte in the node
  header, which carries only `level` and `fanout`. If the family is
  encrypted, both are encrypted with ADR-11's scheme keyed by the node
  hash. Node hashes are unchanged by the keyed-id decision of §5: in
  `integrity` mode they stay unkeyed and are authenticated through the
  HMAC'd commit that names the root.
  - **R2, node verification (normative).** After opening a node extent
    (`kind = node`; `codec` must be 0, any other value is an error), the map
    recomputes the node hash (a leaf: its whole encoding; an interior node:
    header ‖ child hashes) and requires it to equal the parent's
    `child_hash` (the FULL commit's `map_root.hash` for the root) **and** the
    extent header `chunk_id`. This holds for nodes read from the local pack
    store as much as for fetched ones. In `integrity` mode this is what
    binds a node to its place: the extent tag alone does not.
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
    leaf index. A decoded dirty leaf costs 24 KiB, twice its sealed size,
    so the overlay has a cap, counted in dirty leaves, that forces a
    seal (ADR-8).
- **Seal (on a FULL commit):**
  1. Serialise dirty leaves.
  2. Recompute hashes up the paths.
  3. Write the new nodes into one map pack.
  4. The new root goes into the commit.
- **Cost:** `dirty_leaves × ~11.4 KiB` (encoded, worst case of one dirty
  chunk per leaf) plus the upper nodes, which were 45% of the bytes at
  1 000 dirty leaves. Measured at 10 TiB on one core: 1 000 scattered
  dirty leaves seal in 60 ms and 10 000 in 275 ms. Scattered-write
  workloads dirty many leaves. That is bounded by the async interval and
  by the overlay cap, and if needed mitigated by map-delta records in
  DELTA commits.
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
    named by FULL commits as index packs (`index/<pack:016x>`, §6.1) and,
    from phase 2, uploaded with them.
  - It can always be rebuilt from segment footers, which is `fsck
    --rebuild-index`.
  - **Runs are checksummed, not authenticated.** A hit from a run is a
    hint. A read verifies the extent it finds (§6). A dedup hit is
    confirmed against the extent header's full `chunk_id` and its tag
    before the map points at it (§5, step 4), so a bad run can cost a
    miss, never a write.
  - **Index runs may lag one commit.** A run is persisted after the FULL
    commit that covers its segments (§8.2, step C8), so a FULL cannot
    name the run for its own segments. `index_packs[]` of FULL *n* lists
    the runs that were durable before FULL *n* was created; they cover the
    segments named by commits up to *n*−1. The run for *n*'s segments is
    named by the next FULL. Open catches up the rest from segment footers
    (§4.4). A commit never names a run that is not yet durable.

## 8. Commits, cuts and modes

### 8.1 Cuts

- A global atomic `write_seq` is assigned at write admission, across all
  queues and journal lanes. It is unrelated to the `commit_seq` of §8.2.
- To cut at `c` (a `write_seq` value):
  1. Pick `c` = the current `write_seq` value.
  2. Wait until every write with `write_seq ≤ c` has reached the journal;
     that is only the in-flight ones.
  3. Destage up to `c` into sealed segments and the map, using MVCC on
     the overlay: writes with `write_seq > c` stay out of this map version.
  4. Seal the map.
- A cut never stalls new writes; it only waits for in-flight ones. Every
  completed FLUSH names a `write_seq`, so "the state at the last FLUSH before
  time t" is well defined (CDP at commit granularity).

### 8.2 FULL commit

```
commit { kind: FULL, commit_seq, epoch, parent_seq, parent_hash, cut_seq, size, map_root,
         map_packs[], new_segments[], index_packs[], chain_floor, heat?,
         created_at, mac? }
EPOCH  { kind: EPOCH, commit_seq, epoch, parent_seq, parent_hash, authority?,
         created_at, mac? }
```

- **Numbering.** `commit_seq` is the commit's position in the chain:
  `parent_seq + 1`. The genesis commit, a FULL with an empty map that
  volume creation writes, is `commit_seq` 0 with no `parent_seq`, and its
  `parent_hash` is the BLAKE3 of the `volume` object's bytes as stored.
  `cut_seq` is a `write_seq` (§8.1); a FULL carries both numbers and they
  are independent. `parent_seq` and `since_seq` are commit numbers.
- **`chain_floor`** is a `commit_seq`: the lowest retained commit. It
  starts at genesis and moves up as retention releases commits; it is
  not genesis once `N` FULLs exist. In
  phase 1 the retention policy is `last:N` FULL commits (default 2, the
  tip and its predecessor), so `chain_floor` is the older of the two
  (§8.7, §11).
- **`authority`** on an EPOCH commit is optional. It names the backend that
  becomes the authority and is set by an authority transfer (§12); a plain
  ownership change (§8.4) leaves it out. In a local-only volume `epoch` is
  0 in every commit until a backend exists (§13.3).

- **`created_at`** is informational wall-clock time. It is a **writer
  clamp**, `created_at = max(now, parent.created_at)`, so it never goes
  backwards along a chain written by well-behaved hosts. It is not a chain
  validity rule: a reader cannot check it without the parent, and a host
  with a slow clock may legitimately take over a chain. Mount and `fsck`
  never reject on it; `fsck` warns when it decreases. `epoch`,
  `commit_seq` and `chain_floor` stay reader rules, because they are
  authenticated monotone state, not clocks.
- **Chain fields (every kind, ADR-11):** all five commit kinds carry
  `epoch`, `parent_seq` and `parent_hash`, the BLAKE3 of the parent
  object's bytes as stored.
- **Authentication:** on an encrypted volume the commit is sealed, and the
  AEAD authenticates the whole body. On an `integrity` volume every
  commit kind, and the lease, carries `mac`, an HMAC over the body under
  the integrity key from the keyring. On a `plain` volume `mac` is an
  unkeyed BLAKE3 checksum of the body: it detects corruption and nothing
  else, and the hash chain does not stop a bucket writer who recomputes
  it.

Upload order:

1. All new segments and packs are uploaded, using parallel PUTs with
   AIMD concurrency from `upload-concurrency`.
2. Then `commits/<commit_seq>` is written with `If-None-Match: *`.
3. Then, best effort, `head` is updated.

In async mode a FULL commit is cut every `T`/`B`. Unsynced bytes above
`B_max` throttle admission.

**Index runs a FULL names.** A FULL names only index runs that were
durable before the commit object (`index_packs[]`, §7). The run covering
a FULL's own segments is persisted afterwards and named by the next FULL.

**Local commit order.** A local FULL commit is made in this durable order;
each step has a crash point:
1. Pick `c`, wait for in-flight writes, cut the overlay (§8.1).
2. Destage the cut. Extents go to the open segment, the map is updated in
   memory, and index inserts are held back.
3. Seal the open segments: footer, trailer, `fdatasync`, rename, directory
   `fsync`.
4. Seal the map into a pack: `fdatasync`, rename, directory `fsync`.
5. Build and seal the FULL commit and write it: temporary file,
   `fdatasync`, rename, directory `fsync`. **This is the durability point
   of the cut.**
6. Write the checkpoint hint.
7. Persist the index run for the sealed segments.
8. Reclaim the journal up to the cut, release the overlay and update the
   space ledger.

A crash before step 5 completes leaves the previous commit as the tip; the
new segments and pack are orphans, deleted at open (§4.4). The journal is
never reclaimed before step 5 is durable.

### 8.3 DELTA commit (sync mode)

- On FLUSH or FUA, all flush waiters are gathered into one group.
- `cut = max write_seq` of that group. The journal records
  `(last_commit_cut, cut]` are framed, compressed and encrypted into
  `commits/<commit_seq>` (kind DELTA, `If-None-Match: *`). Like every commit it
  carries `epoch`, `parent_seq` and `parent_hash`.
- One PUT completes every waiter in the group. While it is in flight the
  next group accumulates, with one PUT outstanding per volume.
- **Size cap:** if the group's data is over the DELTA cap (default
  8 MiB), the bulk goes as a segment first and the DELTA references it.
- **Folding:** FULL commits are cut in the background on the async
  cadence. Mount replays DELTAs on top of the last FULL.
- **Write-through** (`write_ack = s3`, ADR-27): plain writes join the
  same flush groups, and each completes when its group's DELTA is on the
  authority.
- **FUA** is a cut at its own `write_seq` (ADR-28). An ext4/xfs fsync arrives
  as an empty FLUSH and then a FUA write of 4 KiB or less, serialised by
  the block layer, so it costs two flush groups.
- **Flush lane** (ADR-28): commit objects and DELTA bodies up to the
  DELTA cap travel on a dedicated connection pool per backend (default
  3), with the hedged-retry budget of reads. Everything else is on the
  bulk lane (§9).
- **Unflushed-byte cap:** with `flush = s3`, acknowledged bytes not yet
  in a PUT and not covered by a commit are capped at `sync_dirty_max`
  (default 64 MiB; at most `mem_buffer / 4` with local tier `none`).
  Above it, admission throttles. The fsync bound is
  `sync_dirty_max / goodput + 2 PUT RTT`.
- **Over-cap groups** are split into segments of at most `flush_segment`
  (default 4 MiB), uploaded in parallel on the bulk lane with AIMD. The
  DELTA commit naming them follows on the flush lane.
- **References:** a DELTA's `refs: [{kind: segment | journal, id, hash}]`.
  `journal` (a sealed raw journal batch staged ahead of a FLUSH) is
  reserved and unused in v1.
- **Telemetry:** `flush_group_bytes`, `flush_latency` and
  `flush_lane_queue`. The CLI warns when a `flush = s3` volume's p50
  flush group exceeds 1 MiB.

### 8.4 Mount (attach) from a bucket

1. Read `volume`.
   - **Attach policy (ADR-11):** if the caller supplied key material, the
     volume must be encrypted, else refuse as a downgrade. An integrity
     credential declares an `integrity` volume. A `plain` volume needs no
     credential, and a credential supplied for one is refused as a
     downgrade (the caller expects protection the volume lacks).
   - Read the family `keyring`, unwrap the KMK (or the integrity key) from
     a slot, and verify the keyring MAC. Reject a `keyring_seq` below the
     highest seen. A supplied KMK fingerprint must match.
   - Verify the descriptor MAC of `volume`. A failure refuses the mount.
2. Find the tip: start from the `head` hint, LIST `commits/` from it,
   and probe forward until a 404. Commit numbers are contiguous
   (§8.2), so the next slot is always `tip + 1`.
3. **Find the authority** (ADR-24):
   - Read the latest CONFIG at or below the tip.
   - Go to the backend it names as the authority. If that backend's tip
     is a TRANSFER, follow `TRANSFER.to`.
   - Repeat until a tip is not a TRANSFER. Continue on that backend.
   - Any InSync backend may serve data reads during and after the
     attach.
4. Load the newest FULL ≤ tip and replay subsequent DELTAs, in order,
   up to the first gap.
   - Verify the chain from that FULL to the tip: each commit's
     `parent_hash` must equal the BLAKE3 of the parent object's bytes, and
     `epoch` must never decrease. A broken link or a decreasing epoch
     refuses the mount. (Verification stops at `chain_floor`, whose parent
     may be absent; the genesis commit's `parent_hash` is the BLAKE3 of
     `volume`.)
   - On an `integrity` volume, verify the HMAC of every commit replayed.
     On a `plain` volume verify its checksum.
   - **R4, the rollback floor (normative).** After the chain verifies,
     compare the tip with the local floor in `state/pins` (§15, ADR-31): a
     tip with `commit_seq` below `max_commit_seq` is refused as `Rollback`,
     and a tip at that height whose hash differs from `tip_hash` as `Fork`.
     The checkpoint's `last_commit_seq` is a hint (discarded when above the
     tip) and is never an anchor. A host with no floor (first attach,
     ephemeral client) cannot detect rollback.
5. Acquire the lease: CAS `lease`, `epoch+1`. If the lease is held and
   not released, wait for the deadline (or `--steal`). **R4, the lease is
   opened only through `open_lease_fresh(bytes, max(chain epoch,
   pins.max_epoch))`**, so an old sealed version of the object fails; a
   lease whose `epoch` is below that floor is rejected.
6. Write an EPOCH commit at `tip + 1`. If that write gets 412, another
   writer appended: re-read, replay, retry. If the 412 slot holds a
   TRANSFER, follow it (step 3).
7. Raise the local floor to the verified tip (the EPOCH commit written in
   step 6 included), then start the device. Map nodes are faulted lazily,
   and prefetch follows the heat map (§10.4).

### 8.5 Lease and fencing

- **Lease:** `{epoch, owner, deadline, released, transferred_to?}`,
  renewed every 30 s via `If-Match` (TTL 90 s, configurable).
  `transferred_to` is optional. A completed authority transfer sets it on
  the old authority's lease with a best-effort CAS (§12); the chain, not
  the lease, is authoritative. The object is sealed, and `epoch`
  is in the plaintext of its body. A reader rejects a lease below the
  newest epoch of the verified chain, because an old version of the same
  object would pass the AEAD.
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
  3. Create `snapshots/SNAP` (`If-None-Match`) as `{commit_seq: c, created,
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
  commit_seq | --time t`):
  1. Resolve the target commit. `--time` means the newest commit with
     `created ≤ t`.
  2. Create `readers/<host>-<id>` `{commit_seq, deadline}`, renewed every
     `TTL/3`.
  3. Open the map root read-only.
  4. Expose `/dev/lunix/NAME@S` with `UBLK_ATTR_READ_ONLY`.
  - No lease is needed, and any number of hosts can mount at once.
  - It shares the node's cache slab, and reads go to the nearest InSync
    backend.
  - Unmount deletes the reader pin. A crashed reader's pin simply
    expires.
- **Rollback** (`lunix rollback NAME --to S|commit_seq`):
  - Requires the owner, with the device detached (or `--offline`) and
    the journal empty.
  - It writes a FULL commit with `map_root` = the target's root, a new
    `commit_seq` and `parent` = the current tip. The rollback is therefore
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
    thinning is decided from commit `created` order and `commit_seq`, never by
    deleting on a timer that could race a writer.
- **GC roots:**
  - retained commits;
  - snapshot pins, reader pins and holds;
  - the tip's FULL plus its DELTAs.
- **Chain floor:**
  - Each FULL records `chain_floor`, the lowest retained `commit_seq`
    (in phase 1, the older of the last `N` FULLs; default `N` = 2).
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
- **Two lanes per backend (ADR-28):** the flush lane (commits and small
  DELTA bodies only) and the bulk lane, which has an in-flight byte cap
  `upload_inflight_max` (default 32 MiB) and a token bucket at
  `upload_share` (default 0.8) of the measured goodput.
- Retries cap at 30 s per call. Transient failures retry forever in sync
  mode (a "hard mount"), unless `flush_timeout` is set; then FLUSH fails
  with EIO.
- **`lunix doctor s3://…`** probes:
  - `If-None-Match`, `If-Match`, ranged GET, LIST-after-PUT and multipart;
  - 8-way concurrent CAS (Constellation `probe.rs`);
  - clock skew;
  - latency and throughput at 4 KiB, 64 KiB, 1 MiB, 4 MiB and 16 MiB.

  - bucket versioning state.

  The results decide whether the backend may be the authority. A
  versioned bucket qualifies only if the concurrent-CAS probe passes
  (Ceph RGW tracker 80925).
- **Measured provider behaviour** (phase 0):
  - AWS S3 Standard and OVH answer concurrent `If-Match` swaps with 409,
    so the client retries the same attempt on 409. Creates answer 412.
  - Both accept `If-None-Match: *` on `CompleteMultipartUpload`.
  - OVH returns a version id on PUT while `GetBucketVersioning` shows no
    status, so `doctor` decides from the probe and not from the status.
  - A segment of up to 16 MiB goes as one PUT. Multipart pays from about
    25-32 MiB and costs a Create and a Complete (about 115 ms on AWS).
- Provider adapters: GCS uses generation preconditions
  (`x-goog-if-generation-match`). Tigris needs `X-Tigris-Consistent:
  true`. Express One Zone (directory buckets) is a later accelerator.

## 10. Read path and cache

### 10.1 Lookup

1. Overlay (journal data).
2. Map entry → hole (zero-fill) or `chunk_id`.
3. Local location: own segment, or a cache-slab entry keyed by the
   16 B `id_prefix`. The slab entry holds the full `chunk_id`, which is
   checked. A local read verifies the extent's tag (§6) and does not
   recompute the chunk id unless `verify_reads` is set.
   - **R1:** the read passes the leaf's `id_prefix` and the volume's chunk
     size as the expectation, and the extent's header `chunk_id` must match
     (§6). A read with no expectation is a scan and sets `verify_id`. The
     same holds for every read in the destage path (partial-chunk base
     read, dedup confirmation).
4. Otherwise a backend miss: a ranged GET of the extent via the hint (or
   the index), verify (R1, and the recomputed id), decompress, insert into
   the slab, serve.

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

GC and compaction ship in phase 1 for local-only volumes, with the space
ladder's compact step (§4.3) real. Phase 2 adds the backend side:
deleting remote segments, fenced on the lease epoch. Mirror GC comes with
mirrors (phase 5).

- **Phase 1 roots.** A local-only volume has no snapshots, readers or
  holds yet. Its roots are the last `N` FULL commits (retention `last:N`,
  default 2: the tip and its predecessor, so an offline repair can
  truncate a damaged tip; mount never falls back by itself, §4.4). GC drops segments, map packs and index runs no root reaches,
  and deletes commit objects below `chain_floor` after the grace. Chain
  verification (mount, `fsck`) walks from the tip down to `chain_floor`;
  the floor commit's parent may be absent. Phase 4 adds tiered retention
  and the other roots.
- **Local grace.** A local-only volume has no lease and no reader pins, so
  its grace covers only in-flight cuts, destage and reads. It is never
  shorter than that. The same condemn, grace, recheck, delete order
  applies, and `epoch` is 0.
- **Mark:** walk the maps of all GC roots (§8.7: retained commits, the
  tip's FULL plus DELTAs, snapshot, reader and clone-hold pins) and
  mark the trained dictionaries (ADR-30) that live extents reference, and
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
  - Delete-time index invariant: a segment that stale hints still point
    into may be deleted only if the index durably names the current copy of
    every live id those hints resolve to, or that copy's segment is named
    by the chain, so `catch_up` restores the entry after a crash.
  - GC never condemns a segment or pack whose id is above the tip's
    high-water mark (the highest id any retained commit names): such files
    are the open segment or uncommitted output, never garbage.
- **Space accounting** (ADR-25):
  - For each snapshot and retained commit, Lunix reports:
    - `REFER`: the compressed bytes referenced;
    - `WRITTEN`: bytes changed since the previous retained root, by
      Merkle diff summing leaf `clen`;
    - `USED`: bytes referenced only by this root, which deleting it
      would free.
  - Results are cached in `gc/space/<commit_seq:016x>` and recomputed incrementally
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
    The family `keyring` is copied the same way, after the authority's
    CAS (ADR-11).
  - Every CONFIG and TRANSFER commit carries `epoch` and `parent_hash`
    like any commit (ADR-11), so a steal's abandoned branch cannot be
    spliced back into a chain.
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
- **Copy order:** segments and packs, then commits in `commit_seq` order, then
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
     `EPOCH{authority: B}` at `t+2` on B (the optional `authority` field,
     §8.2).
  6. Resume publication on B. A becomes a mirror, or is drained through
     CONFIG commits on B. A's `lease` gets `transferred_to: B` (CAS, best
     effort; the chain is authoritative).
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

## 13. Storage topologies and local tier roles (ADR-26)

| topology | local tier | backends | FLUSH is durable as |
|---|---|---|---|
| local only | `store` | 0 | journal `fdatasync` |
| local + S3 | `cache` (or `store` for a full local replica) | ≥ 1 | `fdatasync` (async) or a DELTA commit (sync) |
| S3 only | `none` | ≥ 1 | a DELTA commit (sync); `--unsafe-flush` allows async |

**Copy invariant:**
- Every FLUSH-acknowledged write is on a durable sink: the journal or
  the authority's chain.
- At least one complete copy exists: a `store` tier or the authority.
- Transitions that break either rule are refused.

### 13.1 S3 only (`local none`)

- **Write sink:**
  - A memfd ring of journal records (§4.1 format), `mem_buffer`
    bytes (default 256 MiB). It also holds the open segment being
    packed for upload.
  - When the buffer fills, admission backs off: cut and commit sooner,
    destage, throttle. The first rung is `sync_dirty_max`, at most
    `mem_buffer / 4` here (ADR-28). There is no ENOSPC unless `bucket_quota` is set.
- **Keeper:**
  - The memfd is held by the systemd FD store (`FDSTORE=1`) or by
    `lunix-keeper`, a tiny process in the DaemonSet pod, via
    `SCM_RIGHTS`, so a `lunixd` crash or upgrade keeps acknowledged
    writes.
  - On recovery, the buffer header (`volume uuid, device id,
    generation`) must match. Otherwise the device is deleted (EIO), not
    recovered. A mounted file system then goes read-only, and the
    deletion completes only when it is unmounted.
  - The memfd holds plaintext journal records, like the local journal
    (ADR-11, §15).
  - The phase 0 spike used a standalone keeper process. The systemd
    FD-store variant is untested.
  - The RAM sink's drain must not block the queue thread. The phase 0
    spike drains inline when the ring fills.
- **Reads:**
  - Overlay, then map (interior nodes resident), then `mem_cache`
    (default 256 MiB, S3-FIFO, leaves and extents), then a ranged GET
    (§10.2-10.3).
- **Dedup:**
  - A RAM filter over the bucket's `index_packs`. A hit costs a cached
    ranged GET of one index block, and only on the destage path.
- **Partial-chunk writes:** need the base chunk. That is a cold RMW via
  GET, counted in metrics.
- **GC and compaction:** live extents are read by ranged GET. The work is
  budgeted as on any backend.

### 13.2 Live transitions

- **Sink switch at a `write_seq` barrier `b`:**
  - Overlay entries carry their sink. Writes with `write_seq > b` go to the
    new sink.
  - A FLUSH spanning `b` waits for both sinks.
  - The old sink is retired after a commit covering `b` is on the
    authority.
- **`none → cache`:** create the directory, open the journal, switch
  sink. The RAM buffer drains through normal commits. Warm on demand or
  from `heat/` (§10.4).
- **`cache → none`:**
  - Requires sync mode (or `--unsafe-flush`).
  - Switch sink to RAM, then commit a cut covering `b`: every journal
    record and local segment is now on the authority.
  - Drop the cache, then delete the directory.
- **`cache → store`:** hydrate every segment reachable from retained
  commits, with the backfill machinery of §12 (hot first, restartable).
  Eviction stops at completion.
- **`store → cache`:** needs an InSync backend. Eviction starts.
- **Remove the last backend:**
  - Needs `store`.
  - Write `CONFIG{backends: [], detached: local}` on the authority,
    then release the lease. Commits continue in `state/commits/`.
  - `volume import` of that bucket refuses without `--force`.

### 13.3 Local-only volumes and attaching S3 later

- **Local-only:** a local-only volume is the same store with no backend.
  Commits are written to `state/commits/<commit_seq:016x>` locally: the
  same objects, without CAS. They record `epoch` = 0 until a backend
  exists, and they are sealed under their bucket-relative names (§6.1), so
  they upload verbatim.
- **Attaching:** `backend add` makes the first backend the authority:
  1. Probe it. On a `plain` volume, warn first (and ask for confirmation
     on a terminal): once the bucket holds the volume, anyone who can write
     it can rewrite history and data undetected (§15). The mode is fixed in
     the descriptor, so it cannot be upgraded afterwards.
  2. Write `volume`: the exact bytes of the local `volume` file.
  3. Acquire the lease.
  4. Copy segments, packs and commits.
  5. Write the first CONFIG commit, naming this backend as the authority
     (ADR-24).
  6. Switch the mode to async.
  7. Enable eviction once the backend is InSync.

## 14. Resize

- **Validation:** the driver accepts any size, including one that is not
  a multiple of the 4096-byte block and a shrink under a mounted file
  system. The daemon validates every size before `UPDATE_SIZE`.
- **Grow:** CAS the new size into the next commit, then `UPDATE_SIZE`
  (`set_capacity_and_notify`), then the file system's grow (CSI
  `NodeExpandVolume` or the user). Measured online on ext4, xfs and
  btrfs under load.
- **Shrink:** `lunix resize --shrink --discard-tail`. It is refused while
  the tail maps data, unless `--force`. Sequence: commit the new size,
  `UPDATE_SIZE`, then GC drops the tail's chunks.

## 15. Security

- **Keys:**
  - Keys, AEAD and integrity follow ADR-11.
  - Key slots live in the family `keyring`, a single mutable object
    written by CAS on the authority, with `keyring_seq` in a MAC'd body.
    Slot wraps are bound to family, slot id, kind and KDF parameters.
    Argon2id parameters are bounded when read and floored when written.
  - **Downgrade defence:** the `volume` descriptor is MAC'd under a
    KMK-derived key. The attach request, not the bucket, decides whether
    the volume must be encrypted: supplying key material means it must
    be, and the descriptor MAC must verify. The orchestrator supplies the
    KMK fingerprint. The local trust-on-first-use pin stays for the CLI
    only, because an ephemeral client has no pin.
  - **History:** every commit is hash-chained and carries its epoch.
    Mount rejects broken links, decreasing epochs and a stale lease.
  - **Integrity volumes (the default for unencrypted volumes):** every
    commit and the lease carry an HMAC under an integrity key from the
    keyring, so the bucket alone cannot forge state. Extent contents are
    covered through the chunk ids in the authenticated map, and each extent
    carries its own BLAKE3-128 tag (keyed with a key derived from the
    integrity key), so a flipped byte is caught on a local read too (§6).
    `lunix volume create` makes this mode unless told otherwise. For a
    local volume it also creates a key-file slot (`integrity.key`, 0600,
    next to the volume), so attach needs no extra step; `--passphrase`
    adds a passphrase slot for a volume that will move hosts.
  - **Plain volumes (explicit opt-out, `--plain`):** commits, the lease,
    footers and tags carry unkeyed BLAKE3 checksums. They detect
    corruption (a flipped byte, a torn write) and nothing else. Anyone who
    can write the bucket can rewrite commits, footers and data and
    recompute every checksum; a local read does not recompute the chunk id,
    so a swapped extent with the right header id returns wrong data. The
    mode is fixed in the descriptor, so a plain volume can never be made
    safe later. It is supported everywhere, including with S3 backends,
    and attach needs no credential. The CLI and docs warn when plain is
    chosen and when a backend is added to a plain volume (§16).
  - **Keyed chunk ids (integrity mode, maintainer 2026-11-12).** In
    `encrypted` and `integrity` mode `chunk_id = blake3::keyed_hash(
    addressing_key, plaintext)`; the addressing key comes from the KMK or
    from the integrity key by purpose (ADR-11). Only `plain` volumes use
    unkeyed ids. Without the key nobody can compute an id, so the
    `id_len = 16` residual of the M2 review (a 2^64 BLAKE3 birthday search
    on guest data, then a bucket writer swapping the two chunks on a local
    read) does not exist in `integrity` mode. `plain` volumes keep it, and
    they promise nothing against a bucket writer anyway.
  - **Reading from a hostile bucket (rules R1-R4, normative).** A bucket
    writer lacks the KMK (`encrypted`) or the integrity key (`integrity`).
    The unkeyed parts are index runs, container trailers, `head`, object
    names and sizes. The design needs no per-kind tag, because these four
    rules hold in the engine, the map and the store:
    - **R1** every data read passes `OpenExpect` with the leaf `id_prefix`
      (or the full id) and the chunk size, and fails on a mismatch; a read
      without that is a scan and sets `verify_id` (§6, §10.1).
    - **R2** the map recomputes each node's hash against the parent's
      `child_hash` (the commit's `map_root.hash` for the root) and the
      header `chunk_id`, and requires codec 0 (§7).
    - **R3** a footer is used only after `MetaCodec::open` succeeds on it,
      and `seg_hash` is checked whenever a region is read whole (§6).
    - **R4** attach refuses a tip below the local floor and opens the lease
      through `open_lease_fresh` (§8.4).

    With them, swapping, relocating, replaying an older extent, presenting
    a node as data or the reverse, substituting a footer, and truncating or
    deleting objects are detected in both `encrypted` and `integrity` mode;
    splicing or replaying commits is detected by `parent_hash`, `commit_seq`
    and `epoch` (§8.4). Binding `kind` into the integrity tag (a per-kind
    tag key) is not done; see ADR-11. `plain` volumes get none of this.
  - **Rollback anchor (ADR-31).** An older but fully valid chain cannot be
    told from the current one by anything in the bucket, so v1 keeps a
    **local floor** `{max_commit_seq, tip_hash}` in `state/pins` version 2.
    It is raise-only and is written after every verified attach (the EPOCH
    commit of the attach included) and after every FULL commit is durable,
    never before. At attach a tip below `max_commit_seq` is refused as
    `Rollback`, and a different hash at the same height as `Fork` (an
    abandoned branch, ADR-24). Offline `fsck --repair --truncate-tip` and an
    explicit operator reset are the only things that lower it.
    - Phase 5 adds the maximum tip over the InSync mirrors (an authority
      tip below a mirror's is refused) and, for Kubernetes, a raise-only
      floor in the orchestrator's status. A write-only notary bucket in
      another account is the documented later upgrade.
    - **Limits, stated plainly.** A host with no floor (first attach, an
      ephemeral client with no orchestrator floor) cannot detect rollback;
      this is ADR-11 F7's "by construction" case restated for history. A
      purely local volume cannot be protected from anyone who can write its
      local disk: that person also holds `integrity.key` and `state/pins`
      and can rewrite both. For a stolen or modified local disk the honest
      claim is corruption detection and crash safety; integrity mode
      defends the bucket copy, and encrypted mode with a passphrase slot
      resists a reader of the disk, not a writer who also has the key file.
  - **Randomness.** Every salt and nonce (the extent salt, the metadata
    and slot nonces, the Argon2id salt) and every generated root key comes
    straight from the operating system through `getrandom(2)` on every
    call. A userspace DRBG, such as the one behind `aws-lc-rs`'s
    `rand::fill`, is not used: it reseeds from the OS only every few
    thousand calls and does not see a VM snapshot restore, which would
    replay salts. The kernel CRNG is reseeded on VM fork (vmgenid, Linux
    5.18), below the ublk floor.
  - **Hostile keyring cost.** The format bounds a slot at 1 GiB, t 10 and
    p 16, and a keyring may hold up to 32 slots, so a bucket writer can make
    an attach cost minutes of CPU and a gibibyte of memory inside `lunixd`.
    The daemon therefore applies a policy ceiling beyond the format: a
    maximum Argon2id memory per slot (default 256 MiB, configurable) and a
    total time and memory budget over all slots tried in one unlock; a
    slot over the ceiling is skipped and reported. It tries first the slot
    that unlocked the last attach on this host, and wipes the Argon2id
    working blocks after each attempt. Wrong passphrase, corrupted wrap,
    foreign family and bad MAC remain one error after the same work.
  - **Key files.** A key file is opened without following symlinks and
    without blocking (`O_NOFOLLOW | O_NOCTTY | O_NONBLOCK`) and must be a
    regular file. Group or other permission bits are logged, not refused (a
    Kubernetes mount is not 0600). `integrity.key` is created `create_new`,
    0600, with the file and its directory fsynced.
  - **Forged `dek_gen`.** A header `dek_gen` above the keyring's newest is
    reported as an authentication failure, like the other checks done
    before the AEAD: before authentication a forged header cannot be told
    from a writer bug.
  - **Index runs** are checksummed and never MAC'd or sealed. They are
    hints: reads verify what they find, and a dedup hit from a run is
    confirmed against the extent's `chunk_id` and tag first (§5, §7).
  - **Slot removal revokes nothing already seen:** removing a slot or
    changing a passphrase leaves the KMK unchanged for anyone who held it.
    Revocation is migration to a new KMK. DEK rotation (`dek_gen`)
    protects only new writes.
  - **Plaintext at rest locally:** the journal, the dirty overlay and the
    keeper-held write buffer are plaintext, and so may be the old
    allocation of a buffer that held decrypted bytes and was reallocated
    (ADR-11 F11). Run the local directory on
    dm-crypt or LUKS if the host disk is in the threat model.
  - Keys live in mlocked, zeroized memory, and core dumps are forbidden.
- **Hardening rules carried over from the Constellation audit:**
  - Bounded decode and bounded allocations from decoded counts.
  - Decompression capped by the volume's chunk size, never by the `ulen`
    in an extent header.
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
- **Control API:** a typed request/response protocol of length-prefixed
  JSON frames over the Unix socket, with a protocol version. Frames have
  a size bound. It is not an on-disk format, so it can change between
  releases; the version tells the CLI and the daemon apart. gRPC was
  rejected: it adds a build dependency with no phase 1 benefit (ADR-16).
- **Crypto mode flags.** `--encrypt`, `--integrity` and `--plain` are
  mutually exclusive; with none of them `volume create` makes an
  **integrity** volume (ADR-11).
  - `integrity` creates a key-file slot (kind 2) with a random key in
    `<dir>/<uuid>/integrity.key` (0600) for a local volume, and attach picks
    it up. `--passphrase` adds a passphrase slot instead, for volumes that
    move. `--encrypt` takes the same slot options.
  - `plain` creates no key material and prints a warning: plain detects
    corruption only and protects against nobody who can write the bucket.
  - `backend add` and `volume create ... s3://...` on a plain volume print
    the same warning and need confirmation (`--yes` to skip it).
  - Attaching a plain volume needs no credential.
- **CLI sketch:**

```
lunix volume create NAME --size 200G --dir /data/lunix [--chunk 64K] [--encrypt | --integrity | --plain] [--passphrase] [--compress auto]
lunix volume import NAME s3://bucket/prefix/<uuid> --dir /data/lunix
lunix attach NAME            # → /dev/lunix/NAME     lunix detach NAME [--release]
lunix backend add NAME s3://… [--role authority|mirror]   lunix backend rm NAME <id> [--drain]
lunix mode NAME async --interval 30s --max-unsynced 1G | sync | write-through | local
lunix durability NAME --write-ack local|s3 --flush local|s3   (ADR-27)
lunix local NAME none|cache [--size 100G]|store   (live; ADR-26)
lunix volume create NAME --size 200G --local none s3://…   (S3 only; implies sync)
lunix space NAME --floor 10% --on-full wait --full-timeout 60s --cache-max 100G
lunix resize NAME 300G       lunix snapshot NAME SNAP [--freeze MNT]   lunix snapshot rm|ls NAME
lunix mount-at NAME --snapshot S|--commit <commit_seq>|--time <t>   (read-only time travel)
lunix rollback NAME --to S|<commit_seq>   lunix retain NAME <policy>   lunix diff NAME A B
lunix export NAME --snapshot S -o image.raw   lunix gc NAME --dry-run
lunix compress estimate NAME [--level N] [--dict …] [--sample 1%] [--exact]   (ADR-30)
lunix compress train|apply|ls|rm NAME …                                       (ADR-30)
lunix clone SNAP NEW         (post-v1, ADR-25; v1 `restore SNAP NEW` = full copy)
lunix status [NAME]   lunix doctor s3://…   lunix verify NAME   lunix fsck NAME
lunix warm NAME   lunix migrate plan NAME --add s3://… --remove <id>   lunix steal NAME
lunix upgrade                # QUIESCE → daemon exits (no STOP_DEV) → exec → recover, no unmount
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
- **Durability:** the StorageClass parameters `writeAck` and `flush`
  (`local|s3`, ADR-27).
- **Local tier:** the StorageClass parameter `localTier: none|cache|store`
  (default `cache`) and `cacheSize`. `none` suits diskless or ephemeral
  nodes (ADR-26).
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
| local disk lost, write-through | no acknowledged write lost | same |
| S3 unreachable, async | commits pause; unsynced grows to `B_max`, then writes throttle | retry; alert on RPO age |
| S3 unreachable, sync | FLUSH waits (hard) or EIO after `flush_timeout` | per-volume policy |
| network partition, owner isolated | lease renewal fails → self-fence → read-only | the new owner steals after TTL; the zombie gets 412 |
| two daemons attach the same volume | the second waits for the lease or fails | the EPOCH commit decides; there is never a double writer |
| local FS full | space ladder → wait or ENOSPC | §4.3 |
| host FS EIO / corruption | volume read-only, alert | re-import from the bucket |
| bucket object corrupt | the fetch fails verification | read from a mirror; scrub repairs |
| backend rollback / substitution | a spliced or re-ordered chain (`parent_hash`, epoch) or a stale lease is detected; an older valid prefix is detected on a host with a floor (`Rollback`, `Fork`; §8.4, §15) and not on one without | refuse to mount; a host or client with no floor cannot tell |
| mirror lags / fails | its state drops to CatchingUp | the authority is unaffected |
| `lunixd` crash, S3 only | I/O stalls | recover; replay the keeper-held memfd buffer |
| `lunixd` and keeper both lost, S3 only | the buffer is gone | the device is deleted (EIO), never resumed with stale data; a mounted file system goes read-only; remount from the bucket |
| host power loss, S3 only | unflushed writes lost (legal) | attach from the bucket |

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
| sync-mode fsync latency | ≈ 2 PUT RTT for an ext4/xfs fsync (FLUSH, then FUA; ADR-28), 1 RTT for a data-only fdatasync, plus `sync_dirty_max / goodput` under a bulk writer. S3 Standard, measured in phase 0 from the same region: 4 KiB PUT p50 30 ms, 1 MiB p50 70-78 ms, two serial 4 KiB create-only PUTs p50 54 ms / p99 94 ms ([s3probe](../../bench/s3probe/RESULTS.md)). Express One Zone: PUT p99 ~7 ms at 4 KB, same AZ ([fact-check §C](research/2026-11-11-fact-check.md)) |

The measured baseline is in [bench/BASELINE.md](../../bench/BASELINE.md),
with the §20 target table and a "ceiling measured / target / status"
column. The framework ceiling of libublk's null target at 4 KiB random
read is 978 K IOPS (p99 38.7 µs) on one queue and 3.8 M IOPS with four
queue threads, against `null_blk` at 4.9 M, so the framework leaves
headroom for the product's own work. These were measured at 4 active
queues, not 8.

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

1. **Rollback anchor. Closed (maintainer, 2026-11-12; ADR-31).** A
   malicious bucket can serve an old but valid prefix of the chain, which
   `parent_hash` and `epoch` (ADR-11) do not stop. v1 keeps a local floor
   `{max_commit_seq, tip_hash}` in `state/pins` version 2 (§15); phase 5
   adds the maximum over InSync mirrors and a raise-only Kubernetes floor;
   a notary bucket is a documented later upgrade. A host with no floor, and
   a purely local volume against someone who can write its disk, are not
   protected, and the docs say so. A KMS has no monotonic counter.
2. **Default chunk size.** 64 KiB against 16 KiB. The MSR Cambridge
   simulation favours 16 KiB on upload bytes but costs 4× the map and
   index entries (ADR-5 gate outcome). Decided after phase 1 benchmarks.
3. **Map write amplification** under scattered writes. Measured in
   phase 0: 0.18-0.55 of the dirty logical bytes in map bytes. DELTA-
   style map-delta records are the fallback if phase 1 finds that too
   high.
4. **Cache policy.** S3-FIFO narrowly fails the ADR-14 rule on MSR
   Cambridge; the policies differ by at most about 4 %. Revisited in
   phase 2 with segment-granular eviction.
5. **Express One Zone as a `sync-fast-wal` tier.** A later ADR.
6. **Cross-volume dedup on the node cache** (golden images) without
   leaking across families.
7. **Staging raw journal batches ahead of FLUSH** (`refs.kind =
   journal`). Opened as ADR-29 only if the ADR-28 phase 3 gate shows
   fsync p99 under a 200 MB/s mix above 2× the two-PUT floor.
8. **USER_COPY through io_uring.** The blocking `pread`/`pwrite` copy
   lost about 10% at 4 KiB; issuing it with the backend SQEs may
   remove that loss (ADR-17). Untested.
