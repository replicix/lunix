# Lunix Decisions (ADRs)

Each record gives the decision, the alternatives rejected, and why. The
context is in [DESIGN.md](DESIGN.md), and the evidence is in the dated
notes under [research/](research/). New decisions, and changes of mind,
are appended here.

**Status:** every ADR below is **proposed** (2026-11-11). It becomes
*accepted* when the maintainer says so, or when a phase 0 spike confirms
it (marked *gate:*).

## ADR-1: A new Rust daemon on ublk + io_uring, separate from Constellation

**Decision:** Lunix is its own repository and binary. It is a ublk server
that uses the `libublk` crate for the per-queue io_uring loops and the
`io-uring` crate for its local files. It shares extracted crates with
Constellation (ADR-19) but is not a Constellation frontend.

**Rejected:**
- *A Constellation file exported as a loop or ublk device.* This would
  bring POSIX semantics, multi-writer leases, P2P and the metadata log
  into every block I/O. Constellation's 4 MiB chunks and per-chunk objects
  are also wrong for 4 KiB random writes.
- *NBD.* It is slower; the 2022 qcow2 comparison measured ublk about 4×
  faster on 4K random IOPS. A daemon restart drops the device, and the
  ceph-csi rbd-nbd experience shows the same problem in Kubernetes.
- *FUSE file + loop.* It adds two extra layers and double caching.
- *SPDK / vhost-user-blk.* These need dedicated cores and hugepages, or a
  VM boundary; that is not a general host device.
- *TCMU / LIO.* These are SCSI-shaped and slower.
- *A kernel module.* Excluded by the goals.

**Evidence:**
[ublk note](research/2026-11-11-ublk-io-uring-rust.md),
[prior art](research/2026-11-11-prior-art.md).

## ADR-2: Single writer per volume; conditional writes required on the authority backend

**Decision:**
- A volume has exactly one writer, the *owner*, at a time.
- The owner holds the volume's lease on the **authority** backend. That
  lease is a CAS object carrying a monotonic `epoch`.
- Every commit is a create-only object (ADR-9).
- The authority backend must pass the `If-None-Match: *` and `If-Match`
  probes.
- A *mirror* backend may lack conditional writes. Such a mirror can never
  become the authority, which means the volume cannot fail over to it
  without a manual `--unsafe` override.

**Rejected:**
- *Multi-writer.* File systems on a block device cannot tolerate it.
- *An external lock service* (etcd, Redis or DynamoDB). That is extra
  infrastructure, and it breaks when the volume moves between clusters.
- *Clock-based leases only.* They are unsafe under process pauses and
  partitions; Constellation ADR-12 makes the same argument.

**Consequences** (verified 2026-11-11 in
[the fact-check](research/2026-11-11-fact-check.md)):

- **Can be the authority:** AWS S3 (General Purpose and Express One
  Zone), R2, OVH, Scaleway, Azure (native ETag conditions), MinIO and
  Tigris (needs `X-Tigris-Consistent: true`).
- **GCS:** has no S3-style conditional PUT. It needs a provider adapter
  that uses `x-goog-if-generation-match` (0 means create-only).
- **Backblaze B2:** returns `501` for `If-None-Match: *`, so it is
  **mirror-only**.
- **Ceph RGW:** its conditional PUT is not atomic on *versioned* buckets
  (tracker 80925). Hetzner lists conditional PUT on versioned buckets as
  an exception.
- **Authority versioning rule:** an authority bucket must therefore have
  versioning **off**, unless `doctor`'s concurrent-CAS probe passes on
  the versioned bucket.
- **Probe before use:** Wasabi, DigitalOcean Spaces and Hetzner have no
  vendor statement and must pass the probe.

## ADR-3: Consistency contract: a volatile write cache with prefix-consistent cuts

**Decision:**
- The device advertises a volatile write cache and FUA, through
  `UBLK_ATTR_VOLATILE_CACHE | UBLK_ATTR_FUA`.
- A completed write is *volatile*. A completed FLUSH, or a FUA write,
  is durable at the volume's durability level:
  - local fdatasync for local-only volumes and in async mode;
  - an S3 commit in sync mode.
- Every write gets a global sequence number `seq` at admission.
- A **cut** at `c` is the device state after applying every write with
  `seq ≤ c`, overlaps resolved by `seq`. A cut is taken only once all
  writes with `seq ≤ c` have reached the journal.
- Every commit on S3 is a cut. A cut contains every write acknowledged
  before it was taken, so it is a legal post-power-loss image for any
  file system or database.

**Rejected:**
- *Advertising write-through (no volatile cache).* Then every write would
  need durability before its acknowledgement, and the file system would
  never send FLUSH, so sync mode would be impossible to make fast.
- *Arbitrary subsets at sync points.* Ordering matters to journaling file
  systems.

