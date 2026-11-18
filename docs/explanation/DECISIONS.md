# Lunix Decisions (ADRs)

Each record gives the decision, the alternatives rejected, and why. The
context is in [DESIGN.md](DESIGN.md), and the evidence is in the dated
notes under [research/](research/). New decisions, and changes of mind,
are appended here.

**Status: all ADRs (1-28, 30, 31) are accepted** by the maintainer
(2026-11-12). Phase 0 closed the same day; each gated ADR carries a dated
**Gate outcome** block, and the numbers are in
[the baseline](../../bench/BASELINE.md). Acceptance keeps the follow-ups
the gates named:

- **ADR-2:** verified on AWS S3 Standard, OVH and MinIO. Other providers
  must pass `lunix doctor` before they may be an authority.
- **ADR-5:** the default chunk size (16 or 64 KiB) is decided by the
  phase 1 exit benchmark; both sizes are supported.
- **ADR-11:** an external cryptographer review is recommended before
  release.
- **ADR-14:** S3-FIFO is the working default; phase 2 revisits the policy.
- **ADR-28:** its phase 3 gate decides whether journal-batch staging
  (ADR-29) is opened.
- **ADR-29** is a reserved number for that staging ADR, not yet a
  decision.
- **ADR-31:** the v1 rollback anchor is a local floor; the mirror,
  Kubernetes and notary parts follow in phase 5 and later.

**Amended 2026-11-12 (phase 1 review).** The maintainer accepted the
decisions of the review of the phase 1 plan. Each affected ADR below has
an "Amended 2026-11-12 (phase 1 review)" block: ADR-4, 6, 7, 8, 9, 11, 14,
16, 19, 24, 26 and 30. [DESIGN.md](DESIGN.md) follows them.

**Amended 2026-11-12 (M2 security review).** ADR-6, ADR-9 and ADR-11
carry a second block for the maintainer's tamper-resistance decisions (keyed
chunk ids in `integrity` mode, OS randomness, rules R1-R4, the hostile
keyring cost policy), and ADR-31 records the rollback anchor.

**Proposed 2026-11-18:** [ADR-32](#adr-32-write-back-pressure-a-paced-admission-band-below-the-limit-and-a-bounded-full-volume)
(write back-pressure and the full volume) is **proposed, not accepted**. It
amends ADR-14's ladder if accepted.

**Terminology.** Two sequence numbers exist. `write_seq` numbers writes
and cuts (the `seq` of ADR-3, 4, 10, 26, 27 and 28, and `cut_seq`).
`commit_seq` is a commit's position in the chain (the `seq` of
`commits/<seq>`, `heat/<seq>`, snapshot and reader pins, `parent_seq`,
`since_seq`, `base_seq` and `chain_floor`). Older text that says `seq`
means whichever fits its context.

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

**Gate outcome (phase 0, 2026-11-12):** partial — `spikes/s3probe` ran
the authority probes against AWS S3 Standard (three full sweeps, from
the same region and from another continent), OVH Object Storage Milan
(one full sweep) and MinIO RELEASE.2026-11-10 (unversioned and versioned
bucket). All three meet the authority rule: create-only PUT and stale
`If-Match` answer 412, the 8-way create has one winner in 20/20 rounds,
the 8-way swap has one winner in 100/100 rounds with no torn read, and
read-your-write holds by GET and LIST. **AWS S3 Standard and OVH are
confirmed authority backends**, and MinIO passes. Cloudflare R2, GCS,
B2, Scaleway, Wasabi, Hetzner, Tigris and DigitalOcean Spaces are *not
run: no credentials*; their lines under Consequences stay fact-check
claims. Evidence: [s3probe](../../bench/s3probe/RESULTS.md).

- AWS and OVH answer concurrent `If-Match` swaps with 409
  `ConditionalRequestConflict` (310-408 per 100-round race on AWS, 128
  on OVH), and concurrent creates with 412. A client must retry the same
  attempt on 409. Each round still had exactly one winner. MinIO never
  returned 409. No bug against `replicix-objcas`.
- `If-None-Match: *` on `CompleteMultipartUpload` works on both:
  Complete on an absent key 30/30, on an existing key 412 10/10, and an
  8-way race has one winner in 70/70 rounds on AWS and 50/50 on OVH. A
  multipart commit is therefore a valid fencing primitive.
- Unexplained: in two of the three AWS races, one loser was answered
  with a 5xx whose code was not captured. The third run captured codes
  and saw none. The one-winner rule held in those rounds.
- OVH returns a version id on PUT, yet `GetBucketVersioning` shows no
  status, and a read-only listing of the probe prefix found 0 versions
  after cleanup. The versioning rule is met as OVH serves the bucket.
  OVH should be asked what its versioning state is.
- The versioned-bucket rule is unchanged. MinIO passed on a versioned
  bucket; Ceph RGW (tracker 80925) was not available, so the one case
  where the rule matters was not run.
- Open: R2 (a launch provider) and S3 Express One Zone need credentials
  before ADR-2 can be called confirmed for all launch providers.

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

*Amended by ADR-26:* with no local tier, a FLUSH is durable only as an S3 commit.

*Amended by ADR-28:* prefix cuts are kept because every commit is a published image, not because file systems need write order.

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
- *Arbitrary subsets at sync points* (for example, persisting a FUA write
  alone and committing older writes later). Such an image is a legal
  post-power-loss state: ext4, xfs, btrfs and the databases in scope order
  with FLUSH/FUA points and checksums, not with write order
  ([FUA and FLUSH semantics §3](research/2026-11-12-fua-and-flush-semantics.md)). It is rejected
  because every commit is a published image: a restore point, a
  time-travel or rollback target and a mirror's failover state. A cut is
  a state the guest passed through; a non-cut image is one it never saw.
  Cuts also keep the chain single-numbered (`cut_seq`, `chain_floor`,
  `(last_cut, cut]` framing) and avoid exposing guests reliably to
  non-prefix states that real disks produce only rarely. See ADR-28.

**Evidence:** [cache & consistency §1](research/2026-11-11-cache-consistency-s3-protocol.md),
[novel ideas §1.3](research/2026-11-11-novel-ideas.md).

## ADR-4: Journal-first write path; dedup, compression and encryption at destage

*Amended by ADR-26:* with no local tier, the journal is a RAM sink: a memfd held by a keeper, in the same record format.

**Amended 2026-11-12 (phase 1 review):**
- **Lanes.** The journal has one lane per ublk queue. `write_seq` is
  global, and the lanes are merged by `write_seq` at recovery.
- **Group flush.** A FLUSH is a group across lanes: a flush coordinator
  waits until every lane's completed watermark reaches the group's highest
  `write_seq`, then issues one `fdatasync` per lane with unflushed
  appends. At most one group is in flight. A FLUSH covers the writes
  completed before its group formed.
- **Record framing.** A record is an inline 64-byte header followed by the
  data. The data is copied into
  the lane's batch buffer. Records are packed back to back inside a lane
  batch and only the batch is padded to 4 KiB, so n 4 KiB writes cost
  `ceil(n · 4160 / 4096)` blocks (under 1.6 % overhead at n ≥ 32; an
  isolated QD1 write costs 8 KiB). There is no separate header block
  (DESIGN §4.1).
- **Recovery floor.** Recovery takes the `cut_seq` of the newest FULL
  commit as its floor, scans every file of every lane, keeps records above
  the floor, merges them and stops at the first gap. The checkpoint only
  says which files may be skipped. There is no mid-ring corruption error
  and no flush marker; `fsck` warns about valid records beyond the gap.
- **What `on_journal_damage = refuse` refuses: the flushed watermark
  (maintainer, 2026-11-14, after the M12a re-review; the record layout part
  re-signed in `FREEZE.md` on 2026-11-14).** Every journal record carries
  `flushed`, the highest `write_seq` a completed group flush had made durable
  on every lane when the record was appended (on-disk-formats §5.2).
  Recovery reports the highest value any surviving record carries. A FLUSH
  or FUA completes only after the group flush covering it has written and
  `fdatasync`ed every lane's records up to its `upto`, so every record at or
  below the watermark was durable before a later record witnessed it:
  - a gap at or below the watermark is loss of durable data, and `refuse`
    refuses it (the volume stays offline for `fsck`). A damaged range alone
    is not: without such a gap every number above the floor is held by a
    valid record, so the range held only records at or below the cut (which
    the commit covers; a commit does not sync the journal, so a power loss
    can tear them) or no record at all. Wherever a FLUSH- or FUA-covered
    record is lost, the record carrying the watermark lies above it and the
    merge cannot pass the missing number, so the gap rule sees every such
    loss. **Rule change E1, maintainer sign-off 2026-11-16 (rule change, no
    layout change):** the earlier rule also refused a damaged range whose
    next valid record in its lane was at or below the watermark; after one
    crash that tore a record at or below the cut, the first lane open raised
    the watermark past it and every later `refuse` open was refused, the one
    after a clean close too (M12b review);
  - a `write_seq` found twice (a wipe that did not happen) is never residue
    and is refused at any watermark;
  - everything above the watermark is treated as crash residue, whatever its
    shape: a torn last record, a gap across lanes (a record still in its
    lane's open batch at a kill while another lane's later record landed), a
    torn batch followed by a later batch of the same lane (two batches in
    flight). It is logged at warn and wiped, with every record after the
    first gap;
  - a ring file left by a reinitialisation interrupted between its header
    write and its rename (valid header of a higher ordinal, zero body) is
    not damage (on-disk-formats §5.1): a kill during journal reclaim opens.
  This replaces the rule of 2026-11-13 (refuse on any gap or any damage with
  a later record), which refused ordinary crashes of multi-queue volumes.
  **Marks close the unwitnessed window (maintainer, 2026-11-14, review M2;
  the format part, kind 3 and the mark ring, is pending the `FREEZE.md`
  re-sign).** A completed flush is witnessed only by a record appended after
  it completed; without one, every record above the highest surviving
  watermark (after a burst, one FLUSH and no later write, the whole burst)
  would read as residue under medium damage. So the flush coordinator
  writes a witness itself: when no later group flush makes a record
  carrying the watermark durable within the idle delay (default 50 ms), it
  appends a **mark** (record kind 3, 64 bytes, no data) carrying the
  watermark to its own mark ring `m00-<ordinal>.j` and syncs it. A mark
  takes the next `write_seq`, so it leaves no gap; recovery counts it for
  contiguity and the watermark and never hands it to the engine (it changes
  no block); at most one mark per group flush that raised the watermark, and
  a metric counts them. No mark is written for a watermark the last durable
  cut covers (a clean attach takes no number, so an idle volume still cuts
  nothing). With the mark ring full the mark is retried once reclaim frees a
  file. The window that remains is a crash within the idle delay, a full
  mark ring until reclaim frees a file, and `mark_idle = None`: the last
  flush is then unwitnessed, and medium damage to its records reads as
  crash residue and is wiped with every record after it (a crash alone
  cannot damage them). Costs accepted: a failed mark write poisons the
  journal (its number is taken); the mark's write and `fdatasync` run on
  the flush coordinator, so a FLUSH arriving meanwhile waits for one more
  sync; and a damaged durable mark below the watermark is refused under
  `refuse` but under `wipe` takes the acknowledged records after it,
  although the mark held no data (bridging such a gap in the mark ring is a
  possible later change). Inside that window the rule is weaker
  than the rule of 2026-11-13, which refused any records beyond a gap;
  everywhere else it is stronger, since damage to a flushed record with no
  later record in its lane is refused too. A lost or torn mark is residue.
- **Resync after damage (maintainer, 2026-11-12, M1.A0 review).** On a
  header or CRC failure at `p` the reader does not end the lane: it scans
  forward from `p + 8` in 8-byte steps for the next header that passes
  magic, `ver`, `gen`, `off`, CRC and a `write_seq` above the last
  accepted. The lane ends only when none exists. Global `write_seq`
  contiguity still decides what is kept, so damage above the recovery floor
  is a missing `write_seq` and stops recovery exactly as before, and damage
  at or below the floor costs nothing. This is a reader-side rule; the
  record format does not change. Forging an accepted header needs the
  64-bit file `gen`.