**Evidence:** [cache & consistency §1](research/2026-11-11-cache-consistency-s3-protocol.md),
[novel ideas §1.3](research/2026-11-11-novel-ideas.md).

## ADR-4: Journal-first write path; dedup, compression and encryption at destage

**Decision:**
- A write is appended to a local O_DIRECT journal, which is preallocated,
  checksummed and 4 KiB-granular. The dirty-extent overlay is updated,
  and the request completes.
- Zero detection, hashing, dedup, compression, encryption and packing run
  later, in a **destage** pipeline on a worker pool.
- FLUSH is one group-committed `fdatasync` of the journal.

**Rejected:**
- *Inline hashing and compression on the acknowledgement path* (VDO or
  ZFS style). It costs microseconds per 4K and a core at about 1M IOPS,
  and leaves nothing to absorb overwrites.
- *Writing chunks directly without a journal.* Every partial-chunk write
  would then need a synchronous read-modify-write (RMW).

**Consequences:**
- Reads consult the overlay first.
- Overwrites within a commit window are absorbed, so S3 sees only the last
  version.
- Journal space is reserved before a write is accepted.

## ADR-5: Fixed, aligned chunks; default 64 KiB; no content-defined chunking

**Decision:**
- The logical chunk size is a per-volume power of two from 16 KiB to
  1 MiB, fixed at creation. The default is **64 KiB**.
- The block size is 4096.
- A partial-chunk overwrite is merged with the old chunk at destage. That
  is a cold RMW fetch if the old chunk is not cached.
- A `db` profile uses 16 KiB.

**Rejected:**
- *CDC (FastCDC).* Block devices are aligned and overwritten in place,
  and CDC adds boundary-shift cost for no dedup gain.
- *4 KiB chunks.* The map would be 16× larger (8-12 GiB per TiB) and the
  BLAKE3 SIMD lanes underused.
- *Constellation's 4 MiB.* The RMW amplification is absurd for 4K writes.

**Trade-offs:**
- 64 KiB keeps the map at about 0.75 GiB per TiB, paged.
- 16 KiB gives better dedup for VM images and less RMW per random write,
  at 4× the map.

*Gate:* a phase 0 trace-driven simulation (RMW amplification, uploaded
bytes, map size) on the MSR, Alibaba and Tencent block traces confirms
the default.

**Evidence:** [data path §1](research/2026-11-11-data-path-hash-compress-encrypt.md).

## ADR-6: Chunk identity = BLAKE3-256 of the plaintext; zeros are never stored

**Decision:**
- `chunk_id = blake3(plaintext)`. When the volume family is encrypted it
  is `blake3::keyed_hash(addressing_key, plaintext)`; this is
  Constellation's ADR-10 scheme.
- All-zero chunks are found with an OR-accumulate SIMD scan, measured at
  100+ GB/s. They become **holes** in the map and are never hashed or
  stored.
- DISCARD and WRITE_ZEROES are journaled as hole records.
- An optional FIPS profile uses HMAC-SHA-256.

**Rejected:**
- *xxh3 as the identity.* It is not collision-resistant, so dedup would be
  unsafe. xxh3 is used only for in-memory table keys, always checked
  against the full id.
- *SHA-256 by default.* It is 2.3× slower than BLAKE3 on this host.

## ADR-7: Packed segments in one format, locally and on S3

**Decision:**
- Destaged chunks are packed in write order into immutable **segments**.
  The target size is 16 MiB (range 4-64 MiB).
- Each extent in a segment has a framed header (codec, `ulen`, `clen`,
  `chunk_id`, AEAD tag), and the segment ends with a footer index.
- Local segments use **the same bytes** as S3 segments. Initial sync is
  therefore a file copy, a local-only volume needs no conversion when a
  backend is attached, and the local store is a byte-exact cache.
- The extent framing reuses Constellation's `CCH1` header and codec
  registry, adding LZ4 as codec 2.