- **Checkpoint tails.** The checkpoint's lane tails are used only if its
  `last_commit_seq` is at most the tip's `commit_seq`. `fsck --repair
  --truncate-tip` zeroes the checkpoint.
- Local commits and journal records carry `epoch` = 0 until a backend
  exists.
- "The journal deduplicates by `seq`" (DESIGN §3) is reworded: a reissued
  request is re-admitted with a new `write_seq`, and the reissue is
  idempotent at the block level.

**Gate outcome (phase 0, 2026-11-12):** accepted (gate) — one
group-committed `fdatasync` per sync point is confirmed. Linked
`WRITE→FSYNC` did not meet the rule on ext4, xfs or the real-disk
directory: its latency is 1.4-2.8x higher at sustainable load, and at
saturation it reaches 0.11-0.55x of group commit's records per second.
btrfs is the exception that does not meet the rule: linked chains reach
1.4-4.9x there, but the p99 clause holds in only two of four cells, so
there is no per-file-system default. `O_DSYNC` or `RWF_DSYNC` per write
is rejected (43-1041 rec/s against 2.6-36.5 K for group commit). Limits:
3 s cells, one run per cell, loop-backed images, and the plan's 50 K
rec/s offered load is not sustainable on this disk, so the rule was
evaluated at 2 K and 5 K rec/s. Evidence:
[journal-bench](../../bench/journal-bench/RESULTS.md).

- The journal files are preallocated and zero-filled. A growing file had
  1.07-1.31x the p99 of a preallocated one; the 20 % rule fires on the
  real-disk 4 KiB case.
- On btrfs the journal directory must be `nodatacow` (`chattr +C`):
  group commit reached 7 674 rec/s there against 380 rec/s on default
  btrfs.
- A sync point costs one FUA write plus one FLUSH on every file system
  tested ([ublk-recovery](../../bench/ublk-recovery/RESULTS.md), P0.4).
  Groups of 16 or more records amortise it. A loop-backed file system
  sustained about 550 flushes per second.
- DESIGN §4.1 drops the linked-batch alternative.

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

**Gate outcome (phase 0, 2026-11-12):** run on MSR Cambridge (27 of
36 volumes with at least 1 GiB written; Alibaba and Tencent CBS not
run). The fixed rule does **not** confirm 64 KiB and points to 16 KiB:
S3 bytes per logical GiB written (data plus sealed map, T = 60 s,
compression ratio 0.5) are p50 0.76 / p90 1.05 at 16 KiB against
1.30 / 1.95 at 64 KiB and 3.03 / 5.57 at 256 KiB, and map write
amplification p90 at 64 KiB is 68.8 % against the 25 % limit. The cost
of 16 KiB: 3.9× the map (2,627 against 671 MiB per TiB touched, 48 B
entries; about 0.46× that with ADR-8's encoding), 4× the chunk-index
entries, per-extent crypto overhead about 19 % of a core at 5 GB/s
against 4 %, and sealed map bytes above the uploaded data at T ≤ 60 s
(193 % p90), which would need map-delta records. RMW per 1,000 writes:
71 at 16 KiB, 58 at 64 KiB. The traces are 2007 enterprise servers with
small random writes and no VM-image or database workloads.
**Maintainer decision (2026-11-12): the default is decided after
phase 1**, from phase 1 benchmarks with real file systems, databases and
VM images; both 16 KiB and 64 KiB stay supported. Chunk size is a
per-volume setting in the descriptor, so the default is not part of the
format freeze. Evidence:
[tracesim](../../bench/tracesim/RESULTS.md),
[traces](../../bench/tracesim/TRACES.md).

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

**Amended 2026-11-12 (phase 1 review):** every extent carries a 16-byte
tag (ADR-11), so reads from the local store verify the tag instead of
recomputing the chunk id. On an unencrypted volume the tag is BLAKE3-128
over every byte of the extent before the tag: keyed with a key derived
from the integrity key in `integrity` mode, unkeyed in `plain` mode (a
checksum that detects corruption only). The chunk id stays the identity; a
`verify_reads` option recomputes it on local reads, and an extent fetched
from a backend is still verified by recomputing it.

**Amended 2026-11-12 (M1.A0 review, dedup confirmation):** a dedup hit
taken from a persisted index run is confirmed before the map points at it:
the extent header at the hinted location must carry the same full 32-byte
`chunk_id`, and the extent's tag (AEAD, keyed or, in plain mode, unkeyed)
must verify. Index runs are checksummed, not authenticated, so without this
a forged or wrong entry naming another valid extent would make a new chunk
never be stored. An unconfirmed hit is a miss. A hit from the in-process
table built from verified footers needs no confirmation.

**Amended 2026-11-12 (maintainer decision, M2 security review): keyed ids
in `integrity` mode.** The chunk id is BLAKE3 keyed with an addressing key
in `encrypted` mode (derived from the KMK) and now also in `integrity` mode
(derived from the integrity key, by its own purpose id, distinct from the
extent-tag and HMAC keys). Only `plain` mode stays unkeyed. Reason: with
unkeyed ids and `id_len = 16`, a guest that finds two chunks with equal
16-byte prefixes (a 2^64 birthday search over its own data) plus a bucket
writer could swap them on a local read, because local reads do not
recompute the id. Without the key no one can compute an id, so that search
has no oracle. Cost: none per I/O (keyed BLAKE3 runs at the unkeyed
speed). Consequences: dedup stays within the family and now also requires
the key; the map's leaf `id_prefix` is the prefix of the keyed id; M2
implements it, and map node hashes are unchanged (unkeyed in `integrity`
mode, authenticated by the HMAC'd commit). The format freeze carries one
new purpose id (see ADR-11). Rules R1 and R2 of ADR-11 still apply: the id
check is what binds an extent to its place.

**Decision:**
- `chunk_id = blake3(plaintext)`. When the volume family is `encrypted` or
  `integrity` it is `blake3::keyed_hash(addressing_key, plaintext)`; this
  is Constellation's ADR-10 scheme.
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

**Amended 2026-11-12 (phase 1 review):** the framed header's tag is in
every extent. It is the AEAD tag on an encrypted volume and a BLAKE3-128
tag on an unencrypted one (ADR-11). There is one extent layout for both.

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

**Amended 2026-11-12 (phase 1 review):** the two questions the gate left to
phase 1 are decided.
- **Fanout** is a volume-descriptor field, `map_fanout`: 256 or 512, with
  512 the default. A leaf holds `map_fanout` entries. The choice is fixed
  at creation and needs no migration.
- **Interior nodes** are stored raw, through the extent codec (codec 0). No
  zstd or LZ4 is used on nodes, since hashes do not compress. The node
  header has no codec byte; it carries `level` and `fanout`. Leaves and
  interior nodes both go through the extent codec, so the codec field of
  the node's `CCH1` header is the only codec indication.

**Gate outcome (phase 0, 2026-11-12):** amended (gate) — rules 1-4 pass
(rule 5 is a report) at 10 TiB, fanout 512, 64 KiB chunks, one core. Seal of 1 000
uniformly scattered dirty leaves takes 59.7 ms and writes 23.1 MiB raw
(20.4 MiB stored); 10 000 leaves take 275 ms. A diff of 1 000 differing
leaves takes 1.04 ms and reads 998 nodes (bound 8 000). Cold open plus
the first lookup is 4 reads, equal to the depth. A 256 MiB leaf cache
gives a 294 MiB peak RSS over 1 M random lookups (limit 320 MiB). The
full table would be 7.5 GiB flat; the encoded leaves are 3.5 GiB (358
MiB/TiB). The flat alternative writes 43× more bytes per seal with 1 MiB
blocks and 3× more with 64 KiB blocks, and cannot localise a diff
without reading the changed blocks. Evidence:
[radixmap](../../bench/radixmap/RESULTS.md).

- An interior node's hash covers its child hashes (and level), never the
  child locations. Locations are hints, not content. Otherwise roots
  would differ per backend and "identical content, identical roots"
  would be false. Verified: 1 TiB built sequentially, in random order
  and from the model gives one root.
- The mutable overlay has a cap that forces a seal. A decoded dirty leaf
  costs 24 KiB against 11.4 KiB sealed; 100 000 dirty leaves peak at
  2.65 GiB RSS, and all leaves of a 10 TiB volume would be 7.5 GiB. The
  cap and the `B_max` throttle are stated in dirty leaves.
- Scattered writes cost 0.18-0.55 of the dirty logical bytes in map
  bytes, worst when each dirty leaf has one dirty chunk. Map-delta
  records in DELTA commits remain the mitigation if phase 1 finds that
  too high. Sync mode pays about 70-90 KiB of map per fsync.
- Depth at 10 TiB is 4 (3 up to 8 TiB at 64 KiB), as DESIGN §7 states.
- Not adopted by the gate, left to phase 1: leaving interior nodes
  uncompressed or LZ4 (zstd-1 costs about 40 µs per node and hashes do
  not compress), and fanout 256 (36-45 % fewer bytes per seal at twice
  the resident interior nodes, 30 MiB at 10 TiB). Gates 2-4 were not run
  at fanouts 256 and 1024.

**Decision:** the LBA → chunk map is a fixed-fanout radix tree.

- **Leaf entries:**
  - Each leaf holds `map_fanout` entries (512 by default, covering 32 MiB
    of LBA at 64 KiB chunks).
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

**Amended 2026-11-12 (phase 1 review):**
- **Numbering.** A commit's `commit_seq` is its position in the chain,
  `parent_seq + 1`, and is distinct from the write `seq`. The genesis
  commit is `commit_seq` 0 with no parent. Its `parent_hash` is the BLAKE3
  of the `volume` object's bytes as stored. In phase 1 `chain_floor` is the
  `commit_seq` of the older of the last `N` FULL commits (default 2); commits
  below it are deleted after the GC grace.
- **Index runs.** "Every object a commit names is durable before the commit
  is created" holds for `index_packs[]` because a FULL names only the runs
  that were durable before it. The run for a FULL's own segments is named
  by the next FULL, so index runs may lag one commit.
- **Orphans.** At open, before the index catches up, every sealed segment
  and map pack with an id above the highest the chain names, and that no
  commit names, is deleted. Otherwise a dedup hit could point the map at a
  segment that no commit lists.
- **EPOCH** has the optional field `authority` (the backend that becomes
  the authority; ADR-24).
- **Dedup confirmation (M1.A0 review).** Index runs are not authenticated,
  so a dedup hit from a run is confirmed against the extent header's full
  `chunk_id` and its tag or AEAD before the map points at it (ADR-6). An
  unconfirmed hit is a miss.
- **`created_at` is a writer clamp,** `created_at = max(now,
  parent.created_at)`, not a chain validity rule. Mount and `fsck` never
  reject on it (`fsck` warns). `epoch`, `commit_seq` and `chain_floor`
  remain reader rules.
- **Rollback floor (ADR-31).** Attach compares the verified tip with the
  host's floor `{max_commit_seq, tip_hash}` and refuses a lower tip
  (`Rollback`) or a different hash at the same height (`Fork`). The lease
  is opened only through `open_lease_fresh(bytes, max(chain epoch,
  pins.max_epoch))`.
- **Object ids** (segments, map packs, index runs) are never reused. After
  the open-time orphan sweep the next id is 1 + the maximum of the
  checkpoint's `id_high`, the highest id the chain names and the highest id
  present before the sweep, so `fsck --repair --truncate-tip` cannot make
  the allocator reissue a swept id.

**Decision:**
- **The commit chain:**
  - The volume's history is a chain of create-only objects
    `commits/<seq:016x>`, written with `If-None-Match: *`. This is
    Constellation's `commits.rs` pattern.
  - Commit kinds:
    - **FULL**: map root, size, `parent_seq`, `parent_hash`, `epoch`,
      `cut_seq`, the segments it introduced, optional heat-map reference.
    - **DELTA**: journal records inline, compressed and encrypted, on top
      of the last FULL.
    - **EPOCH**: owner change.
    - **CONFIG** and **TRANSFER**: backend membership and authority
      moves (ADR-24).
    - *Amended by ADR-11 (F5):* every kind carries `parent_hash` (the
      BLAKE3 of the parent object's bytes) and `epoch`. Mount verifies the
      links from the newest FULL and rejects a decreasing `epoch`, so valid
      commits of two histories cannot be spliced into one chain.
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

*Amended by ADR-28:* a FUA write is a cut at its own `seq`, so an ext4/xfs fsync is two commits (FLUSH, then FUA). Flush uploads have their own lane, and unflushed bytes in sync mode are capped by `sync_dirty_max`.

*Amended by ADR-27:* the modes are presets of two settings, `write_ack` and `flush` (each `local` or `s3`). A new preset, `write-through`, makes every acknowledged write durable on S3.

*Amended by ADR-26:* S3-only volumes run in sync mode, unless `--unsafe-flush` is given.

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

## ADR-11: Encryption: AES-256-GCM with per-extent derived keys; Constellation's key hierarchy

**Status:** accepted by the maintainer for the format (2026-11-12),
amendments included. An external cryptographer review of the 15 questions
in the [review note](research/2026-11-11-adr-11-crypto-review.md) is still
recommended before release. It is a note, not a blocker for phase 1.

**Amended 2026-11-12 (phase 1 review):**
- **Tag in every extent.** The 16-byte tag is present in every extent. On
  an encrypted volume it is the AEAD tag. On an unencrypted volume it is
  BLAKE3-128 over the extent header and payload, keyed with the integrity
  key from the keyring if one exists, else unkeyed. Local reads verify the
  tag instead of recomputing the chunk id (`verify_reads` recomputes it);
  fetched extents are still verified in the order of "Integrity" below.
  This closes the gap where a flipped payload byte in an unencrypted raw
  extent was returned as data.
- **Widths.** The stored extent header keeps `CCH1`'s widths (2-byte codec,
  8-byte `ulen`). The AAD and the key-derivation `info` are a canonical
  re-encoding, not the header bytes: they use the widths of the normative
  KAT vectors, a 1-byte codec, a 1-byte level and 4-byte `ulen` and `clen`.
  Both are safe, because a chunk is at most 1 MiB and a codec id is at most
  255.
- **AAD object name.** In the metadata AAD, "object name" is the
  bucket-relative object path (for example `commits/<commit_seq:016x>`),
  for every kind. A local-only commit is sealed under that name, so its
  bytes upload verbatim.
- `heat/`, `gc/` and `dicts/` bodies freeze only the envelope
  `{kind id, ver, bytes}` in phase 1.

**Amended 2026-11-12 (maintainer decision, after the M1.A0 review): three
volume modes.** The descriptor's crypto mode is one of:
- **`encrypted`:** AEAD on extents and metadata.
- **`integrity`:** an HMAC on every commit and on the lease, keyed extent
  tags, and an integrity key in the keyring. This is F5/F7 as accepted
  below, and it **remains the default for unencrypted volumes**.
- **`plain`:** mode 0. Unkeyed BLAKE3 checksums and tags, no key material.
  It detects corruption only and gives no protection against anyone who
  can write the bucket: commits, footers and tags can be recomputed, and a
  local read does not recompute the chunk id, so a swapped extent with the
  right header id returns wrong data. The mode is fixed in the create-only
  descriptor, so a plain volume can never be made safe later. It is an
  explicit opt-out, supported everywhere including volumes with S3
  backends. The CLI and docs warn about these limits when plain is chosen
  and when a backend is added to a plain volume.
- **Default and CLI.** `lunix volume create` without `--encrypt` or
  `--plain` makes an `integrity` volume (flags `--encrypt`, `--integrity`,
  `--plain`). For a local volume it auto-creates a key-file slot (kind 2)
  whose random key lives next to the volume (`<dir>/<uuid>/integrity.key`,
  0600), so attach needs no extra step; `--passphrase` adds a passphrase
  slot for volumes that move. Kubernetes supplies the integrity credential
  from a Secret, like an encryption key. The per-I/O cost is nil: keyed
  BLAKE3 runs at the unkeyed speed and the HMAC touches metadata only.
- **Attach rules (F7 extended).** A `plain` volume needs no credential. A
  credential supplied for a plain volume is refused as `Downgrade`, like a
  pin that records a stronger mode than the descriptor has. The descriptor
  of a plain volume carries an unkeyed BLAKE3 checksum in place of the MAC.
- Index runs stay checksummed and unauthenticated in every mode, made safe
  by the dedup confirmation of ADR-6.

**Amended 2026-11-12 (M2 security review and maintainer decisions)**
([review](../plans/v1/wip/01-p1-m2-review.md)):
- **F3, randomness wording.** Salts and nonces come from the operating
  system through `getrandom(2)` on every call, never from a userspace DRBG.
  This covers the extent salt, the metadata and slot nonces, the Argon2id
  salt and generated root keys. Reason: AWS-LC's `RAND_bytes` (behind
  `aws_lc_rs::rand::fill`) is a per-thread CTR-DRBG reseeded from the OS
  only every 4096 generate calls, and it detects a VM snapshot restore only
  through SysGenID, which mainline Linux lacks, so a restored snapshot would
  replay up to 4096 salts per thread (the F1b case). The kernel CRNG is
  reseeded on VM fork by vmgenid since Linux 5.18, below the ublk floor.
  Mitigation A (`chunk_id`, codec, level and `clen` in `info`) bounds the
  impact, but the rule stands. "The salt never comes from a buffered RNG
  pool" above reads "never from a userspace DRBG".
- **Keyed chunk ids in `integrity` mode (maintainer).** See ADR-6. The
  addressing key is derived from the integrity key with a purpose id of its
  own; `plain` stays unkeyed. The keyring and the descriptor are
  unchanged. It adds one purpose to the key-purpose table, which is a
  frozen format item (re-sign, below).
- **Rules R1-R4 are normative** (DESIGN §6, §7, §8.4, §10.1, §15). The
  keyed tag of an `integrity` volume covers extent bytes only, so kind and
  location are unbound and an `open` with no id expectation accepts any
  valid extent of the family. No format change is needed, provided:
  - **R1** every data read passes the leaf `id_prefix` (or the full id) in
    `OpenExpect`, with the chunk size, and fails on a mismatch; a read
    without an expectation is a scan and sets `verify_id`.
  - **R2** the map recomputes each node's hash against the parent's
    `child_hash` (the commit's root for the root) and the header
    `chunk_id`, and requires codec 0.
  - **R3** footers are used only after `MetaCodec::open` succeeds, and
    `seg_hash` is checked whenever the region is read whole.
  - **R4** attach refuses a tip below the local floor (ADR-31) and opens
    the lease through `open_lease_fresh`.
  With keyed data ids and unkeyed node hashes, a node can never pass as data
  or the reverse in `integrity` mode without a preimage.
- **Rollback anchor.** Decided in ADR-31. The rollback paragraph under
  "Integrity" below is closed by it.
- **Hostile keyring cost policy.** The read bounds (1 GiB, t 10, p 16, 32
  slots) are the format's. The daemon adds a policy ceiling: a maximum
  Argon2id memory per slot (default 256 MiB) and a total budget over the
  slots tried, tries the slot that unlocked the last attach first, and wipes
  the Argon2id blocks after use. A slot over the ceiling is skipped and
  reported.
- **Not done: a per-kind tag key.** Binding `kind` into the tag (a
  purpose-table change) would add defence in depth for the price of
  re-signing the freeze; the reviewer did not recommend it for v1.
- **Freeze note.** `state/pins` version 2 (ADR-31) and the new
  addressing-key purpose id are amendments to frozen items. Each needs the
  maintainer's re-sign of `FREEZE.md` (items 11 and 14) and a golden update
  before the phase 1 format freeze is final.

**Gate outcome (phase 0, 2026-11-12):** the crypto review (P0.8) is done
and found the salted per-extent key, constant nonce and AES-256-GCM
construction sound: the amended nonce rule held in every modelled case
(10 000 mixed re-encodes, no repeat), 40 tests pass and 27 independent
cross-checks agree. The model reproduces the first draft's hole
(recompress leaks the XOR of plaintexts). Six findings (F2, F5, F6, F7,
F9, F10) changed the frozen format; the maintainer accepted the
amendments on 2026-11-12. Cost on one Zen 5 core: the per-extent key
schedule adds about 0.35 µs (0.55-0.65 µs with a fresh salt and the
accepted derivation); a full seal of 64 KiB takes 3.77 µs as first written
(17.4 GB/s) and 3.98 µs as accepted (16.5 GB/s), and of 4 KiB 0.65 µs
against 0.92 µs. Argon2id at 64 MiB, t = 3 costs 75-83 ms per wrap. Not
run: the FIPS-validated build, a real KMS, parser fuzzing, AArch64.
Evidence: [crypto-kat](../../bench/crypto-kat/RESULTS.md),
[review note](research/2026-11-11-adr-11-crypto-review.md).

- Amendments F2, F5, F6, F7, F9, F10 (and F1b, F3, F8, F11, F12) accepted
  by the maintainer, 2026-11-12:
  - F2: authenticated `dek_gen` in the extent header and the AAD.
  - F5: `parent_hash` and `epoch` in every commit kind; an HMAC on every
    commit of an `integrity` volume (the default for unencrypted volumes;
    `plain` is an explicit opt-out, see the three-modes amendment); lease
    epoch floor.
  - F6: one mutable, CAS-written `keyring`; bounded Argon2id; honest slot
    semantics.
  - F7: key material at attach means "must be encrypted", checked by the
    descriptor MAC; the orchestrator supplies the KMK fingerprint. A plain
    volume needs no credential.
  - F9: every metadata object sealed, by the object-kind table.
  - F10: a reserved `nonce[12]` in the extent header for the FIPS profile.
  - F1b, F3, F8, F11, F12: the keyed extent derivation over `info`, a
    salt that is never carried over, a (key, nonce) tripwire, plaintext
    journal and overlay documented, the lease epoch floor.
- The vectors in `bench/crypto-kat/raw/kat-vectors.json` are normative for
  phase 1 tests. They use the `lunix extent v2\0` info string below. Their
  field named `dek_epoch` is the same value as `dek_gen`.
- DESIGN §5, §6, §6.1, §8, §15 and §22.1 follow this record.

**Decision:** encryption is chosen at volume-family creation.

- **Key hierarchy (taken from Constellation's `e2e.rs`):**
  - A master key (KMK) is wrapped by one or more key slots in the family
    keyring: Argon2id passphrase, KMS key or raw key file. Several slots
    let a volume switch KMS when it changes cloud.
  - Every other key is derived from the KMK by purpose, with keyed BLAKE3
    (HKDF-SHA-256 in the FIPS profile): `keyed_hash(KMK, "lunix kdf v1\0" ‖
    family_id ‖ purpose ‖ args)`. The purposes are the addressing key (one
    for data, one for map nodes), one DEK per `dek_gen`, one metadata key
    per object kind, the volume-descriptor MAC key, the keyring MAC key
    and the KMK fingerprint.
  - **`dek_gen`** is the DEK generation, a `u32` starting at 0. It is
    named `dek_gen` so it is not confused with the lease and commit
    `epoch`. Rotation raises it, new extents use the newest generation,
    and old extents keep theirs until compaction rewrites them. A reader
    derives any generation's DEK from the KMK.
- **Extent format:**
  - `header {magic, ver, codec, level, ulen} + chunk_id + clen +
    [dek_gen(4) + salt(24) + nonce(12)] + payload + tag(16)`. The bracketed
    fields exist when the volume is encrypted.
  - "Encrypted" is read from the authenticated volume descriptor, never
    from an extent. `family_id`, `id_len` and `kind` (data extent or map
    node) also come from the descriptor and the container (`segments/` or
    `map/`), not from the extent.
  - `nonce` is zero in the default profile. In the FIPS profile it holds
    the module-generated nonce.
- **Data encryption:**
  - The extent key is `keyed_hash(DEK_gen, info)` with `info = "lunix
    extent v2\0" ‖ family_id ‖ kind ‖ dek_gen ‖ codec ‖ level ‖ clen ‖
    id_len ‖ chunk_id ‖ salt`. The FIPS profile uses HKDF-SHA-256 Expand
    over the same `info`.
  - The `salt` is a fresh random 192-bit value drawn **inside the seal
    function on every call**, from `getrandom(2)`. No retry, re-encode or
    compaction path may carry one over: the extent builder has no salt
    setter, re-encoding always re-seals, and the salt never comes from a
    userspace DRBG (amendment of 2026-11-12, M2 review).
  - The cipher is AES-256-GCM with a fixed all-zero nonce, because every
    key encrypts exactly one payload. It runs through `aws-lc-rs`; ring
    measured 11 GB/s per core on the first host. The FIPS profile cannot
    seal with a caller-chosen nonce in aws-lc-rs, so it stores the module's
    random nonce in the `nonce` field.
  - The AAD is `ver ‖ codec ‖ level ‖ ulen ‖ kind ‖ dek_gen ‖ family_id ‖
    id_len ‖ chunk_id`. It holds no volume, segment or offset, so re-packing,
    mirroring and dedup can copy extents verbatim.
  - Debug and fuzz builds run a (key, nonce) tripwire that fails on any
    repeat.
- **Sealed objects (every metadata object's contents):**

  | object | sealing |
  |---|---|
  | `volume` | plaintext fields plus a descriptor MAC (see Downgrade) |
  | `families/<id>/keyring` | slots wrapped, body MAC'd |
  | `lease` | XChaCha20-Poly1305, `lease` metadata key |
  | `commits/*`, including DELTA bodies | XChaCha20-Poly1305, `commit` and `delta` metadata keys |
  | `map/*` nodes | the extent scheme, `kind` = node |
  | segment footers | XChaCha20-Poly1305, `footer` metadata key |
  | `heat/*`, `gc/*`, `readers/*`, `holds/*`, `snapshots/*` bodies | XChaCha20-Poly1305, one metadata key per kind |
  | `dicts/*` (ADR-30) | XChaCha20-Poly1305, `dict` metadata key |
  | `head`, `sync/*/progress` | not sealed: advisory, never trusted, only counters |

  - Metadata uses a random 192-bit nonce. The AAD is `"lunix meta v1\0" ‖
    volume ‖ kind ‖ object name` (the family for the keyring). It holds no
    bucket or backend id, because mirrors hold byte-identical objects. A
    commit moved to another `seq` or volume fails.
  - Object names stay plaintext, so a bucket reader still sees counts,
    sizes, times and access patterns.
  - The FIPS profile needs an AES-GCM form of this sealing, with a key
    derived from the metadata key and a random per-object salt. It is not
    built.
- **Keyring (F6):**
  - There is one mutable object per family, `families/<family-id>/keyring`.
    The `volume` object holds no slots.
  - It is written by CAS (`If-Match`) on the authority only and copied
    byte-identically to the mirrors. Its body holds `keyring_seq`, the
    slots and the family id, and is MAC'd with the keyring MAC key.
    `keyring_seq` rises on every change, and a keyring below the highest
    `keyring_seq` seen is rejected, so a removed slot cannot come back
    silently.
  - A slot is the KMK wrapped with XChaCha20-Poly1305 under a key-encryption
    key. The AAD is `"lunix slot v1\0" ‖ family ‖ slot_id ‖ kind ‖ KDF
    params ‖ salt`. A KMS slot is bound by the KMS encryption context
    `{family, slot, key_ref}`. Azure Key Vault wrap takes no AAD, so its
    slot needs a MAC over the slot instead.
  - Argon2id parameters are stored per slot and unwrap uses the stored
    ones. They are bounded when read (at most 1 GiB, t ≤ 10, p ≤ 16) and
    floored when written (at least 19 MiB, t ≥ 2).
  - Removing a slot or changing a passphrase does **not** revoke a KMK the
    holder has already seen. Revocation is migration to a new KMK. DEK
    rotation protects only new writes, and a dedup hit keeps pointing at an
    old extent until compaction rewrites it. A mounted volume holds its KMK
    in memory, so slot changes take effect at the next attach.
  - An `integrity` family has a keyring too. Its slots wrap an integrity
    key instead of encryption keys (see Integrity). A `plain` family has no
    key material, so it has no slots.
- **Downgrade (F7):**
  - The `volume` descriptor carries a MAC keyed by a KMK-derived key. It
    covers every plaintext field, with the label `"lunix volume v1\0"`.
  - The attach request, not the bucket, decides whether the volume must be
    encrypted. If the caller supplies key material (a slot credential, a
    KMS reference or a key file), the volume must be encrypted and its
    descriptor MAC must verify, else attach is refused as a downgrade
    (`Downgrade`). A caller that supplies the integrity credential of an
    unencrypted volume declares it integrity-only, and the descriptor MAC
    must verify under the integrity key. A `plain` volume needs no
    credential; a credential supplied for one is `Downgrade` (the caller
    expects protection the volume lacks).
  - The orchestrator (StorageClass, `LunixBackendSet` or the Secret)
    supplies the KMK fingerprint. A different volume with its own KMK is
    refused as `WrongKmk`. The local trust-on-first-use pin stays for the
    interactive CLI.
  - Without a credential, a fingerprint or a pin, a volume recreated as
    plaintext cannot be told from a real one. That follows from the
    ephemeral-client model.
- **Commit chain (F5):**
  - Every commit kind (FULL, DELTA, EPOCH, CONFIG, TRANSFER) carries
    `parent_hash`, the BLAKE3 of the parent object's bytes as stored, and
    `epoch`.
  - Mount verifies the links from the newest FULL to the tip and rejects a
    decreasing `epoch`. Two valid branches, such as the abandoned branch of
    a DR steal (ADR-24), can no longer be spliced into one chain.
  - The `lease` carries its `epoch` in the plaintext of its sealed body. A
    lease whose epoch is below the newest verified epoch is rejected,
    because an old sealed version of the same object would otherwise pass
    the AEAD.
- **Consequences:**
  - There is no nonce-space limit. A (key, nonce) pair never repeats, even
    when the same chunk is recompressed (ADR-12) or re-encoded by a newer
    zstd.
  - Re-packing and mirroring copy extent bytes verbatim, so they never
    re-encrypt.
  - Commits, the lease and the other metadata use random-nonce
    XChaCha20-Poly1305 (RustCrypto `chacha20poly1305`), a second AEAD
    implementation to audit.
  - A bucket reader sees chunk-id equality within a family, `clen`, object
    names and timing. An opt-in `pad` option buckets `clen`; it is off by
    default and its cost on real data is unmeasured.
  - The local journal, the dirty overlay and the keeper-held memfd hold
    plaintext. ADR-11 protects the bucket and the extents cached from it.
    dm-crypt or LUKS on the local directory is recommended.
  - Long-lived keys live in `Zeroizing` memory in the locked key arena
    (DESIGN §3). Key material in use is NOT all locked today (M12a final
    review F1, 2026-11-15): the passphrase and its request copies during
    attach and create, the unwrapped root key and KEK while a slot is
    opened, key-file bytes, Argon2 working memory, per-extent AEAD keys and
    cipher schedules on destage and read-helper stacks, and keyed hash
    states can sit on pageable memory. Until that is closed (locked stacks
    for every key-handling thread, locked buffers for the passphrase and
    slot plaintext, `blake3` zeroize, fail-closed arena growth), run the
    daemon with no swap or with encrypted swap. Core dumps are off (`PR_SET_DUMPABLE 0`, `RLIMIT_CORE 0`,
    `MADV_DONTDUMP`).
  - A Lunix KMK must never be shared with Constellation. The labels differ,
    so an accidental share still yields unrelated keys.
- **Amended 2026-11-11, before acceptance:** the first draft derived the
  key from `chunk_id` alone. That produced deterministic ciphertext and a
  GCM nonce-reuse hole whenever the same plaintext was compressed to
  different bytes. The random salt removes the hole.
- **Amended 2026-11-12, at acceptance:** the extent key moved from
  `blake3::derive_key("lunix extent v1", DEK_epoch ‖ chunk_id ‖ salt)` to
  the keyed form above, which binds family, kind, codec, level and `clen`
  and has a PRF slot for the secret. The header gained `dek_gen` and
  `nonce`, the commits gained `parent_hash`, and the keyring became one
  mutable object.

**Integrity:**
- Every fetched extent is checked in this order: header sanity (version,
  known codec, `chunk_id` matches the map), AEAD tag, `ulen` against the
  volume's chunk size (never the header), bounded decompression, then the
  chunk id is recomputed. The recompute is also the key commitment that
  GCM lacks.
- On a tag failure the daemon returns no data, counts it
  (`extent_auth_failures_total{backend}`), tries another InSync backend
  and never caches the bad extent. A tag-valid extent with the wrong id is
  a writer bug or a key compromise and raises an alarm.
- Encrypted volumes: the AEAD authenticates every commit body, and
  `parent_hash` links them.
- `integrity` volumes (plain volumes carry an unkeyed checksum instead and
  get none of this): every commit (FULL, DELTA, EPOCH, CONFIG and
  TRANSFER) carries an HMAC over its body, including `epoch` and
  `parent_hash`, under an integrity key held in the family keyring. A
  DELTA's HMAC covers its inline records, and a FULL's covers the map root.
  Since the map is a Merkle tree over chunk ids, this authenticates the
  volume state end to end. The lease carries the same HMAC. The integrity
  key is chosen over "corruption detection only" (maintainer,
  2026-11-12), and every commit is covered so that a bucket writer cannot
  forge a DELTA or CONFIG between two FULLs.
- Rollback to an older valid prefix of the chain needs an anchor outside
  the bucket: the local floor of ADR-31 (v1), the mirror maximum and a
  Kubernetes floor (phase 5). Without a floor it cannot be detected.

**Rejected:**
- *Per-chunk random-nonce XChaCha20-Poly1305.* That is Constellation's
  choice. It runs at about 2-3 GB/s, and re-packed or mirrored
  ciphertext would differ.
- *Unkeyed convergent encryption.* The provider could confirm known
  files.
- *AES-GCM-SIV.* About 5× slower in the measured crates.
- *Slots in the create-only `volume` object.* Slots must change after
  creation.
- *Corruption detection only as the default for unencrypted volumes.* The
  maintainer chose the HMAC as the default (2026-11-12). Corruption
  detection survives as the explicit `plain` mode (2026-11-12, after the
  M1.A0 review recommended against offering it in v1).

**Gate:** the crypto design review ran in phase 0 and is closed. The
external review is recommended before release. A FIPS profile uses
HKDF-SHA-256 and HMAC-SHA-256.

## ADR-12: Compression `auto`: an LZ4 probe, then zstd-1; recompress on seal

*Amended by ADR-30:* zstd dictionaries come through an explicit `lunix compress train|apply` command that measures the volume's own data. Codec id 3, `dicts/<id>` and the GC rule are reserved in the phase 1 format.

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

*Amended by ADR-26:* the local store is optional. Its role is `store`, `cache` or `none`.

**Amended 2026-11-12 (phase 1 review):** the "compact" rung is real in
phase 1 for local-only volumes. GC and compaction ship in phase 1 (DESIGN
§11), because a local-only volume evicts nothing and its store would
otherwise grow without bound under churn.

**Gate outcome (phase 0, 2026-11-12):** run on MSR Cambridge (27
volumes, 64 KiB chunks). The fixed rule does **not** accept S3-FIFO, and
no other policy meets the amendment margin. Median request miss ratios
at 1 / 5 / 10 % of the working set: LRU 0.395 / 0.315 / 0.271, S3-FIFO
0.382 / 0.297 / 0.261, SIEVE 0.396 / 0.294 / 0.255, W-TinyLFU 0.397 /
0.309 / 0.269. S3-FIFO is within 5 % of the best and below LRU at every
size, but it loses to LRU on 33 % of volumes at 1 % (limit under 25 %)
and fails the scan test on one volume (mds_1, 5.1-5.3 points worse than
LRU at 0.5-1 %; the mean over the 9 scan volumes is better than LRU).
SIEVE is not 5 % better than S3-FIFO at two sizes; W-TinyLFU is worse.
The policies differ by at most about 4 % in miss ratio, and the policy
is implementation only (nothing on disk or in S3), so it can change in
any release. S3-FIFO stays the working default; phase 2 revisits it
with segment-granular eviction and Lunix's own traces. Evidence:
[tracesim](../../bench/tracesim/RESULTS.md).

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
  - Mirrors receive byte-identical segments, map packs and commits, and
    the family keyring (ADR-11), which the authority writes by CAS.
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

*Amended by ADR-26:* S3-only volumes add a keeper (the systemd FD store or `lunix-keeper`) that holds the RAM write buffer across daemon restarts.

**Amended 2026-11-12 (phase 1 review):** the control protocol is
length-prefixed JSON frames over the Unix socket, with a protocol version
(DESIGN §16). gRPC is rejected: it adds a build dependency with no phase 1
benefit. The protocol is not an on-disk format and can change later.

**Gate outcome (phase 0, 2026-11-12):** amended (gate) — user recovery
with `REISSUE` keeps a mounted file system consistent. kill -9 with a
restart: 36 cycles on ext4 and 10 on xfs, zero fio errors, zero verify
failures, fsck clean after every phase. Kill to serving again takes 0.26
s with an immediate restart (heartbeat stall p50 221 ms, max 238 ms on
ext4) and 1.16 s with a 1 s restart delay; no stall exceeded the restart
delay by more than 0.2 s. Both negative controls (no `REISSUE`;
acknowledged-but-dropped writes) fail as expected, so the clean runs
mean something. A planned QUIESCE handover passed 20 of 20 cycles on
ext4 with the corrected daemon (median stall 180 ms, one 2.0 s outlier
while in-flight I/O drained). Evidence:
[ublk-recovery](../../bench/ublk-recovery/RESULTS.md).

- `lunix upgrade`: `QUIESCE_DEV` returns once every queue has an idle
  command; requests the server already holds stay with it. The daemon
  completes and commits those with commands of its own, waits until its
  queue threads have ended with nothing in flight, runs the engine's
  final commit, then exits without `STOP_DEV`. libublk's `run_target()` sends `STOP_DEV` when the queue
  threads end; that deadlocked the kernel thread in
  `ublk_wait_tagset_rqs_idle` while the mounted file system, fio and
  jbd2 sat in D state, and only a reboot cleared it. `run_target()`
  cannot be used on this path.
- A daemon never stops a device, and never exits, while started requests
  are not being completed. This wedged a device twice. The one sanctioned
  exception is `abort()` after a fatal internal error: it is equivalent
  to `kill -9`, which `USER_RECOVERY_REISSUE` recovers.
- `DEL_DEV` of a LIVE device is a `STOP_DEV`. It is sent only after a
  successful `TRY_STOP_DEV`, or by another process for a device without a
  server; never by the serving process as a fallback.
- `detach` uses `TRY_STOP_DEV`: it fails with EBUSY while the device is
  mounted and succeeds after `umount`. Plain `STOP_DEV` yanks the device
  from a mounted file system, which then returns EIO.
- `DEL_DEV` with a mounted file system blocks until the file system is
  unmounted, because the kernel waits for the last reference to the
  block device.
- The driver retains `SET_PARAMS`, `dev_info` and queue affinity across
  recovery. Recovery sends no `SET_PARAMS`.
- A device created with `USER_RECOVERY_FAIL_IO` cannot be recovered
  (requests fail with EIO about 150 ms after the kill). It is a give-up
  policy: delete the device and remount. `FAIL_IO` and `REISSUE` cannot
  be combined (`ADD_DEV` returns EINVAL).
- Not tried: flipping a live device to fail-I/O from the control plane
  (the `on_hang` watchdog).

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

**Gate outcome (phase 0, 2026-11-12):** amended (gate) — the kill -9
recovery rule is met (see ADR-16), all mandatory flags are present on
the 7.0 kernel (the `GET_FEATURES` mask is `0x7ffff`, all 19 flags), and
online grow works (ADR-20). Both performance rules fail, so the copy
path stays the default. Evidence:
[ublk-datapath](../../bench/ublk-datapath/RESULTS.md),
[ublk-recovery](../../bench/ublk-recovery/RESULTS.md).

- **USER_COPY: rule not met; copy stays the default and USER_COPY is
  opt-in.** It wins at 64 KiB and above: 64k writes give +11 % to +43 %
  IOPS and 10-30 % less CPU per I/O, 64k random read at QD128 gives
  2.55×, and the resident set falls from 148 MB to 26 MB. It loses at 4
  KiB: writes lose 8-12 % IOPS, and several QD1 cells lose 5-10 %,
  because each request pays an extra `pread` or `pwrite` from the queue
  thread. Untested for phase 1: issuing that copy through io_uring
  alongside the backend SQEs, which may remove the 4 KiB loss.
- **BATCH_IO: rule not met; it is used only when asked for.** With
  libublk 0.4.8's batch transport, 64k writes give 0.73× (QD32) and
  0.97× (QD128) of copy's IOPS at 1.03-1.37× the CPU per I/O, and 64k
  random read at QD32 gives 0.50×. 4 KiB cells are within -10 % to +9 %.
  **BATCH_IO is not used with QUIESCE:** a QUIESCE handover broke the
  file system on its second cycle (kill -9 passed 20 of 20). The result
  describes libublk's implementation (queue-thread-only commit, 64-tag
  fetch buffers), not the kernel interface. libublk also refuses
  BATCH_IO with USER_COPY, though the kernel accepts the combination.
- Flag rules found by probing `ADD_DEV`: `QUIESCE` needs
  `USER_RECOVERY`; `REISSUE` and `FAIL_IO` exclude each other;
  `INTEGRITY` needs `USER_COPY`. The driver sets `COMP_IN_TASK`,
  `PER_IO_DAEMON`, `BUF_REG_OFF_DAEMON` and `SAFE_STOP_DEV` itself.
- libublk 0.4.8 cannot set `NO_AUTO_PART_SCAN`, `SAFE_STOP_DEV`,
  `FAIL_IO` or `PER_IO_DAEMON`. The spike adds the device through the
  control ring and libublk attaches afterwards; this works.
  `NO_AUTO_PART_SCAN` behaves as documented (`/dev/ublkbNp1` appears
  only after `partx -a`).
- The kernel floor stays provisional. The "first kernel" column of the
  feature table comes from the fact-check note and was not re-verified:
  there was no kernel tree on the host.

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

**Gate outcome (phase 0, 2026-11-12):** accepted (gate) — libublk 0.4.8
is an adequate base. At 4 KiB random read, 8 jobs, QD128, `ublk-null`
reaches 3.83 M IOPS against 4.90 M for `null_blk` (0.78×; rule: ≥ 1.0 M
and ≥ 0.5×). At QD1 the median latency is 3.1 µs against 0.7 µs, +2.4 µs
(rule: ≤ +15 µs). Copy mode costs about 0.85 µs of daemon CPU per 4 KiB
I/O. The comparison with the kernel loop device is inconclusive: the
disk limited both to about 6 K IOPS. Four queue threads were used, on a
shared host. libublk 0.5.0 was not published, so it was not evaluated
and the 0.4.x pin holds. Evidence:
[ublk-baseline](../../bench/ublk-baseline/RESULTS.md),
[ublk-datapath](../../bench/ublk-datapath/RESULTS.md).

- At 64 KiB to 1 MiB with 8 jobs, copy-mode ublk reached 17-41 % of
  `null_blk` (the per-byte copy dominates: 64k write 4-10 µs per I/O).
  That is the USER_COPY question in ADR-17.
- The worker pool hands completions back to the queue thread (eventfd
  plus a queue), the only commit model libublk 0.4.8 offers. `--workers
  4` was slower than inline in every mode: at QD1 the 4 KiB random write
  p50 rose from 4.9 to 8.8 µs, about +4 µs per hop. Small synchronous
  requests run inline on the queue thread, as this ADR already says. A
  pool is still needed for hash and LZ4 at high write rates (4-10 µs per
  64 KiB write).
- libublk 0.4.8 gaps that the daemon works around: no control flags
  beyond the basic set (ADR-17), `run_target()` issues `STOP_DEV`
  (ADR-16), and batch completions can be committed only from the queue
  thread. libublk also assumes handlers complete at once: after the
  first `ABORT` it queues no commit and ends its loop, and a completion
  in the same batch as the first `ABORT` keeps its in-flight count above
  zero for ever. The layer keeps its own queue-down flag (set by an
  `ABORT`, or by the control side after `QUIESCE_DEV` or `TRY_STOP_DEV`:
  a queue whose tags are all held gets no `ABORT`), commits held requests
  itself and never blocks in libublk once down.

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

## ADR-19: Shared crates with Constellation

**Amended 2026-11-12 (phase 1 review):** the crate list gains a sixth crate,
`replicix-zdict`, new code and not an extraction. It holds the
zstd-with-dictionary codec (codec id 3, encode and decode, registered
behind `chunkfmt`'s `CODEC_ZSTD_DICT`), the dictionary blob format and a
generic trainer and evaluator, so that Constellation can use them too.
Lunix's study harness depends on it (ADR-30).

**Status: accepted (2026-11-12, maintainer).** All five crates move now,
including `csi-common`, into the public repository
[replicix/replicix-core](https://github.com/replicix/replicix-core).
Constellation is switched to depend on it straight away, importing the
new crates directly (no re-export shims), rather than migrating at its
own pace.

**Gate outcome (phase 0, 2026-11-12):** accepted (maintainer), executed
— milestone P0.9 ran outside the spike plan. `replicix-core` exists at
<https://github.com/replicix/replicix-core> (commit `caa47b4`) with the
five crates named below extracted from Constellation, and Constellation
is migrated onto it (Constellation main `6e25bad6`). There is no
`bench/` result file for this gate.

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

**Gate outcome (phase 0, 2026-11-12):** amended (gate) — online grow
through `UPDATE_SIZE` works under load on ext4, xfs and btrfs: a 16 → 64
GiB grow during fio random write with verify gave no I/O error and a
clean fsck; the new size appeared in sysfs and `blockdev --getsize64`
without a remount, and `resize2fs`, `xfs_growfs` and `btrfs filesystem
resize max` grew the file system online. Grow right after a kill -9
recovery also works. The device reports `write back` and `fua 1` by
default and `write through`, `fua 0` without `VOLATILE_CACHE`. Evidence:
[ublk-recovery](../../bench/ublk-recovery/RESULTS.md), P0.4.

- The driver does not validate the new size. It accepted a size that is
  not a multiple of the 4096-byte logical block (64 GiB + 12 KiB + 512
  B), a resize to the current size, and a shrink from 64 to 8 GiB on an
  idle, unmounted device (the 64 GiB file system then refused to mount,
  and growing back restored it). libublk's wrapper refuses the unaligned
  size; the raw command does not. The daemon therefore validates every
  size: a multiple of the block size, and a shrink only by the rules
  below.
- Shrink with a mounted file system was not tested.

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

**Amended 2026-11-12 (phase 1 review):** two fields that the steps used
without defining are now part of the frozen bodies. An EPOCH commit has the
optional field `authority` (step 5 writes `EPOCH{authority: B}`). The
`lease` has the optional field `transferred_to` (step 6).

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
   - *Amended by ADR-11 (F5):* like every commit kind, CONFIG and
     TRANSFER carry `parent_hash` and `epoch`. The abandoned branch of a
     DR steal (item 6) therefore cannot be spliced into the surviving
     chain.
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
  - **Interior child entries reserve `lineage_idx` too (M1.A0 review).**
    The 48-byte interior entry `{child_hash, pack, off, len}` gets
    `lineage_idx: u8` in the unused top byte of `len` (`len` is at most
    65 644, so a u24 holds it): 0 in v1, rejected if non-zero. Without it
    a clone would have to copy every leaf and interior node of its base
    (about 300-350 MiB per TiB) at creation. The entry size and the node
    hash (child hashes only) do not change.
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

## ADR-26: Three storage topologies (local only, local + S3, S3 only), switchable live

**Amended 2026-11-12 (phase 1 review):**
- **Index packs** have the bucket path `index/<pack:016x>`. A FULL's
  `index_packs[]` names only runs that were durable before the commit, so
  the runs may lag one commit (ADR-9).
- **Local file set.** The local directory holds `volume` (the exact
  descriptor bytes), `volume.json` (local-only settings), `state/` with the
  local `commits/`, `checkpoint`, `device` and `pins`, and
  `<dir>/families/<id>/keyring`; the host registry is
  `<state_dir>/volumes.json` (DESIGN §4). Local commits record `epoch` = 0
  until a backend exists and are sealed under their bucket-relative names.
- **Device identity.** Before recovering a device the daemon checks the
  recorded `boot_id`, queue count, depth, flags and size against the device
  (DESIGN §3).

**Gate outcome (phase 0, 2026-11-12):** accepted (gate) for the
keeper-held RAM write buffer; the systemd FD-store variant is *not run*.
A memfd ring with a header `{magic, device id, generation}`, held by a
keeper over `SCM_RIGHTS`, survived 23 mounted ext4 kill -9 cycles (3 +
20) with fio verify and two fsync writers: fsck clean, zero bad blocks
of acknowledged fsyncs. Exec to recovered took p50 314 ms and max 1.26 s
over the 20 cycles; heartbeat stall p50 594 ms, max 1.82 s. **Lost
keeper:** with the keeper and daemon both killed, the restarted daemon
found no buffer, printed `REFUSED`, deleted the device and never resumed
it; the file system saw EIO and went read-only. The generation-mismatch
refusal exists in code but was not exercised separately. Acknowledgement
latency of the RAM sink on a raw device, 4 KiB random write, no FLUSH:
3.7 µs p50 at QD1 (p99 11.7 µs) and 42 µs at QD32 (712 K IOPS), against
2.7-4.6 ms for a durable journal write on the P0.5 loop-backed file
system. Evidence: [ublk-recovery](../../bench/ublk-recovery/RESULTS.md),
P0.12.

- Replay of a 256 MiB buffer after kill -9: 3.76 s with 64 KiB records
  and 23.7 s with 4 KiB records, onto a slow local file (about 10 K
  random 4 KiB writes per second). The device stalls for the whole
  replay. In the real design the replay feeds the commit path, so these
  are upper bounds for a slow medium.
- The spike's RAM sink drains inline on the queue thread when the ring
  fills (backpressure by delay, about 1.4 s once). In phase 3 the drain
  must not block the queue thread.
- A refusing process stays blocked in `DEL_DEV` while a mounted file
  system holds the block device, and completes when the file system is
  unmounted (see ADR-16).
- Not run: the systemd FD-store variant (it needs `sd_notify(FDSTORE=1)`
  and a `LISTEN_FDS` recovery path in the daemon), and replay against a
  real remote sink. fio's verify coverage was limited by variable block
  sizes with `norandommap`; the fsync writers and the checksums of the
  data files are independent of that.

**Context:**
- Lunix assumed a local directory on every owning host. Diskless or
  ephemeral nodes, and pods that move often, want a volume with **no
  local storage at all**.
- The maintainer decided (2026-11-12) that a volume must run in any of
  three topologies, and that adding or removing the local tier or any
  S3 backend must never disrupt the device.

**Decision:**
1. **The local tier has a role**, chosen per volume on the owning host:

   | role | contents | needs | evicts |
   |---|---|---|---|
   | `store` | a complete copy: journal, every live segment, map, index | — | nothing |
   | `cache` | journal plus a partial, evictable copy (ADR-14) | ≥ 1 backend | clean data (S3-FIFO) |
   | `none` | no local files; a RAM write buffer and RAM caches | ≥ 1 backend | RAM caches |

   The three topologies follow from it: **local only** = `store` with no
   backend; **local + S3** = `cache` (or `store`, which keeps a full local
   replica) with one or more backends; **S3 only** = `none` with one or
   more backends.
2. **Copy invariant.** At every instant, every FLUSH-acknowledged write
   is durable on at least one durable sink: the local journal or the
   authority's commit chain. The volume also has at least one complete
   copy: a `store` local tier or the authority. The daemon refuses any
   transition that would break either rule. In particular, it refuses to
   remove the last backend unless the local tier is `store`.
3. **The S3-only write path.**
   - **RAM write buffer:** a ring of journal records in a `memfd`,
     in the same record format as the disk journal (DESIGN §4.1). The
     overlay, destage and cut code are therefore unchanged; only the sink
     differs. Writes are acknowledged from RAM.
   - **Durability:** FLUSH and FUA complete when a DELTA commit is on
     the authority, as in sync mode (ADR-10). `async` is allowed only
     with an explicit `--unsafe-flush`, for scratch volumes, the
     equivalent of QEMU's `cache=unsafe`, because a FLUSH would otherwise
     claim a durability the volume does not have.
   - **Bounds:** the buffer has a size limit, `mem_buffer` (default
     256 MiB per volume). It holds unsynced records and the open
     segment. When it fills, a memory ladder runs: cut and commit
     sooner, destage, then throttle admission. There is no ENOSPC; an
     optional `bucket_quota` can add one.
4. **Daemon restart without a local disk.**
   - A daemon crash is not a power loss: the file system above keeps
     running and still believes its acknowledged writes. The memfd
     therefore outlives `lunixd`. A **keeper** holds the file
     descriptor: the systemd file-descriptor store (`FDSTORE=1`), or
     under Kubernetes a tiny `lunix-keeper` process in the DaemonSet pod
     that holds it via `SCM_RIGHTS`.
   - On ublk user recovery, the new `lunixd` takes the memfd back and
     replays it like a journal.
   - If the buffer is missing, or its generation does not match the
     device's, the device is **not** recovered. It is deleted, so I/O
     fails with EIO, and acknowledged writes never vanish silently under
     a mounted file system. A host power loss loses the buffer, which is
     legal (ADR-3).
5. **Reads and metadata without a local disk.**
   - Interior map nodes are resident, as in every topology. Leaves and
     extents go to a bounded RAM cache, `mem_cache` (default 256 MiB,
     S3-FIFO). Readahead fills it.
   - **The chunk index lives in the bucket.** Its runs are already
     published as `index_packs` in every FULL commit (DESIGN §8.2). With
     no local tier, dedup consults a RAM filter, then a cached ranged GET
     of the index block on a filter hit. This happens on the destage path
     only, never on the acknowledgement path.
   - Compaction and GC fetch live extents with ranged GETs. They are
     rate-limited and cost-scored as usual.
6. **Live transitions.** The device stays up throughout, and no request
   fails.
   - **Switching the write sink at a seq barrier `b`:** writes with
     `seq ≤ b` stay in the old sink, and writes with `seq > b` go to the
     new one. Overlay entries carry their sink. A FLUSH covering both
     waits for both. The old sink is retired once a commit covering `b`
     is on the authority.
   - **`local none → cache`:** create the directory and open the
     journal, then switch sink. The RAM buffer drains through the
     normal commits. The cache then warms on demand, optionally from the
     heat map (ADR-23).
   - **`local cache → none`:** the volume must be in sync mode, or
     `--unsafe-flush`, and the CLI says which. Switch sink to RAM, then
     commit a cut covering `b`, so every journal record and every local
     segment is on the authority. Then drop the cache and delete the
     directory.
   - **`local cache → store`:** hydrate. Every segment reachable from
     retained commits is copied into the local tier, using the backfill
     machinery (ADR-15). Eviction stops when hydration completes.
   - **`local store → cache`:** needs at least one InSync backend, as
     today (DESIGN §13).
   - **Removing the last backend:** needs `store`. A final
     `CONFIG{backends: [], detached: local}` is written on the
     authority, and the lease is released. Commits continue locally. The
     bucket keeps a valid but stale read-only image, which `volume
     import` refuses without `--force`.
   - **Adding or removing mirrors, and authority transfer**, work as
     before in every topology (ADR-15, ADR-24).
   - **S3 only → local only:** add a local cache, hydrate to `store`,
     then remove the backends.
7. **Control surface:**
   - `lunix volume create|attach --local none|cache|store`;
     `lunix local NAME none|cache [--size]|store`.
   - The StorageClass parameter `localTier` (default `cache`), with
     `cacheSize`.
   - Read-only time-travel mounts (ADR-25) already behave like `none`
     readers, and now share its code.

**Rejected:**
- *Acknowledging FLUSH from RAM by default in S3-only.* FLUSH would lie
  both on power loss and, silently, on a daemon crash.
- *A tmpfs or zram directory as the "local disk".* It runs today's code,
  but `fdatasync` on it is a no-op, so FLUSH claims a durability it does
  not have. Space-ladder errors would also hit RAM limits instead of a
  file system. The RAM buffer is the same idea, done honestly.
- *Writing each write straight to S3.* That means one PUT per 4 KiB write,
  which is impossible on both latency and cost.
- *Changing topology only at detach.* That breaks the no-disruption
  requirement.
- *Making the local tier a member of the CONFIG backend set.* Any host
  must be able to reach a backend, and a host-local copy is not
  reachable. The chain records only `detached: local`, so a stale bucket
  is never mistaken for the live volume.

**Consequences:**
- **Performance:**
  - S3-only writes are acknowledged at RAM speed, and FLUSH costs one
    PUT round trip, as in sync mode.
  - Read misses cost a GET. Latency-sensitive S3-only volumes should use
    a same-region bucket, or S3 Express One Zone as the authority.
- **Memory per S3-only volume:** `mem_buffer + mem_cache` plus interior
  map nodes (≈ 1.6 MiB/TiB) and the index filter.
- **Format:**
  - The journal record format is shared by disk and RAM sinks, and is
    frozen in phase 1 as before.
  - CONFIG gains the `detached` field.
  - The segment, map and commit formats are unchanged.
- **Gate (phase 0, P0.12):** the P0.3 kill -9 recovery cycle, repeated
  with a memfd buffer held by a keeper. The checks:
  - fsck is clean;
  - no acknowledged-and-flushed write is lost;
  - a lost keeper fails the device with EIO, and never resumes it with
    stale data.
- **Amends:**
  - ADR-3: durability in S3-only is an S3 commit.
  - ADR-4: the journal may be a RAM sink.
  - ADR-10: S3-only implies sync unless `--unsafe-flush`.
  - ADR-14: the local store is optional.
  - ADR-16: the keeper process.

## ADR-27: Durability is two settings: `write_ack` and `flush`

**Context:**
- A file system's fsync reaches the device as a FLUSH (`REQ_PREFLUSH`)
  and FUA writes. Lunix advertises a volatile cache and FUA (ADR-3), so
  it receives both.
- The maintainer asked (2026-11-12) for durability to be configured as
  two explicit choices:
  1. Whether an acknowledged *write* survives the loss of the whole
     client machine.
  2. Whether an acknowledged *fsync* is persisted to the local disk only,
     or to S3.

**Decision:** two per-volume settings, changeable at runtime. The
ADR-10 modes become presets of them.

| setting | `local` | `s3` |
|---|---|---|
| `write_ack`: when a write completes | once it is in the local journal (or the RAM buffer, with no local tier) | once a DELTA commit holding it is on the authority: write-through |
| `flush`: when a FLUSH or FUA completes | after a journal `fdatasync` | once a DELTA commit holding everything before it is on the authority |

| `write_ack` | `flush` | preset | survives losing the client machine |
|---|---|---|---|
| `local` | `local` | `async` (default with a backend) | commits up to the last `T`/`B` cut |
| `local` | `s3` | `sync` | every fsync-acknowledged write |
| `s3` | `s3` | `write-through` (new) | every acknowledged write |
| `s3` | `local` | refused: contradictory | — |
| — | — | `local` (no backend) | nothing once the disk is lost |

- **`write-through`:**
  - Each write waits for the group commit that the sync mode uses for
    FLUSH. Concurrent writes share one DELTA PUT, with one PUT in flight
    per volume, and FLUSH then completes at once.
  - The device keeps advertising a volatile cache, so no ublk parameter
    changes and the switch is live. A device may always behave more
    durably than it advertises.
- **Local tier `none` (ADR-26):** `flush = local` has no disk to persist
  to, so it is refused there unless `--unsafe-flush` is given. Then
  FLUSH completes from the keeper-held RAM buffer.
- **Background commits:** in every combination, FULL commits still run
  on the `T`/`B`/`B_max` cadence and fold DELTAs.
- **Configuration:**
  - The CLI is `lunix durability NAME --write-ack local|s3 --flush
    local|s3`. `lunix mode` stays as preset shorthand.
  - The CSI StorageClass parameters are `writeAck` and `flush`.

**Rejected:**
- *Write-through by advertising no volatile cache.* ublk device
  parameters cannot be changed on a live device, so switching modes
  would need a detach. It would also stop file systems sending FLUSH,
  which loses the ordering hints.
- *Making write-through the default.* Every write costs one S3 PUT round
  trip. The measured S3 Standard PUT p50 was 70 ms, so a queue-depth-1
  writer gets about 14 IOPS, and QD32 gets about 450. Express One Zone,
  with a p99 of about 7 ms, gives roughly 10× that. It is also billed per
  PUT group, like sync mode (ADR-10 cost note).
  - Applications that need durability already fsync. So do file system
    journals and databases. `flush = s3` (`sync`) gives them the same
    guarantee at a fraction of the cost.
  - `write-through` matters only for writers that never issue a FLUSH.

**Amends:**
- ADR-10: its modes are presets of these two settings, and
  `write-through` is added.

## ADR-28: FLUSH and FUA stay prefix cuts; flush-lane discipline instead of staging or a FUA fast path

**Gate outcome (phase 0, 2026-11-12):** partial — the P0.10 lane test
and the two-PUT measurement are done on AWS S3 Standard and OVH; the
phase 3 end-to-end gate stays open. **Two serial 4 KiB create-only
PUTs** (the FLUSH-then-FUA pattern), AWS same region: p50 54 ms, p90 62
ms, p99 94 ms (n = 200), 1.8× a single 4 KiB PUT (p50 29.7 ms). OVH from
a remote client (RTT 20 ms): p50 97 ms, p99 133-212 ms. A 1 MiB data PUT
then a 4 KiB commit PUT on AWS: p50 81 ms, p99 133-189 ms; the commit
PUT after a data PUT is no slower than a stand-alone one. This supports
"about 2 PUT RTT for an ext4/xfs fsync"; on AWS Standard the measured
pair is about 54 ms, below the about 140 ms estimated under
Consequences. Evidence: [s3probe](../../bench/s3probe/RESULTS.md),
[fsync-trace](../../bench/fsync-trace/RESULTS.md),
[ublk-recovery](../../bench/ublk-recovery/RESULTS.md).

- **Lane rule (4 KiB PUT p99 under load ≤ 1.5× idle):** not resolved.
  With 8 concurrent 16 MiB uploads (about 0.5 GB/s, unthrottled) on AWS,
  the p50 of a 4 or 64 KiB PUT rose 6-11 % with or without a separate
  client. The 4 KiB p99 was 51 ms on a separate client (idle 59 ms,
  0.87×) and 198 ms on the shared client (3.4×, one outlier in about 45
  samples). On a saturated 31 MB/s link to OVH the p99 was 1.13×
  (separate) and 1.18× (shared). A separate connection pool, as rule 2
  specifies, is the safe choice; p99 needs more samples.
- `CompleteMultipartUpload` (conditional or not, 1-4 parts under 20 MiB)
  is one request of about 87 ms on AWS and 76 ms on OVH, about 3× and 2×
  a 4 KiB PUT. A multipart commit is not cheaper on the flush path:
  Create, parts and Complete cost about 146 ms on AWS against 30 ms for
  one create-only PUT.
- Segment upload form (ADR-7): keep one PUT per segment up to 16 MiB. On
  AWS a 16 MiB single PUT has p50 194 ms against 249-252 ms for two or
  three parallel parts, because Create (about 23 ms) and Complete (about
  90 ms) cost more than the parallel parts save. Multipart pays from
  about 25-32 MiB (32 MiB in four 8 MiB parts: 265 ms against 336 ms).
  Both providers reject non-final parts under 5 MiB. `flush_segment` (4
  MiB) is a single PUT.
- FLUSH per sync (P0.4): one FUA write and one FLUSH per single-job sync
  on ext4, xfs and btrfs, and no file system issues more than 1 FLUSH
  per sync. This matches the fsync-trace.
- Not run: PUT latency per size with 8 parallel streams, the shaped 200
  MB/s bulk upload, and R2 and S3 Express One Zone (no credentials). The
  phase 3 end-to-end gate below is unchanged.

**Context:**
- Recommended by the design review in
  [fsync scope](research/2026-11-12-fsync-scope-recommendation.md).
- The maintainer asked (2026-11-12) whether an fsync of one file could
  persist only that file's data under `flush = s3`. It cannot: the device
  sees no files, and journal transactions span files. The remaining
  question was whether to shrink the cost of a FLUSH that drags in every
  write completed before it.
- Measured on kernel 7.0 ([fsync-trace](../../bench/fsync-trace/RESULTS.md)): an ext4 or xfs
  fsync is one empty FLUSH request followed within microseconds by one
  FUA write of 4 KiB or less; the flush group carries everything written
  since the previous flush point (4.1 MiB at a 200 MB/s stream, 19 MiB
  p50 and 38 MiB max at 1 GB/s) at an unchanged flush rate; a bare FUA
  with no FLUSH ahead of it arises only from `O_DIRECT|O_DSYNC` overwrite,
  which no database in scope uses by default.
- Modelled ([background staging §3](research/2026-11-12-fsync-background-staging.md)): staging
  unflushed writes to S3 ahead of the FLUSH helps only with 1-2 MiB
  objects (about 135-160 ms against a 260 ms baseline and a 65-70 ms
  floor) and costs 3x to 11x the AWS request bill, loses overwrite
  absorption, and needs sealed staged objects, orphan GC and mirror
  fan-out. [FUA and FLUSH semantics](research/2026-11-12-fua-and-flush-semantics.md) shows that
  persisting a FUA write alone is a legal crash image but produces
  commits that are not cuts.

**Decision:**
1. **Prefix cuts stay.** A FLUSH completes when a DELTA holding every
   write with `seq <= cut` is on the authority (ADR-10). A FUA write is
   treated as a FLUSH followed by itself: its commit is a cut at its own
   `seq`. Every commit on S3 remains a cut. No background staging and no
   FUA-only commit in v1.
2. **Two upload lanes per backend** (DESIGN §9):
   - the **flush lane**: a small dedicated connection pool (default 3)
     that carries commit objects and DELTA bodies up to the DELTA cap,
     and nothing else; its PUTs get the hedged-retry budget of reads;
   - the **bulk lane**: segments, map and index packs, FULL folds, mirror
     copies, GC and compaction traffic, with an in-flight byte cap
     `upload_inflight_max` (default 32 MiB) and a token bucket at
     `upload_share` (default 0.8) of the goodput measured by `doctor` or
     by the lane itself.
3. **A sync-mode unflushed-byte cap.** With `flush = s3`, bytes that are
   acknowledged, not yet in a PUT and not covered by a commit are capped
   at `sync_dirty_max` (default 64 MiB; with local tier `none`, at most
   `mem_buffer / 4`). Above it, admission throttles, as `B_max` does in
   async mode. The fsync bound is `sync_dirty_max / goodput + 2 PUT RTT`.
4. **Over-cap flush groups** are split into segments of at most
   `flush_segment` (default 4 MiB, inside ADR-7's range) uploaded in
   parallel on the bulk lane with AIMD; the DELTA commit that names them
   follows on the flush lane (ADR-9 upload order unchanged).
5. **Format reservation.** A DELTA commit's reference list is
   `refs: [{kind: segment | journal, id, hash}]`. `journal` (a sealed raw
   journal batch uploaded ahead of a FLUSH) is reserved and unused in v1,
   so staging can be added later without a format change.
6. **Guidance and telemetry.** Metrics `flush_group_bytes`,
   `flush_latency` and `flush_lane_queue`. The CLI warns when a
   `flush = s3` volume's p50 flush group exceeds 1 MiB and recommends a
   separate volume for the bulk writer, or S3 Express One Zone as the
   authority for low-volume latency-sensitive volumes. Documentation
   states that an ext4/xfs fsync costs two PUT round trips in
   `flush = s3`.

**Rejected:**
- *Background staging of unflushed writes* (sealed small segments, raw
  journal batches, pipelined commits, multipart Complete as commit,
  Express append). Modelled gain about one half of a PUT time over the
  rules above; loses overwrite absorption (LSVD measured write
  amplification 1.97 to 1.35 on some traces); early destage forces
  partial-chunk RMW, a GET on S3-only volumes; 3x to 11x the AWS request
  bill for the 200 MB/s case; new key-derivation context; orphan GC;
  mirror fan-out. Deferred, not closed: see the gate.
- *Pipelined commits naming objects not yet durable.* Breaks ADR-9's
  invariant and adds a second validity notion beside the F5 chain
  verification.
- *A FUA-only fast path.* Legal but it introduces non-cut commits, which
  need seq-set framing, highest-seq replay across commits for writes,
  discards and holes, a cut/non-cut marker in restore points, time-travel
  and rollback, and new GC and mirror rules, for a workload (QEMU
  `cache=directsync`, fio `--sync=1`) nobody runs in production.
- *Dropping `UBLK_ATTR_FUA`.* Removes bare FUA at the source but turns
  every `O_DSYNC` write into write plus full FLUSH and switches off
  iomap's write-through path.
- *Per-file sync.* The device sees no files; journal transactions span
  files.

**Consequences:**
- DESIGN §20's "sync-mode fsync latency about 1 PUT RTT" becomes
  "about 2 PUT RTT for an ext4/xfs fsync (FLUSH then FUA, serialised by
  the block layer); 1 RTT for data-only fdatasync". Standard: about
  140 ms; Express One Zone: about 14 ms.
- Request cost is unchanged for small groups; large groups cost one PUT
  per `flush_segment`, which is the same curve any sub-16 MiB upload is
  on.
- No new object kinds, no change to the copy invariant (ADR-26), to
  mirrors (ADR-24) or to retention (ADR-25). Write-through (ADR-27) is
  unaffected: its groups already use the flush lane.

**Gate (phase 0, extends P0.10; phase 3 for the end-to-end run):**
- P0.10 adds PUT p50/p99 at 4 KiB, 1, 2, 4, 8 and 16 MiB per provider,
  single-stream and 8 parallel, with and without a concurrent 200 MB/s
  upload on separate connections. The lane rule is confirmed if the
  4 KiB PUT p99 under load stays within 1.5x its idle value.
- Phase 3: the fsync-trace s2a/s2b mixes on ext4 and xfs over a real
  `flush = s3` volume, with and without rules 2-4, on AWS Standard and
  one zero-request-fee provider. If p99 fsync latency under the 200 MB/s
  mix remains above 2x the two-PUT floor, staging of raw journal
  batches (`refs.kind = journal`) is designed as ADR-29.

**Amends:**
- ADR-3: the rejection "Arbitrary subsets at sync points. Ordering
  matters to journaling file systems." is replaced by the text below.
- ADR-10: the sync bullet gains "FUA is a cut at its own seq; an ext4/xfs
  fsync is therefore two commits"; the cost note cites this ADR for the
  group-size curve.
- DESIGN §8.3: add lanes, `sync_dirty_max`, `flush_segment`, `refs[]`.
- DESIGN §9: two connection pools per backend; `doctor` reports PUT
  latency at 4 KiB, 1 MiB, 4 MiB and 16 MiB.
- DESIGN §13.1: `sync_dirty_max` is the first rung of the memory ladder.
- DESIGN §20: fsync target corrected as above.
- DESIGN §22: add "Staging of raw journal batches ahead of FLUSH
  (ADR-28 gate)".

## ADR-30: Compression advisor and zstd dictionaries, trained on demand from the volume's own data

**Amended 2026-11-12 (phase 1 review):**
- **The benchmark moves to phase 1** (plan milestone M18). It still sets the
  adoption thresholds and the built-in set, and phase 4 starts from its
  results. It has two arms:
  - the Lunix arm, as specified under Phasing;
  - a Constellation-style arm: whole files under 4 MiB as single objects,
    4 MiB chunks of larger files, and the dictionary chosen by file
    extension.
- **Core split.** The dictionary codec (codec id 3, encode and decode), the
  dictionary blob format and a generic trainer and evaluator live in
  replicix-core as the crate `replicix-zdict` (ADR-19), so Constellation
  can use them. Phase 1 pins its codec-3 decoder test vectors as goldens.
- **Frozen `dicts/` body.** Phase 1 freezes only the envelope
  `{kind id, ver, bytes}`; `bytes` is the blob defined by `replicix-zdict`.

**Context:**
- Each chunk is compressed on its own (ADR-12), so a 4-16 KiB chunk has
  little history to match against. zstd dictionaries fix that for
  similar-but-not-identical content (database pages, file-system
  metadata, text, OS files), which dedup cannot touch because dedup needs
  identical chunks. On mixed user data they gain little.
- Automatic training would guess on the user's behalf. The maintainer
  proposed (2026-11-12) an explicit command that trains on the volume's
  own data and reports the achievable gain before anything changes.

**Decision:**
1. **`lunix compress estimate|train|apply|ls|rm`:**
   - `estimate NAME [--level N] [--dict auto|builtin:<name>|<id>]
     [--sample 1%|64M] [--exact]` answers "what would this setting save
     on this volume?" for higher zstd levels, built-in dictionaries or a
     trained candidate. It changes nothing.
   - `train NAME [--classes auto|1-4] [--sample …]` produces candidate
     dictionaries and the same report.
   - `apply NAME --dict <id> [--new-only | --rewrite cold | --rewrite
     all]` turns a dictionary on. The default is new writes only;
     `--rewrite` recompresses existing extents in the background through
     ADR-12's cold-segment recompression.
2. **Sampling:**
   - Chunks are drawn uniformly over the current map's live extents. Holes
     and extents stored raw are skipped, and their share is reported
     because it caps any gain.
   - Reads come from the local tier, else by ranged GET. When the sample
     needs backend reads, the command first prints the GET count and the
     bytes to read, and asks for confirmation. It does not price them:
     prices depend on the provider, the region, the network path and the
     contract.
3. **Training:**
   - zstd's trainer (`ZDICT`, fastCover) runs on 80 % of the sample in a
     low-priority background task inside `lunixd`.
   - With `--classes`, a cheap per-chunk classifier (magic numbers,
     text against binary, byte entropy) splits chunks into up to 4
     classes, one dictionary each. Unclassified chunks use no dictionary.
4. **Report:** measured on the held-out 20 %:
   - per candidate: dictionary size, class, share of stored bytes it
     applies to, and the ratio with and without it;
   - volume-wide: stored bytes now and projected, with a 95 % confidence
     interval (from a bootstrap over the sample);
   - destage CPU change, the change in stored bytes per backend, and the
     GET and PUT counts and bytes that `--rewrite` would need. No money
     figures: the operator applies their own provider's prices.
   - `--exact` compresses every eligible chunk instead of a sample. Its
     backend reads are counted and confirmed first, like any sample.
5. **Adoption rule:** `train` marks a candidate "recommended" only if it
   gains at least 5 % on its class and 1 % volume-wide. Otherwise the
   volume stays without a dictionary.
6. **Built-in dictionaries:** Lunix may ship dictionaries trained once on
   public data (ext4 and xfs metadata, common Linux distributions,
   PostgreSQL and InnoDB pages). They are identified by a global id,
   versioned forever, carried in the binary and never stored in a
   bucket. Nothing is trained on user data for them.
7. **Security:** training happens inside `lunixd`, which already holds
   the keys. Trained dictionaries derive from plaintext, so they are
   sealed like other metadata. The report shows only sizes, ratios,
   request counts and CPU.

**Format reservations (frozen in phase 1, unused until the feature
ships):**
- **Codec id 3:** zstd with a dictionary. The dictionary is named by the
  `dictID` field of the zstd frame header. Ids with the top bit set are
  built-in dictionaries; the others are the volume's trained ones.
  **Known risk:** the zstd format specification lists dictionary ids of
  2^31 and above as reserved for future use. libzstd does not enforce
  this and accepts such ids when compressing and decompressing, but
  another zstd implementation could reject them. The ids are kept (the
  `zdicts` repository's `docs/versioning.md` carries the same note).
- **`dicts/<id:08x>`:** a create-only object under the volume prefix
  holding a trained dictionary, sealed with its own metadata key (added
  to ADR-11's sealed-object table).
- **GC rule:** a trained dictionary is live while any extent reachable
  from a GC root references it. It is deleted only after the usual
  condemn, grace and recheck (ADR-25, DESIGN §11). Mirrors copy
  `dicts/` before any segment that uses it.
- **Clones (post-v1):** a clone reads its bases' dictionaries from the
  parent's prefix, which the parent's holds keep alive.

**Rejected:**
- *Automatic background training.* It spends CPU and backend reads on
  volumes where dictionaries gain nothing, and changes stored bytes
  without the operator's say. A later policy may run `train` on a
  schedule, with the same adoption rule.
- *One global dictionary trained across tenants.* It would leak data
  across families, as ADR-13 rejects for dedup.
- *A dictionary id field in the extent header.* The zstd frame already
  carries one, and the codec id says when to read it.

**Consequences:**
- **Reads:** a cold read of a dictionary-compressed extent needs the
  dictionary first: one small GET per volume, then cached. Decompression
  speed is unchanged.
- **Memory:** a loaded dictionary costs its size plus zstd's prepared
  state, a few hundred KiB to a few MiB per dictionary and level.
- **Dedup and encryption** are unaffected: chunk ids hash the plaintext,
  and every extent gets a fresh salt, so a rewrite is safe.
- **Phasing:** a v1 requirement (maintainer, 2026-11-12).
  - Phase 1 freezes the reservations. It also runs a benchmark of
    dictionaries against plain zstd levels at 4, 16 and 64 KiB on real
    block-device data (it sets the adoption thresholds and the built-in
    set), adds a Constellation-style arm, and delivers `replicix-zdict`
    (codec id 3, the dictionary blob format and the trainer and evaluator)
    in replicix-core.
  - Phase 4 delivers the `lunix compress` commands (`estimate`, `train`,
    `apply`, `ls|rm`) and the dictionary read path next to `gc --dry-run`
    and the space accounting, starting from the phase 1 benchmark's
    results.
- **Amends:** ADR-12 (dictionaries are no longer simply deferred; the
  read path and the command are planned) and ADR-11 (`dicts/` joins the
  sealed-object table).

## ADR-31: The rollback anchor: a local floor in v1, mirrors and an orchestrator floor in phase 5

**Status: accepted (2026-11-12, maintainer: adopt the M2 review's
recommendation).** It closes DESIGN §22.1 and amends ADR-9 and ADR-11.

**Context:** a bucket writer cannot forge a commit, splice two chains or
replay a lease (ADR-9, ADR-11 F5), but it can serve an old, fully valid
prefix of the chain. Nothing inside the bucket tells that prefix from the
current one. `state/pins` already holds `max_keyring_seq` and `max_epoch`,
and every attach writes an EPOCH commit, so a host that attached before
detects a rollback to before its last attach through `max_epoch`. It does
not detect commits hidden from the last session (same epoch), and it
detects nothing on a host that never attached. The checkpoint's
`last_commit_seq` is a hint that is discarded when above the tip (D17); it
is not an anchor.

**Decision:**
- **v1 (mandatory): a local floor.** `state/pins` becomes version 2 and
  gains `{max_commit_seq, tip_hash}`, where `tip_hash` is the BLAKE3 of the
  stored bytes of the commit at `max_commit_seq` (the `parent_hash`
  convention).
  - It is raise-only. It is written after every verified attach (after the
    attach's own EPOCH commit) and after every FULL commit is durable (step
    C6 of the commit protocol), never before: a floor above the durable tip
    would refuse an honest volume after a crash.
  - At attach, after `verify_chain`, a tip with `commit_seq <
    max_commit_seq` is refused as `Rollback`, and a tip with `commit_seq ==
    max_commit_seq` and a different hash as `Fork`.
  - **The fork window (maintainer, 2026-11-13, after the M12a review).** A
    tip above the floor must descend from the floor's commit: the chain is
    walked from the tip back to height `max_commit_seq` (every link
    authenticated, also below `chain_floor`) and must arrive at a commit
    whose hash is `tip_hash`, else `Fork`. With `retain_last = 2` the
    retained chain alone covers only one commit above the floor, so without
    the walk a copy that diverged two or more commits before the tip would
    attach and raise the floor. When the walk cannot be completed (a link
    deleted or damaged), the attach is refused as `ForkUnverifiable` until an
    operator overrides it with an explicit, audited attach flag. GC keeps
    every commit object at or above the floor's height while a floor exists
    (plan M17 rule 6).
  - The floor is recorded at volume creation, with the genesis commit, so a
    volume's first attach on the host that made it is not a "first attach".
  - A rejected `state/pins` (bad length, version or checksum) is an error,
    never read as "no floor". This is rule R4 of
    ADR-11, together with `open_lease_fresh(bytes, max(chain epoch,
    pins.max_epoch))`.
  - Only offline `fsck --repair --truncate-tip` and an explicit,
    audit-logged operator reset lower it. A DR steal from a lagging mirror
    (ADR-24) is the case for the reset.
  - The file is local and never uploaded. Cost: one small local write per
    FULL.
- **Phase 5 (several backends): the maximum over InSync mirrors.** An
  authority whose tip is below an InSync mirror's tip is refused (a mirror
  is a byte-identical copy that may have seen a later commit).
- **Phase 5 (Kubernetes): a raise-only floor** `{commit_seq, tip_hash}` in
  the orchestrator's status, written once per FULL, so a pod that moves to a
  host with no local floor still has one.
- **Documented later upgrade: a notary bucket.** A write-only bucket in
  another account holds a signed or MAC'd `{seq, tip_hash, epoch}` per FULL.
  It defeats an attacker holding only the data bucket's credentials, at the
  price of one PUT per FULL and new infrastructure.

**What it does not protect (the docs say so):**
- A host with no floor (first attach, an ephemeral client with no
  orchestrator floor) cannot detect rollback. This is ADR-11 F7's "by
  construction" case, restated for history.
- A purely local volume cannot be protected from anyone who can write its
  local disk: that person also holds `integrity.key` and `state/pins` and can
  rewrite both. Integrity mode defends the bucket copy. For a stolen or
  modified local disk the honest claim is corruption detection and crash
  safety, and encrypted mode with a passphrase slot resists a reader of the
  disk, not a writer who also has the key file.
- An attacker holding the credentials of every backend and the host is out
  of the model.

**Rejected:**
- *The checkpoint as an anchor.* It is a hint, discarded when above the tip.
- *A KMS counter.* A KMS has no monotonic counter.
- *A bucket-side floor object.* The attacker who rolls the chain back
  rolls that object back too.
- *Deferring all anchors to phase 5.* It would leave v1 with no detection at
  all for a bucket-writer attack that costs nothing to defend against on a
  host that has attached before.

**Consequences:**
- **Format freeze.** `state/pins` version 2 and the new addressing-key
  purpose id (ADR-6, ADR-11) amend frozen items; each needs the maintainer's
  re-sign of `FREEZE.md` and a golden update.
- New attach errors `Rollback`, `Fork` and `ForkUnverifiable` in the
  control API and the CLI, and the audited override; `lunix status` shows
  the floor, also for a detached volume.
- The rollback floor and `chain_floor` (retention) are unrelated: GC
  deleting old commits never lowers the rollback floor.

## ADR-32: Write back-pressure: a paced admission band below the limit, and a bounded full volume

**Status: proposed (2026-11-18), for the maintainer's decision.** Nothing in
it is implemented. If accepted it amends ADR-14 (the ladder's order and
`on_full`) and DESIGN §2, §4.3, §19 and §20. The evidence, the analysis, the
prior art, the alternatives in full, the test list and the implementation
sketch are in
[research/2026-11-18-write-backpressure.md](research/2026-11-18-write-backpressure.md)
(cited below as "the note").

**Context:**
- The space gate has one threshold, the limit `min(cache_max, fs_ceiling -
  reserve)`, and with no `cache_max` the limit is the host file system. Below
  it nothing slows a writer. Runs of 2026-11-18: on `main` a 4 GiB working
  set took 107 GiB of store within minutes and the urgent pass never ran; on
  `p1/gcrate` the same shape held about 49 GiB after 10 min, with compaction
  (about 12 of 64 MiB/s) the bottleneck.
- At the limit a write parks, and the defaults (`on_full = wait`, no
  `full_timeout`) give it no deadline. On a store that reached its limit with
  no collector, one write parked for more than 25 min with 128 requests in
  flight and hung-task reports; a restart with `on_full = error` did not
  release it; only killing the daemon and force-deleting the device did.
- The code has six paths by which a request stays parked without bound (the
  note, §2): no default deadline; a policy change that does not re-judge
  parked requests; a quiesce that cannot drain them; discards that queue
  behind them; deadlines that share a thread with the urgent GC pass; and
  writes parked for a full journal ring, which have no deadline at all.
- With a grace, garbage is proportional to the write rate: the store holds
  about `R × τ` of it, `τ` about 95 s with the defaults on `p1/gcrate`. A cap
  `L` therefore fixes the sustained write rate at `(L - live store) / τ`,
  whatever the mechanism (the note, §3).

**Decision (proposed):**
- **Signal: the headroom** `H = limit - used`, read from the ledger. No rate
  estimate and no GC statistic is a control input in phase 1.
- **Three marks, relative to the limit:**
  - soft: 15 % of the limit, clamped to 1 GiB ..= 64 GiB and to half the
    limit. Above it writes are untouched, at the cost of one comparison
    under a lock the write already takes;
  - hard: the reservation does not fit, as today;
  - resume: the larger of 4 segments and 1 % of the limit.
- **Every volume gets a limit of its own.** `lunix volume create` writes
  `cache_max = store_max_percent × size` (default 200 %) with the existing
  key.
- **Mechanism: pace the admission of a write's journal reservation**, the
  one place a guest write already waits, before it holds a `write_seq`, a
  lock or a charge.
  - Between the soft and the hard mark a token bucket admits bytes at
    `max(r_min, r_mid × x / (1 - x))` with `x = H / soft`, `r_mid =
    max(gc_rate, 16 MiB/s)` and `r_min` = 4 MiB/s: unbounded at the soft
    mark, `gc_rate` in the middle of the band, the minimum near the limit
    (the OpenZFS delay curve in rate form).
  - One request is never delayed longer than `throttle_max_delay_ms`
    (10 s); past that it is admitted if it fits and the bucket carries the
    debt.
  - Never paced: reads; FLUSH (it reserves nothing and never waits for an
    unadmitted write); FUA writes of at most 256 KiB (admitted at once,
    charged to the bucket: the write an fsync waits on); discards and
    write-zeroes (admitted from the reserved region, even on a full volume,
    and not queued behind parked writes); the commit path and GC (they never
    enter the gate).
  - Grants and deadlines run on a timer thread that takes only the gate's
    lock. The ladder keeps its own thread.
- **The full volume:**
  - `on_full = wait` (default): a write that does not fit waits for space,
    FIFO, at most `full_timeout` from its arrival (default **60 s**, the
    dm-thin default; under the kernel's 120 s hung-task threshold). Then it
    fails with `ENOSPC` and the volume **latches full**: the other requests
    waiting for space fail with it and writes fail at once until the headroom
    passes the resume mark. Reads, FLUSH and discards go on; GC goes on.
  - `on_full = error`: fails at once at the limit, as today.
  - There is no `readonly` value: the latch gives the file system the same
    view and opens by itself.
  - The error is `ENOSPC` (`BLK_STS_NOSPC`). `EIO` remains the answer of a
    failed or read-only engine.
  - **Every wait has a deadline**, the journal-ring wait included (`ENOSPC`
    if the last commit failed for space, else `EIO`; at once when the engine
    has stopped).
  - **A policy change re-judges every parked request**: `on_full = error`
    fails them now, a new `full_timeout` counts from each arrival, a larger
    `cache_max` grants and opens the latch.
  - **Restart:** nothing about a parked request is persisted. After a crash
    the reissued requests are admitted from scratch under the policy read at
    open. A quiesce admits every paced request that fits and fails the
    requests waiting for space with `ENOSPC`, so the drain finishes.
  - **Discard frees space:** in the stop and full states the coordinator
    cuts as soon as hole records are journaled and follows with as many
    FULLs as GC's retained set holds (three with `retain_last` = 2), so the
    old extents leave every root; the urgent pass then deletes them after
    the floor grace. About 45 s from discard to space with the defaults.
- **GC's duty.** The engine reads the gate's state at each tick. Outside
  `open` the collector gets four times the step budget, `gc_rate ×
  gc_pressure_rate_factor` (4; uncapped at the limit) and the floor grace
  (10 s + 2 × `read_timeout`) instead of `gc_grace_ms`: the urgent pass's
  concession, per step. `GcBudget` and `GcParams` are per step already, so
  the `Collector` trait does not change. The ladder's urgent pass gets the
  back-off that commits failing for space already have (2 s, doubling to
  60 s while it frees nothing).
- **Observability:** a `throttle` object in the status (state, rate,
  headroom and marks, waiters, delayed writes and time, longest wait,
  `ENOSPC` count, time throttled, GC pressure, ingest and reclaim rates) and
  one log line per state change, plus a 30 s line while throttled (the note,
  §7). The counters live in the gate; the checkpoint is untouched.
- **Configuration:** `volume.json` keeps its keys and its bytes
  (`space.cache_max`, `on_full`, `full_timeout_ms`, `gc_rate` carry the
  per-volume policy). The new keys are daemon-wide, in `lunixd.toml`:
  `throttle`, `throttle_soft_percent`, `throttle_soft_min_bytes`,
  `throttle_soft_max_bytes`, `throttle_min_rate`, `throttle_max_delay_ms`,
  `full_timeout_ms`, `gc_pressure_rate_factor`, `store_max_percent` (the
  note, §8, with defaults and validation).
- **ADR-14's ladder becomes:** pace (continuously, below the limit); at the
  limit evict clean, compact (the urgent pass), then `on_full` with a
  bounded wait and the latch. The "throttle destage" rung stays unused.

**Rejected** (the note, §6, has the trade-offs):
- *Only fixing the hard stop.* Necessary and the first step of this
  decision, but alone it is the SSD write cliff: full speed, then stalls and
  `ENOSPC` on a volume that is merely slow to collect.
- *A token bucket at the measured reclaim rate.* GC reclaims what was
  written `τ` earlier, so the loop feeds on itself with a dead time of a
  minute and winds down. At the hard mark the gate already is this.
- *RocksDB-style backlog triggers with an adaptive rate.* Needs a garbage
  estimate that the mark cannot give fresh (a fast first fill would be
  throttled as garbage), and absolute byte triggers do not scale across
  volume sizes. Its adaptive step is kept as a later refinement.
- *Throttling destage or the cut.* It delays the step that frees journal
  bytes and turns the retained window, and the journal ring then parks
  writes with no pacing at all.
- *Shrinking the queue depth.* Fixed at `ADD_DEV`; `nr_requests` counts
  requests, not bytes. A possible complement later.
- *Foreground GC in the writer's context.* Queue threads never block, and it
  is the known source of stalls in F2FS and SSD firmware.
- *`on_full = readonly`.* Not reversible on a live ublk device.

**Consequences:**
- **`store <= cache_max` + the reserved region + one cut in flight holds by
  admission**, at any write rate. This is the bound the `p1/gcrate` report
  could not give, and the M15 cells can gate on it.
- **The DESIGN §20 write targets are burst targets** unless the volume has
  `R × τ` of garbage room: 300 K 4 KiB IOPS sustained needs about 110 GiB at
  the default grace, about 76 GiB at the floor grace. M16 cells are valid
  only when they were not throttled, and one sustained cell is added.
- **A latency bound replaces an unbounded park:** 10 s of pacing, 60 s at
  the limit, then an error. A full volume under a mounted file system is as
  unpleasant as a full thin pool; it is now prompt, visible and reversible.
- With many large requests in flight the delay cap lets the band be
  overrun; the hard mark, where a byte is admitted when a byte is freed,
  then does the pacing.
- **Format freeze:** no byte of any frozen format changes. One frozen
  *meaning* does (flag F1): a null `space.full_timeout_ms` stops meaning
  "wait for ever" and means "the daemon's `full_timeout_ms`"; the row of
  on-disk-formats.md §21 changes its text, the golden does not. Per-volume
  throttle keys (F2) are deferred because they would change `volume.json`.
- **Interface changes** in `lunix-iface` (not on disk, the lead's sign-off):
  the reservation's class, the snapshot's throttle fields, the status
  message.
- **Phases:** the bounded stop and the discard path (the note's T1 and T2)
  do not depend on `p1/gcrate` and fix a defect that needs a forced device
  deletion today; the band, GC pressure and the status follow `p1/gcrate`'s
  merge. Later: a garbage estimate as a second signal, per-volume keys,
  metadata bypass, a daemon-wide GC budget, and in phase 2 the unsynced
  bytes and the cache tier as further pressure sources of the same gate.

**For the maintainer to decide** (the recommendation first):

| # | question | recommendation | alternative |
|---|---|---|---|
| M1 | The mechanism | the band with a fixed curve, on top of the bounded stop | the bounded stop only, the band later |
| M2 | A default cap for new volumes | `cache_max` = 200 % of the size, written at creation | none: the host file system's floor stays the only limit |
| M3 | A null `full_timeout_ms` (flag F1) | it means the daemon default, 60 s; "for ever" no longer exists | a required number in `volume.json` (a byte change of a frozen file) |
| M4 | The defaults | soft 15 %, minimum rate 4 MiB/s, delay cap 10 s, `full_timeout` 60 s | other values; the sustained M16 cell is the measurement to tune them by |
| M5 | A quiesce while writes wait for space | fail them with `ENOSPC` and finish the drain | abort the process and let `REISSUE` park them again in the successor |
| M6 | The window turn after a discard on a full volume | yes, only in the stop and full states | no: a discard then frees space only as later commits happen |
| M7 | DESIGN §20 and the gates | say "burst" in §20; M16 cells declare their garbage room and are void if throttled; M15 cells set `cache_max` and gate on the store size; one full-volume cell is added | leave §20 as it is and report throttled cells as shortfalls |
| M8 | GC under pressure | floor grace, 4 × budget and rate while writes are slowed | budget and rate only, the configured grace kept |
| M9 | Order of work | T1 and T2 now, on `main`; the rest after `p1/gcrate` | everything after `p1/gcrate` |

*Gate:* the note's §9: the gate's property test (no waiter pending longer
than `full_timeout`), the deterministic engine test, the slow-GC and
replayed-E1 stack tests, the M15 cells under a cap with the full-volume cell,
and the M16 validity rule with the sustained cell.