**Rejected:**
- *One object per chunk* (Constellation's data layout, and s3backer).
  At 64 KiB that is 16 M objects and about $80 in PUTs per TiB,
  against about $0.33 with 16 MiB segments. It also makes LIST and GC
  untenable.
- *Separate local and remote formats.* That would mean a conversion step
  and double code.

## ADR-8: The block map is a fixed-radix Merkle tree with location hints

**Decision:** the LBA → chunk map is a fixed-fanout radix tree.

- **Leaf entries:**
  - Each leaf holds 512 entries, covering 32 MiB of LBA at 64 KiB chunks.
  - An entry is a stored **id prefix** plus a **location hint** (segment,
    offset, `clen`), or *hole*.
  - The id prefix is the first 16 B of `chunk_id`. The id length is a
    per-volume parameter fixed at creation, `0 | 16 | 32` B, with 16 as
    the default.
- **Leaf encoding (amended 2026-11-11):** leaves are **run-encoded with
  bit-packed fixed-width columns**:
  - Each leaf has a dictionary of the segments it references.
  - A run of consecutive chunks that are contiguous in one segment stores
    the segment and start offset once.
  - Each entry adds its `clen`, packed in about 17 bits.
  - Every 16 entries there is a restart point holding the running offset
    sum, so any entry can be looked up in O(1) without decoding the leaf.
  - Holes are runs.
  - Leaves get no outer zstd or lz4: it saves under 1% and makes a point
    lookup 2-100× slower.
- **Leaf encoding costs** (measured on synthetic 1 TiB maps at 64 KiB
  chunks; see [the leaf-encoding note](research/2026-11-11-map-leaf-encoding.md)):

  | Encoding | Size per TiB |
  |---|---|
  | Old layout, 48 B entries | 768 MiB |
  | Old layout + zstd | 630-650 MiB |
  | Run-encoded, 16 B ids | **294-352 MiB** (sequential to fragmented) |

  A point lookup on an encoded leaf takes 121-181 ns; a full decode takes
  0.8-3.6 µs.
- **Id-length rules:**
  - **16 B ids are the minimum safe length.** A 16 B prefix stays
    collision-safe against a hostile bucket or guest, in both keyed and
    unkeyed mode.
  - **8 B ids are rejected.** A guest can grind an unkeyed collision in
    about 2^32 work, and a keyed family has swap probability 2^-9 at
    16 TiB.
  - **Dedup is unaffected.** It always compares full ids from footers and
    the chunk index.
- **Lean profile (no id, 38-96 MiB/TiB): specified, not enabled.** It is
  enabled only after an ADR settles two things:
  - Position-bound extent authentication, or header self-location for
    unencrypted volumes.
  - Forwarding records for stale hints, because the chunk index can no
    longer be keyed from the leaf.
- **Interior nodes:**
  - An interior entry is the child's hash plus the child's location.
  - A node is identified by its content hash. Nodes are immutable and are
    packed into map-pack objects.
- **Hot path:** a mutable overlay serves the hot path. Only the dirty
  leaves, and their paths to the root, are rewritten when a commit is
  sealed.
- **Shape:**
  - The shape depends only on the LBA, so identical content gives
    identical roots on any backend.
  - Holes share one well-known empty-node hash, so sparse volumes cost
    nothing.

Hints make a cold read cost one GET. When compaction moves a chunk, the
current map's hint is rewritten lazily. An older snapshot with a stale
hint falls back to the chunk index (ADR-14).

**Rejected:**
- *A prolly tree* (Constellation's `mtree`). Its history independence
  matters for shifting, variable keys; LBAs never shift, so a radix tree
  is history-independent by construction and simpler.
- *A flat per-extent table.* Commit cost would be O(volume), with no
  O(changed) diff and no root-hash verification.
- *Leaves holding locations only, with no chunk id.* Dedup and
  verification would be impossible.
- *Leaves holding chunk ids only, with no hint.* Every cold read would
  need two GETs.
- *Slot references* (`segment, slot-in-footer`). The leaf shrinks, but
  the footer-side offset and length table costs more than it saves
  (82-107 MiB/TiB). Every cold read would also need a footer GET first.
- *Varint run streams.* A lookup must scan the stream (1.1-2.7 µs), and
  the result is 24% larger than bit-packed columns.

**Enables:**
- Snapshot and backend diffs in O(changed).
- Clones from a single 32-byte root.
- Proof that two mirrors are identical by comparing roots.
- Lazy map loading on a cold mount.

**Evidence:** [novel ideas §1.1](research/2026-11-11-novel-ideas.md),
[constellation reuse §5](research/2026-11-11-constellation-reuse.md).

## ADR-9: A create-only commit chain with an epoch lease; no mutable root

**Decision:**
- **The commit chain:**
  - The volume's history is a chain of create-only objects
    `commits/<seq:016x>`, written with `If-None-Match: *`. This is
    Constellation's `commits.rs` pattern.
  - Commit kinds:
    - **FULL**: map root, size, parent, epoch, `cut_seq`, the segments
      it introduced, optional heat-map reference.
    - **DELTA**: journal records inline, compressed and encrypted, on top
      of the last FULL.
    - **EPOCH**: owner change.
    - **CONFIG** and **TRANSFER**: backend membership and authority
      moves (ADR-24).
  - Every object a commit names is durable before the commit is created.
- **Ownership:**
  - `lease` is a CAS object (`If-Match`) holding the epoch, owner,
    renewal deadline and a `released` flag.
  - A new owner first bumps the epoch on `lease`. It then writes an
    **epoch-marker** commit at the next free slot before serving any I/O.
  - A zombie writer loses its next slot (412) and fences itself.
- **Finding the tip:** `head` is an advisory hint only. The tip is found
  by LIST plus probing forward.

**Rejected:**
- *A single mutable root object updated with `If-Match`.* R2 and GCS
  throttle rewrites of the same key to about 1 per second, and there would
  be no history.
- *Writer-epoch-in-key with no CAS.* That needs reader-side
  disambiguation.
- *Fencing that depends on clocks.*

## ADR-10: Durability modes: local, async (default with a backend), sync

**Decision:**
- **local:** there is no backend, and FLUSH = journal `fdatasync`.
- **async:**
  - FLUSH is local. A FULL commit is cut every `T` (default 30 s) or
    every `B` dirty bytes (default 1 GiB), whichever comes first. Idle
    volumes cut nothing.
  - Unsynced data has hard backpressure: writes stall at `B_max`, so the
    data-loss window is a promise, not a hope.
- **sync:** FLUSH and FUA complete when a DELTA commit holding everything
  up to the cut exists on the authority. That is one create-only PUT per
  flush group, with concurrent flushes coalesced. FULL commits fold
  DELTAs in the background on the async cadence.
- The mode can be changed at runtime.

**Rejected:**
- *A FULL commit per FLUSH.* That is several PUTs per fsync, which is too
  slow and too costly.
- *An S3-Express-only sync tier.* It is not portable. It is kept as an
  optional accelerator for later (`sync-fast-wal`), never as the only
  path.

**Cost note:** in sync mode at 100 flush groups per second, S3 Standard
PUT fees are about $1,300 a month. The CLI warns about this and shows the
measured rate.

## ADR-11: Encryption: AES-256-GCM with per-chunk derived keys; Constellation's key hierarchy

**Decision:** encryption is chosen at volume-family creation.

- **Key hierarchy (taken from Constellation's `e2e.rs`):**
  - A master key (KMK) is wrapped by one or more key slots: Argon2id
    passphrase, KMS key or raw key file. Several slots let a volume
    switch KMS when it changes cloud.
  - The addressing key, the data-key epochs and the metadata key are
    derived from the KMK by purpose, using keyed BLAKE3.
- **Data encryption:**
  - Every extent carries a fresh random 192-bit `salt` in its header.
  - The extent key is `blake3::derive_key("lunix extent v1",
    DEK_epoch ‖ chunk_id ‖ salt)`.
  - The cipher is AES-256-GCM with a fixed nonce, because every key
    encrypts exactly one payload. It runs through `aws-lc-rs`; ring
    measured 11 GB/s per core on this host.
  - The AAD is the format version, codec, level, `ulen` and `chunk_id`.
- **Consequences:**
  - There is no nonce-space limit. A (key, nonce) pair never repeats,
    even when the same chunk is recompressed (ADR-12) or re-encoded by a
    newer zstd.
  - Re-packing and mirroring copy extent bytes verbatim, so they never
    re-encrypt.
  - Commits and the lease use random-nonce XChaCha20-Poly1305 under the
    metadata key.
- **Amended 2026-11-11, before acceptance:** the first draft derived the
  key from `chunk_id` alone. That produced deterministic ciphertext and a
  GCM nonce-reuse hole whenever the same plaintext was compressed to
  different bytes. The random salt removes the hole.

**Integrity:**
- Every fetched extent is checked: AEAD tag, then decompression with a
  `ulen` cap, then the chunk id is recomputed.
- FULL commits carry a MAC over the map root, epoch and parent. Rollback
  detection needs an anchor outside the bucket; see the open questions in
  DESIGN.md.

**Rejected:**
- *Per-chunk random-nonce XChaCha20-Poly1305.* That is Constellation's
  choice. It runs at about 2-3 GB/s, and re-packed or mirrored
  ciphertext would differ.
- *Unkeyed convergent encryption.* The provider could confirm known
  files.
- *AES-GCM-SIV.* About 5× slower in the measured crates.

**Gate:** a crypto design review before the format is frozen. A FIPS
profile uses HKDF-SHA-256 and HMAC-SHA-256.

## ADR-12: Compression `auto`: an LZ4 probe, then zstd-1; recompress on seal

**Decision:**
- Each chunk gets a 4 KiB LZ4 probe. If the probe saves less than 6%, the
  chunk is stored raw.
- Otherwise the chunk is compressed with zstd level 1, using reused
  contexts.
- Under worker backpressure the codec degrades to LZ4, then to raw.
- Volume options are `lz4`, `zstd:N`, `none`.
- Background recompression of cold segments (zstd 9+) is an option.
  It is possible because chunk ids hash the plaintext.

**Rejected:**
- *A fixed high zstd level.* It is CPU-bound at roughly 0.4-0.6 GB/s per
  core on hard data.
- *Dictionaries in v1.* Deferred.

## ADR-13: Dedup domain = volume family; no cross-tenant dedup

**Decision:** dedup happens inside a **family**: a volume, its snapshots,
and its clones. Clones of a golden base volume share the base's chunks
through explicit lineage. The clone's family holds a wrapped base
addressing key.

**Rejected:** a global or cross-tenant dedup index, because of the side
channel described by Harnik et al. (2010) and the confirmation-of-file
attack.

**Amended by ADR-25:** within a family, a volume dedups only against its
own chunks and its frozen lineage bases. It never dedups against data
owned by an independent writer.

## ADR-14: One local store; S3-FIFO cache; out-of-space ladder

**Decision:**
- **Local store contents:**
  - the journal;
  - own segments, the same files as on S3;
  - a chunk-granular read-cache slab for fetched extents;
  - map-node packs;
  - the **chunk index** (`chunk_id → location`), kept as LSM runs that
    can be rebuilt from segment footers.
- **Eviction:**
  - Clean data is evicted by **S3-FIFO**: whole own segments once they
    are synced and cold, otherwise slab entries.
  - Dirty and unsynced data is outside the evictable set.
- **Out of space:**
  - The space ladder runs in this order: evict clean, compact, throttle,
    then the per-volume `on-full` policy. That policy is `wait` (optional
    `full-timeout`, after which ENOSPC) or `error` (immediate ENOSPC).
  - Reads never fail for space. An EIO from the host file system is fatal
    for the volume (it goes read-only and an alert is raised).
  - The journal and commits have a reserved floor.

**Rejected:**
- *File-per-chunk with a global mutex.* This is Constellation's
  `DiskCache`; Lunix copies its state machine but not its layout.
- *An embedded KV for the data.*
- *ARC.* There was patent history, and it has no advantage over S3-FIFO on
  scans.

*Gate:* a trace-driven cache simulation (S3-FIFO against SIEVE and
W-TinyLFU) in phase 0.

## ADR-15: Multiple backends: one authority, byte-identical mirrors, a per-backend cursor

**Decision:**
- **Roles and contents:**
  - A volume has one authority backend and zero or more mirrors.
  - Mirrors receive byte-identical segments, map packs and commits.
  - A commit is copied last, so every backend always holds a valid
    prefix of the chain.
  - Each backend has a cursor, the highest commit it holds.
- **Backend lifecycle:** `Joining` (backfill: hot first, restartable,
  AIMD, bandwidth- and budget-capped) → `CatchingUp` → `InSync` →
  `Draining` → `Removed`.
- **Authority transfer:** `set-authority` is a live, owner-driven
  protocol. It is decided by one CAS (a TRANSFER commit) on the old
  authority, and needs no detach. Membership and backend states are
  CONFIG commits in the chain (ADR-24).
- **Policies:** `primary-async` is the default. `sync-quorum k/n` and
  `sync-all` come later.
- **Reads:** reads choose by EWMA latency and egress cost, with hedging,
  using Constellation's `sources.rs` scoring.

**Rejected:**
- *Erasure coding across clouds.* Egress and request multiplication are
  too costly for a latency-sensitive device. The manifest keeps location
  sets so it can be added later.
- *Provider replication* (CRR and similar). It is single-vendor, and its
  ordering is not commit-aware.

**Evidence:** [CSI, multi-backend & mobility](research/2026-11-11-csi-multibackend-mobility.md).

## ADR-16: Process model: one `lunixd` per host; stateless CSI plugin; user recovery

**Decision:**
- One `lunixd` per host owns every ublk device and local store on that
  host. It is a privileged DaemonSet under Kubernetes.
- Devices are created with `USER_RECOVERY | USER_RECOVERY_REISSUE`. A
  daemon crash or upgrade stalls I/O; it never deletes the device.
  `lunix upgrade` = QUIESCE, exec the new binary, recover.
- The CLI and the CSI node plugin talk to `lunixd` over a Unix socket.
- The CSI plugin holds no data path.

**Rejected:**
- *A data daemon inside the CSI plugin pod.* That is the ceph-csi rbd-nbd
  failure mode.
- *A process per volume.* It multiplies pinned threads and hides shared
  cache and dedup.

## ADR-17: Kernel floor and ublk feature set

**Decision:**
- Probe `UBLK_U_CMD_GET_FEATURES` at startup.
- **Mandatory flags:**
  - `CMD_IOCTL_ENCODE`, which this host's Ubuntu 7.0 kernel needs because
    it builds without `BLKDEV_UBLK_LEGACY_OPCODES`.
  - `USER_RECOVERY`, `USER_RECOVERY_REISSUE`, `QUIESCE` and
    `UPDATE_SIZE`.
- **Used when present:** `BATCH_IO` (7.0), `SAFE_STOP_DEV` (7.0),
  `NO_AUTO_PART_SCAN` (7.0) and `USER_COPY` (6.5). USER_COPY is used
  only if the spike shows it wins.
- **Never used:** `NEED_GET_DATA`, zero-copy and `AUTO_BUF_REG`. Lunix
  must read every byte, and zero-copy buffers are unreadable by the
  server.
- **Kernel versions** (verified against the uapi headers at the tagged
  releases; see [the fact-check §E](research/2026-11-11-fact-check.md)):
  - The mandatory flags arrived as follows: `CMD_IOCTL_ENCODE` in 6.4,
    `USER_RECOVERY` and `REISSUE` in 6.0, and `QUIESCE` and `UPDATE_SIZE`
    in 6.16.
  - The floor is therefore **Linux 6.16**.
  - The supported and tested kernel is **7.0+**, which has BATCH_IO,
    SAFE_STOP_DEV and NO_AUTO_PART_SCAN.
  - `UBLK_ATTR_VOLATILE_CACHE` and `UBLK_ATTR_FUA` exist since 6.0. FUA is
    honoured only together with VOLATILE_CACHE.

## ADR-18: Threads and runtimes

**Decision:**
- One pinned thread per ublk queue runs libublk's executor (4-16 queues,
  QD 128-256, mlocked buffers, `PR_SET_IO_FLUSHER`). It handles I/O, the
  zero scan, journal appends and small decompressions.
- A bounded OS-thread worker pool handles hash, compress, encrypt and
  large decompress.
- A tokio multi-thread runtime handles S3 and control.
- Queue threads never run tokio and never block.
- Local files use the `io-uring` crate directly (registered files, fixed
  buffers, linked write→fsync).

**Rejected:** tokio-uring, monoio and glommio (stale as of 2026), and tokio
on the queue threads.

**Pin:** `libublk` 0.4.x until 0.5.0 (tokio executor) is released and
evaluated.

## ADR-19: Shared crates with Constellation (needs the maintainer's decision)

**Proposal:** extract a public MPL-2.0 `replicix-core` workspace that both
projects depend on:

- `upload-concurrency`, as-is.
- `objcas`: `cas.rs`, `classify.rs`, `probe.rs`, `aws_auth.rs`, the
  lease (with the clock decoupled) and the preflight machinery.
- `chunkfmt`: `ChunkHash`, the `CCH1` format, the codec registry plus
  LZ4, and the `e2e` key hierarchy.
- `platform-lite`: lock, fs primitives and core-dump forbid.
- `csi-common`: proto, credentials, params and the volume-id codec.

Constellation migrates at its own pace.

**Alternatives:**
- *A git dependency on Constellation's workspace.* It drags in fs-core,
  mtree and platform, and its `[patch]` entries do not propagate.
- *Copying files into Lunix.* Fixes would diverge. This is acceptable
  only as a phase 0 stopgap, with MPL headers kept.

**Evidence:** [constellation reuse](research/2026-11-11-constellation-reuse.md).

## ADR-20: Resize: online grow; explicit shrink

**Decision:**
- **Grow** records the new size in the next commit and calls
  `UPDATE_SIZE`. The file system grows with `resize2fs` or `xfs_growfs`,
  done by the CSI `NodeExpandVolume` or by the user.
- **Shrink** requires `--discard-tail`. Lunix refuses if the tail holds
  mapped chunks, unless `--force`; the user must shrink the file system
  first. CSI never shrinks.

## ADR-21: Snapshots, clones and time travel from retained commits

**Superseded by ADR-25**, which splits this into v1 (snapshots, time
travel, rollback, retention, space accounting) and post-v1 thin clones,
with the clone format frozen now.

## ADR-22: Kubernetes: the bucket is the truth; a minimal CRD surface

**Decision:**
- **Volume IDs:** `lunix1:<uuid>`, plus a `backendSet` volume attribute.
  This makes static PVs portable, so Velero, Karmada and Argo can move
  them.
- **CRDs in v1:** only `LunixBackendSet`, which lists endpoints, roles,
  credentials refs and cost weights.
- **CSI:**
  - Block and Filesystem modes, RWO and RWOP.
  - `attachRequired: false`.
  - Online expand.
  - Snapshots per ADR-25. Restore-from-snapshot is a full copy in v1;
    thin clones are post-v1.
- **Deletes:** `DeleteVolume` goes through a trash window. It is refused
  while another cluster's owner holds the lease.

**Rejected:**
- *Authoritative CRs.* Longhorn's model ties volumes to one cluster's
  etcd.

## ADR-23: A warm-start heat map ships with the volume

**Decision:**
- FULL commits may reference a `heat/<seq>` object: sampled hot chunk
  ranges, plus an ordered first-touch trace from the last attach.
- A new owner prefetches in that order from the nearest backend before
  and after attach.
- `lunix warm` does the same on a target node ahead of a planned move.

The format is part of v1. Its population is phase 7.

**Rejected:** ML prefetch (cost, opacity).

## ADR-24: Backend membership lives in the commit chain; authority transfer is live

**Context:**
- Once a volume has a backend, the client is ephemeral. Every host must
  be able to mount the volume from S3 alone, including which buckets it
  uses, their states and which one is the authority.
- The first draft kept the backend set only in the local `volume.json`.
- It also allowed `set-authority` only while the lease was released,
  which meant a detach and a pod disruption.

**Decision:**
1. **CONFIG commits.**
   - A new commit kind, `CONFIG`, carries the volume's backend
     configuration:
     `{config_version, authority: backend_id, backends: [{id, url,
     role, state, since_seq}], policy}`.
   - Every membership or state change is a CONFIG commit on the
     authority's chain, written create-only like every commit:
     - `backend add` → Joining;
     - Joining → CatchingUp → InSync;
     - `set-authority`;
     - Draining → Removed;
     - a policy change.
   - Live progress is advisory and lives in
     `sync/<backend-id>/progress`, a plain PUT written only by the owner:
     backfill progress and per-backend cursors. Copies are idempotent
     because objects are immutable and content-addressed, so progress
     can always be rebuilt by a LIST/HEAD diff.
   - No other mutable membership state exists in S3 or in Kubernetes.
2. **One arbiter.**
   - Conditional writes happen only on the current authority.
   - Mirrors receive byte-identical copies of every commit (CONFIG
     included), in `seq` order, after the objects each commit names. So
     each mirror holds a valid prefix of the authority's chain, including
     its config history.
   - Mirrors need no CAS.
   - Writing transitions with CAS into *every* bucket is rejected (see
     below).
3. **Live authority transfer** from A to B, while the pod keeps doing
   I/O. The owner runs these steps:
   1. B must be `InSync` and must pass the authority probe (ADR-2).
   2. The owner pauses commit *publication*. I/O keeps landing in the
      journal and the cache, and reads keep using any InSync backend.
   3. The owner copies A's remaining commits up to tip `t` to B.
   4. The owner writes `TRANSFER{to: B, epoch: e+1, config_version+1}`
      at `commits/t+1` on **A** (`If-None-Match`). **This CAS is the
      linearization point.** Before it A is the authority; after it B is.
      A zombie committing to A gets 412, reads the TRANSFER and
      self-fences.
   5. The owner copies `t+1` to B, creates `lease` on B (`If-None-Match`,
      epoch `e+1`, same owner), then writes `EPOCH{authority: B}` at
      `t+2` on B.
   6. Publication resumes on B. A is demoted to mirror and receives
      `t+2…` as copies, or is drained per policy: a CONFIG commit on B
      marks it Draining, then Removed. A's `lease` is updated to
      `transferred_to: B` (CAS, best effort; the chain is authoritative).
   - **Visible cost:**
     - async mode: none;
     - sync mode: the in-flight flush group waits for about 3 extra PUT
       round trips, once.
   - **Crash recovery:** a starting owner that finds TRANSFER at A's tip
     rolls forward through steps 5-6. A starting owner that finds no
     TRANSFER treats A as the authority. There is no ambiguous
     intermediate state.
4. **Discovery from any bucket:**
   1. Read `volume`.
   2. Find that backend's tip and read its latest CONFIG.
   3. Go to the named authority.
   4. Follow `TRANSFER.to` pointers until a backend's tip is not a
      TRANSFER.
   - A lagging mirror may name an old authority; that old authority's
     TRANSFER points onward.
   - **Purge rule:** a former authority is never purged until every
     remaining backend's cursor is past its TRANSFER commit, so the
     pointer chain stays intact.
5. **Credentials stay out of S3.**
   - CONFIG names backends by `id` and `url` only.
   - Each host or cluster resolves `id → credentials`: through
     `LunixBackendSet` and Secrets or workload identity (ADR-22), or
     through local credential providers when standalone.
   - Mounting needs credentials for at least one InSync backend plus the
     authority.
6. **DR steal stays the one forced exception.**
   - `lunix steal --backend B --force` writes `EPOCH + CONFIG{authority:
     B}` on B at B's own tip, losing whatever B had not received.
   - If the old authority is merely partitioned, its owner can keep
     committing there until it is fenced, so the old site must be fenced
     first (ADR-15, DESIGN §12).
   - When the old authority becomes reachable, a `TRANSFER{to: B,
     reason: steal}` is written at its tip if its slot is free. If the
     slot is taken, the old chain beyond the steal point is marked
     *abandoned* and quarantined for manual export.

**Rejected:**
- *CAS-writing every transition into every bucket.* There is no
  atomicity across buckets, so a partial failure leaves two "truths" and
  needs a two-phase commit with its own recovery state. It also forces
  CAS on mirrors.
- *A mutable `backends` object per bucket.* That is a second source of
  truth beside the chain, and R2 and GCS throttle rewrites of the same
  key.
- *Membership in Kubernetes CRs.* It ties the volume to one cluster's
  etcd, and fails when the client is ephemeral or standalone.
- *Authority transfer only while the lease is released* (the first
  draft). It needs a detach, which is a pod disruption.

## ADR-25: v1 ships snapshots, time travel, rollback and retention; thin clones come later in a format ready now

**Context:**
- ADR-21 sketched snapshots, clones and time travel in four bullets.
- Thin clones share the parent's data. If that data sat in a family-wide
  pool written and garbage-collected by several owners, multi-writer GC
  and the dedup-versus-delete race would return.
- The maintainer decided (2026-11-11) that everything except clones is
  v1, and that clones may come later. The on-bucket format must already
  support them, because the format freezes in phase 1.

**Decision: in v1** (DESIGN §8.6-8.7, §11):
1. **Snapshots.**
   - `snapshot` forces a cut, then creates `snapshots/<name>`
     (`If-None-Match`) naming that FULL commit.
   - Consistency is `crash` by default. `fs-frozen` is used when a
     mountpoint is given: `fsfreeze` wraps the cut, and the CSI node
     plugin does this for VolumeSnapshots when `snapshot.freeze=true`.
   - Pins are volume objects. They are copied to mirrors after their
     commit, and they survive TRANSFER (ADR-24).
2. **Restore points.**
   - Every retained commit is a restore point: FULL, or a DELTA
     replayed on top of the previous FULL.
   - The granularity is the commit cadence: `T`/`B` in async mode, and
     every flush group in sync mode.
   - "Every FLUSH is a restore point" holds only in sync mode. Async
     users who want finer restore points lower `T`.
3. **Retention.**
   - Tiered thinning, such as `last:10, within:24h, every:1h:48,
     every:1d:30`, plus pins.
   - Commits that fall out of retention stop being GC roots.
   - Old commit objects are deleted only below `chain_floor`, which
     every FULL commit records as the lowest retained seq, so tip
     discovery and mounting are unaffected.
4. **Time-travel mounts.**
   - `mount-at --commit|--time|--snapshot` exposes a read-only ublk
     device (`UBLK_ATTR_READ_ONLY`) on any host, without the lease.
   - The mount holds `readers/<host>-<id>`, a pin with a deadline
     renewed while mounted, which GC treats as a root.
   - Reads come from the node cache, then the nearest InSync backend.
5. **Rollback.**
   - `rollback --to <snapshot|seq>` writes a new FULL commit whose
     `map_root` is the old one, with `parent` = the current tip. It is
     itself undoable.
   - It requires the device detached, or explicitly `--offline`, with an
     empty journal. The owner and the lease are required.
6. **Space accounting.**
   - Per snapshot and retained commit: `USED` (freed if deleted),
     `WRITTEN` (changed since the previous one) and `REFER` (total
     referenced, compressed).
   - These are computed by Merkle diff between consecutive retained
     roots, summing leaf `clen`, and cached in `gc/space`.
   - `gc --dry-run` reports what a retention change would reclaim.
7. **Diff and export.**
   - `diff A B` lists changed LBA ranges via Merkle diff.
   - `export --snapshot S` writes a raw image.
   - An incremental export stream format is post-v1.
8. **CSI.**
   - CreateSnapshot maps to a pin; DeleteSnapshot to unpin.
   - In v1, restoring a snapshot into a new PVC is a **full copy**: a new
     independent volume. It uses server-side CopyObject on the same
     provider, otherwise a streamed copy, and is O(size). Thin
     restore/clone is post-v1.

**Decision: clones are post-v1, but the format is ready now.** Clones
follow the Ceph RBD parent/child model, and v1 freezes these format
elements:
- **Lineage:**
  - `volume.lineage: [{volume_uuid, base_seq, backend_hint}]`. It is
    always empty in v1. Index 0 means "this volume"; index *i* means
    lineage entry *i*.
  - Every entry in a leaf's segment dictionary carries `lineage_idx: u8`,
    always 0 in v1. So a hint can point into a parent's segments under
    the parent's prefix without changing the leaf encoding (ADR-8).
- **Holds:** `holds/<child-uuid>` objects in a parent's prefix, each
  naming a base commit. GC treats holds as roots **from v1**. There is
  therefore never a GC version that could delete a future clone's base.
- **Dedup domain (amends ADR-13):** the volume plus its frozen lineage
  bases. A clone dedups only against its own chunks and chunks reachable
  from its bases, which can never be deleted under it, so no
  cross-owner handshake is needed.
- **Ownership:** each volume owns and garbage-collects only the
  segments it wrote, under its own prefix. Cross-owner GC is never
  needed.
- **Reserved for later:**
  - `flatten`: copy the referenced base data into the child, drop the
    hold.
  - "retained-for-clones" parent deletion.
  - The rule that a clone lives on its parent's backends; moving a clone
    elsewhere needs a flatten or a lineage backfill.

**Rejected:**
- *A family-wide shared segment pool with independent owners.* It needs
  multi-writer GC and the condemn handshake for every volume.
- *Thin clones in v1.* Lineage, holds, flatten, delete semantics and
  backend constraints together add roughly 2-3 weeks of work and test
  surface. The format reservation costs one byte per segment-dictionary
  entry and an empty list.
