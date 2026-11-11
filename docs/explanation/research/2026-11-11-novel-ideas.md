# Novel techniques for Lunix: faster, more resilient, cheaper, standout

> **Corrections (2026-11-11):** several facts in this note were checked against primary sources in [2026-11-11-fact-check.md](2026-11-11-fact-check.md) (ublk flag kernel versions, provider conditional-write support, prices, latencies, LSVD numbers). Where they disagree, the fact-check wins.

Date: 2026-11-11
Author: research agent (Sonnet)
Question: Which novel, clever or distinctive techniques, grounded in literature and existing systems, could make Lunix (ublk + io_uring block device on content-addressed, compressed, optionally encrypted chunks, with a local cache and one or more live-attachable S3 backends, async/sync modes, and a K8s CSI mode) faster, more resilient, cheaper, or give it standout features? For each: mechanism, benefit, cost, risks, prior art, verdict (v1 / later / reject).

Method note: this was written from memory of the literature plus a read of Constellation's ADRs (keyed blake3 chunk ids, XChaCha20-Poly1305, content-addressed trees; see `constellation/docs/explanation/DESIGN.md` around lines 1727-1740 and `DECISIONS.md` ADR-3, ADR-10). No live web verification was done in this pass. Venue/year citations are from memory and prices/latencies are order-of-magnitude. Everything marked UNVERIFIED must be checked before it goes in a design doc as fact.

## Summary

- **The load-bearing idea is a two-tier block map**: a mutable, local, flat LBA-to-extent table on the hot path, and an immutable, fixed-radix **Merkle tree of the map emitted once per epoch** (signed root + monotonic epoch number). That one structure gives O(changed) snapshot diff, instant clones and branches, time-travel mounts, cheap "are these two mirrors identical?" (compare 32 bytes), rsync-style cross-region sync, tamper/rollback detection, and mark-sweep GC with no refcounts. A prolly tree is NOT needed: the LBA keyspace is dense and fixed, so a deterministic fixed-fanout radix tree is simpler and gives the same structural sharing.
- **Decide three on-disk format things in v1 because they cannot be retrofitted**: (1) chunk id = keyed hash under a per-dedup-domain secret (same choice Constellation made, avoids the confirmation-of-file side channel); (2) deterministic, id-derived AEAD so ciphertext is identical across backends; (3) epoch-sealed, Merkle-ized map roots with a signed monotonic epoch.
- **Journal-first, dedup-at-destage.** Acknowledge writes after an append to a local log; hash, zero-detect, dedup, compress and encrypt off the critical path in the seal/destage pipeline. Inline dedup is what kills p99 write latency in VDO/ZFS-style designs. Sync mode alone waits for the pipeline.
- **Epoch-tagged, stall-free crash-consistent cuts** give the async-mode guarantee ("S3 always holds a mountable image as of the last sync point") without freezing I/O. Every FLUSH boundary is a valid restore point, which makes continuous data protection almost free.
- **Log-as-lease fencing**: conditional PUT (If-None-Match) of journal segment N / manifest epoch N is simultaneously the commit protocol and the single-writer fence. This is the same trick as Delta Lake and SlateDB. It makes CSI pod moves safe without a lock service.
- **Warm-start manifest** (access heat map + ordered first-touch trace + successor table) shipped with the volume, plus **pre-copy migration** (warm target while source still serves, then fence, final epoch, promote). This is the single biggest user-visible win for the "pod moves regions" story and is a free replacement for EBS Fast Snapshot Restore.
- **Cache = segmented log with FIFO-family eviction** (S3-FIFO / SIEVE with reinsertion), shared across volumes on a node and keyed by chunk id. Scan-resistant, no random-write amplification on the NVMe, cheap whole-segment TRIM eviction.
- **Dedup fragmentation is the known killer of hash-addressed block stores** (Lillibridge et al., FAST 2013). Design packs for locality from day one (write-order packing, ranged GET coalescing) and plan LBA-clustered recompaction.
- **Hedged GETs (single backend, p95-triggered) and AIMD request concurrency are v1**; cross-backend hedging only between backends that are "near" (egress-free); erasure coding across clouds is rejected for v1 because egress cost dominates.
- **Cheap-to-add differentiators**: sampled miss-ratio-curve (SHARDS) telemetry and a `lunix advise` cost advisor; Intelligent-Tiering pack storage class; recompress-on-seal (lz4 local, zstd on upload); signed roots plus optional Object Lock for ransomware-proof history.

## 1. Architecture-level ideas

### 1.1 Two-tier block map: mutable local table + Merkle-ized epoch roots  (VERDICT: v1)

Mechanism:
- Hot path uses a flat, extent-based L2P table (LBA range to chunk id / local slot; zero runs are holes). It is updated by the single writer pipeline and is never hashed into a tree on the write path.
- At each epoch seal (async interval, or each FLUSH in sync mode), dirty leaf pages are serialized and the tree path to the root is rewritten. Tree: fixed fanout (e.g. 256-1024 entries of 32 B hash per node), depth 3-4 for multi-TB volumes, node id = hash of content, nodes are immutable objects. Root + epoch + parent root + timestamp form a signed "commit" object.
- Since shape is a pure function of LBA, two independent builders produce identical roots from identical content. Define the **volume content digest** over plaintext chunk ids (keyed) rather than ciphertext object names, so root equality means "mirrors hold the same logical volume" regardless of pack layout.

Benefit:
- Snapshot diff = parallel descent skipping equal subtree hashes: cost O(changed leaves x depth), not O(volume).
- Clone/branch = copy a 32-byte root. Time travel = keep old roots. Verify two backends identical = compare roots, then (optionally) spot-check packs.
- Epoch write amplification: with fanout 512 and a 1 TB volume at 16 KB extents (~64M leaves) depth is ~3; one epoch rewriting K dirty leaves writes ~K leaf pages plus a handful of upper nodes. Upper levels are tiny and shared.

Cost/complexity: moderate. Need a node cache, a compact node codec, and GC (mark-sweep from retained roots; see 1.7). Memory: 1 TB at 4 KB blocks with 32 B ids is 8 GB of raw ids; that is why the map unit should be an extent (16-64 KB) or run-length encoded, and why the local table stores an 8 B slot index with the hash held in the node/cold store.

Risks: epoch frequency vs map-object count (PUT cost); tree churn for random-write workloads (every epoch touches many leaves). Mitigate by merging epochs when idle and by journaling deltas (log of map changes between full leaf rewrites, i.e. a mini LSM of map deltas) if leaf rewrite amplification is measured to be high.

Prior art: Venti (FAST 2002, hash-tree archival storage), ZFS block-pointer Merkle tree, Btrfs, git/restic/Kopia/bup/casync, Dolt (prolly trees), Ceph RBD object-map + fast-diff, Neon (immutable layer files + branching on S3), Iceberg/Delta snapshot-metadata model, Cumulus (FAST 2009, backup to S3). Merkle search trees (Auvolat and Taiani 2019) and prolly trees are for sparse, variable keys; not needed here.

Note on "prolly vs radix": a prolly tree gives history independence for arbitrary key insertions. LBA keys never shift, so a radix tree already has history independence. Reject prolly for Lunix; record this so nobody re-litigates.

### 1.2 Journal-first write path; hash/dedup/compress/encrypt at destage  (VERDICT: v1)

Mechanism: ublk request -> append to local log segment (O_DIRECT, io_uring, batched submission) -> update dirty-extent map -> complete. A destage pipeline (worker threads, off the io_uring completion path) hashes (keyed blake3), detects zero/constant blocks, dedups against the index, compresses, encrypts and packs into pack objects, then seals epochs. Reads check dirty-extent map first (journal), then cache, then backend.

Benefit: write latency becomes one NVMe append (tens of microseconds) regardless of hash/compress cost; CPU work is amortized in batches and can use SIMD blake3 over larger buffers and `zstd` multi-block contexts. Matches the "post-process dedup" family (NetApp, Windows Server Dedup) rather than inline (VDO, ZFS) but with the journal ahead of it so there is no space-bloat window problem.

Cost: read-your-write via the dirty map; journal space accounting; free-space floor must count the journal.

Risks: overwrite-heavy workloads can destage data that is overwritten again before upload; mitigate by delaying destage by the async-interval and coalescing (the "write absorb" is a cost win: S3 sees only the last version per extent per epoch).

Prior art: LSVD (Hajkazemi et al., EuroSys 2022, log-structured virtual disk over S3 with a local write log and cache), Ceph RBD journaling, Aurora "the log is the database" (SIGMOD 2017) for the principle, VDO/UDS for the inline contrast.

Benefit quantification (UNVERIFIED, back-of-envelope): blake3 on one AVX2/AVX-512 core hashes a few GB/s on large inputs, but 4 KB-at-a-time on the critical path adds ~1-2 microseconds plus cache misses; at 500K-1M IOPS that is a full core. Moving it off-path keeps the io_uring/ublk threads for I/O only.

### 1.3 Epoch-tagged, stall-free crash-consistent cuts  (VERDICT: v1)

Mechanism: every write is tagged at admission with the current epoch number `e` (an atomic counter). To seal epoch `e`, bump the counter to `e+1` (new writes now go in `e+1`), then wait only for in-flight writes already tagged <= `e` to finish journaling. The sealed set = all writes tagged <= `e`. Because the tag is assigned at admission and the counter is monotone, any write submitted after another write was acknowledged gets a tag >= that write's tag. Therefore the sealed cut contains every write that was acknowledged before the seal, plus possibly some in-flight ones: precisely a legal power-loss image for any filesystem (FS correctness already tolerates any subset of un-flushed writes, plus all writes completed before the last FLUSH/FUA). Overwrites of the same extent in epoch `e+1` must not leak into the epoch-`e` map (MVCC on the dirty-extent map, or per-epoch delta maps merged at seal).

Benefit: no quiesce, no `fsfreeze`, no application cooperation; seal latency is bounded by the slowest in-flight journal append. Every FLUSH completion can name an epoch, so "restore to the last completed FLUSH" is well defined (continuous data protection with FLUSH granularity).

Risks: correctness of the "tag at admission" invariant under multi-queue ublk (different queue threads, same counter: use `fetch_add`/`load(Acquire)` carefully); FUA/flush ordering; discard interplay. Needs a model-checking/fuzz harness (crash at every point, verify with fsck + a golden FS test workload). Prior art: Ceph RBD journal-based mirroring ("crash-consistent" prefix), Hyper-V/Zerto-style CDP journals, BarrierFS/"order-preserving block layer" (Won et al., FAST 2018) and optimistic crash consistency (Chidambaram et al., SOSP 2013) for the ordering/durability separation.

### 1.4 Log-as-lease fencing with conditional writes  (VERDICT: v1 for backends that support it)

Mechanism: journal segment `N` and manifest epoch `N` are written with `If-None-Match: *`. A second writer that tries to continue the same sequence gets 412 and knows it has been fenced. A graceful handoff (CSI pod move) writes a `handoff` record naming the new writer id and last epoch; the new writer's first segment CASes on that. Stale-writer protection: every PUT carries the writer epoch in the key or object metadata; readers ignore anything from a lower writer generation.

Benefit: no external lock service, no clock assumptions; safety when a node is partitioned but still thinks it owns the volume. This is the single most important resilience property for RWO volumes that move.

Cost: requires conditional writes on every backend (AWS S3: If-None-Match Aug 2024, If-Match Nov 2024; GCS preconditions on generation; Azure Blob ETag If-Match; MinIO and Ceph RGW support; Cloudflare R2 conditional PUT; Backblaze B2 UNVERIFIED). A backend without CAS is "second class": allowed only as a non-fencing mirror, fenced through the primary backend.

Prior art: Delta Lake log protocol on S3, SlateDB (Rust LSM on object storage with manifest CAS + writer epochs), Iceberg commit via catalog CAS, Neon safekeepers, WarpStream/Turbopuffer style object-store-as-coordination. Verdict on the brief's open question: **require CAS on the primary (fencing) backend, make it optional on mirrors.**

### 1.5 Keyed chunk ids, deterministic AEAD, per-domain dedup  (VERDICT: v1, format-critical)

- Chunk id = `blake3::keyed_hash(domain_key, plaintext)`. Hash of the uncompressed plaintext, same as Constellation ADR-10, so the id is independent of compression level (enables recompress-on-seal, 4.3).
- Encrypt with an AEAD whose nonce is derived deterministically from the chunk id (SIV-style: `nonce = KDF(domain_key, id)`); because the id commits to the plaintext, the same id never encrypts two different plaintexts, so determinism is safe and ciphertext is identical across backends and across volumes in the same domain. The AEAD tag doubles as the integrity check on read, saving a separate hash of every cache hit.
- Dedup domain = tenant/volume family. Cross-tenant dedup is OFF: the Harnik et al. (2010) side channel and convergent-encryption confirmation-of-file attacks (Douceur et al. 2002; Bellare et al. message-locked encryption, 2013; DupLESS, USENIX Security 2013) are exactly what a shared global index exposes. Sharing happens through **explicit lineage**: a golden base volume (read-only root) that clones reference; clone holders receive a wrapped base-domain key. That yields most real-world dedup wins (VM/CI/golden images) with no side channel.
- Unencrypted volumes may use an unkeyed hash and opt into wider dedup domains; make this a volume-creation flag, not a later migration.

Benefit: safe multi-tenant dedup; self-healing and P2P (verification needs only the domain key); node-wide shared cache across volumes in a domain (1000 CI pods cloned from one golden image fill the cache once).

Risks: key rotation (chunk ids depend on the key; plan a domain key version in the id namespace or accept rewrap-only rotation for the DEK and never rotate the addressing key). Keep this aligned with Constellation's "addressing key" so the Replicix tooling is shared.

## 2. Latency, throughput and tail ideas

### 2.1 Hedged / tied GETs; adaptive concurrency  (VERDICT: v1 single-backend hedge, v1 AIMD, later cross-backend)

- Single bucket: issue a duplicate GET when the first has not returned by the observed p95 (per backend, per request-size bucket), cancel the loser. Dean and Barroso, "The Tail at Scale" (CACM 2013), report big p99.9 reductions for ~2% extra load; for S3, the extra request cost is negligible (GET about $0.0004 per 1000, UNVERIFIED current). AWS's own guidance and the AWS CRT S3 client do the same thing (retry slow requests at a latency percentile). Expect p99 first-byte to move from the hundreds of ms toward 2-3x p50 (UNVERIFIED, measure).
- Cross-backend hedging is only free between backends that are egress-free relative to the node (same cloud and region, or same data center). A cross-region hedge pays egress on the winner or a cancelled-but-started transfer; restrict by a per-backend `hedge_cost_class`. Backend ordering by EWMA latency plus price gives "read from nearest healthy replica" for free.
- AIMD concurrency controller per backend (Netflix concurrency-limits/Vegas-gradient style): increase while latency is flat, back off multiplicatively on 503 SlowDown or latency inflation. Prevents both underusing a 25-100 Gbps NIC and tripping prefix throttling (S3 per-prefix ~5,500 GET/s, 3,500 PUT/s, UNVERIFIED current numbers). Cheap and robust.

### 2.2 Fault granularity larger than 4 KB; ranged-GET coalescing  (VERDICT: v1)

A cache miss should fetch an aligned **extent** (64 KB-1 MB, tunable per volume), not a 4 KB chunk, and adjacent misses within a pack coalesce into one ranged GET. S3 charges per request, not per byte, so one 1 MB range costs the same as one 4 KB range; latency is dominated by the ~15-40 ms first byte. This is a natural readahead that does not depend on the guest FS, and is why Slacker (Harter et al., FAST 2016) found that lazy fetch with locality works: containers need only about 6% of an image's bytes at start. Pair with write-order packing so logically adjacent data is physically adjacent (see 3.1).

### 2.3 Flush coalescing and pipelined group commit  (VERDICT: v1)

Mechanism: classic group commit (DeWitt et al., 1984) with a leader/follower scheme. FLUSHes that arrive while an epoch seal (local fdatasync or S3 PUT) is in flight join the next batch; multiple seals can be pipelined (concurrent segment PUTs, in-order publish of the epoch commit). An adaptive Nagle-style micro-delay (tens to hundreds of microseconds locally, up to ~1-2 ms for S3 mode) widens batches only while the arrival rate suggests it will help.

Benefit: sync-mode throughput stops being 1/PUT-latency. A serial committer still pays one S3 PUT (~20-50 ms Standard small-object, UNVERIFIED) per fsync, but a database with N concurrent committers pays it once per batch. Expect sync mode to be tolerable for group-committing workloads (Postgres, MySQL, etcd with batching) and still slow for single-thread fsync loops: say so in docs.

Prior art: InnoDB/Postgres group commit, BarrierFS (FAST 2018), Aurora's batched log writes.

### 2.4 WAL tier on a low-latency store (S3 Express One Zone and equivalents)  (VERDICT: later; design the Backend trait for it now)

Mechanism: in sync mode, the journal segment PUT goes to a fast tier (S3 Express One Zone: single-digit ms; supports append since Nov 2024; GCS Rapid/zonal buckets, Azure premium or append blobs: UNVERIFIED), and a background compactor writes consolidated packs to the Standard bucket (the durable, multi-AZ tier). The fast tier is single-AZ, so sync-with-Express gives zero-RPO against node loss and bounded exposure to whole-AZ loss until compaction catches up (seconds to a minute). Present it honestly as a third durability level: `sync-multiaz`, `sync-fast-wal`, `async`.

Benefit: fsync latency from ~20-50 ms to ~5-10 ms (UNVERIFIED), i.e. 3-5x; at 5-10 ms a lot more workloads accept sync mode. Cost: Express storage is ~8x Standard per GB (UNVERIFIED) but WAL segments live for seconds-minutes, so the dominant cost is request count; batch with 2.3.

Risks: vendor-specific features; compaction ordering; the AZ-loss window. Alternatives not recommended for v1: quorum WAL on sibling nodes (Aurora/Neon safekeeper style) — much more distributed-systems surface than a block daemon should own.

### 2.5 Stride and successor-table prefetch  (VERDICT: later; small and bounded)

The guest page cache already does sequential readahead, so block-level prefetch must not double up. Useful, cheap additions: (a) majority-trend stride detection (Leap, ATC 2020; O(1)/access) at extent granularity to run ahead of guest readahead on large sequential scans through cold regions; (b) an online **successor table** (Markov/correlation prefetching, Joseph and Grunwald ISCA 1997; Mithril, Yang et al. SoCC 2017 for association mining): for extent A remember the last 1-2 extents accessed after A, persisted in the warm manifest (4.1). Deterministic boot/startup sequences make this surprisingly effective; random DB access gets nothing. Budget prefetch bytes as a fraction of demand and drop on backend pressure. Do not ship ML. Mithril-style full association mining: later or reject unless traces show a win.

### 2.6 Cache layout: segmented log, FIFO-family eviction, node-wide sharing  (VERDICT: v1)

Mechanism: cache is an append-only segmented log (CacheLib Navy / Kangaroo SOSP 2021 / FairyWREN OSDI 2024 style): admitted extents are appended into the open segment; eviction frees the oldest segment wholesale (TRIM/discard of the file region or `fallocate(PUNCH_HOLE)`), with S3-FIFO (SOSP 2023) or SIEVE (NSDI 2024) style lazy promotion and quick demotion, plus **reinsertion** of still-hot extents when their segment is reclaimed. Index by chunk id so the cache is shared by every volume on the node in the same dedup domain.

Benefit: sequential-only writes to the NVMe (device write amplification near 1, long flash life), O(1) eviction work, scan resistance (backup/fsck/rsync scans over a block device are the classic LRU killer), no per-hit metadata writes. Consider NVMe FDP / write hints (UNVERIFIED availability for the host) to place cache segments separately from the journal.

Cost-aware extension (later): Greedy-Dual-Size / Hyperbolic caching (ATC 2017) weighs "S3 GET cost + latency to refetch" against bytes held. In practice backend cost is nearly uniform per request, so large extents have a cheaper refetch per byte; the effect is modest. Do FIFO-family first, measure, then decide.

Optional **raw hot class** (later): keep a subset of hot extents uncompressed and 4 KB-aligned in the cache so reads can use ublk zero-copy (UBLK_F_SUPPORT_ZERO_COPY / auto buffer registration, kernel 6.15+, UNVERIFIED exact flag names/versions) straight from the cache file into the request pages with no memcpy and no decompress. Compressed class remains the default for capacity.

## 3. Storage-efficiency and cost ideas

### 3.1 Locality-preserving packs and anti-fragmentation compaction  (VERDICT: v1 layout, later compactor)

Hash-addressed, deduplicated storage scatters logically sequential data across many containers over time: dedup fragmentation. Lillibridge et al. (FAST 2013) show restore speed collapsing and propose container capping and a forward assembly area; Zhu et al. (Data Domain, FAST 2008) introduced stream-informed segment layout and locality-preserved caching. Lunix mapping:
- Pack chunks in **write order within an epoch** (stream-informed layout), 4-16 MB packs, each with an index/footer enabling ranged GETs, so sequential logical reads become few GETs.
- Compactor (mark-sweep from retained roots): rewrite packs below a live-ratio threshold, and optionally **cluster by LBA and heat** when repacking, so cold logically adjacent data ends up physically adjacent. This is also the LFS cleaner hot/cold-separation idea (cost-benefit cleaning).
- Cap dedup against old packs: if a new write's chunks would reference more than K distinct old packs for a contiguous LBA run, store a fresh copy instead (container capping). Trade a little space for bounded read fan-out.
- Local dedup index memory: Data Domain "summary vector" (Bloom filter) avoids index I/O for new chunks. Use a small Bloom/cuckoo/ribbon filter for the LOCAL index only; VDO's UDS gets about 1 GB RAM per TB with a dense index (UNVERIFIED), so size the budget accordingly and prefer 16-64 KB dedup units over 4 KB.

### 3.2 Compression policy: lz4 locally, zstd on upload, entropy sniffing, per-volume dictionaries  (VERDICT: v1 recompress-on-seal and entropy skip; later dictionaries)

- **Recompress on tier move.** Because chunk id hashes plaintext, the compressed form is free to change. Journal/cache holds lz4 or raw (fast, CPU-cheap); the destager recompresses with zstd at a level chosen from observed CPU headroom when sealing a pack for S3 (uplink bytes and S3 storage are the paid resources; local CPU time is the free one). Adaptive level like `zstd --adapt` (adjust per batch from queue depth and core utilization).
- **Entropy sniffing**: sample a block (btrfs's heuristic approach: byte-histogram entropy / repeated-pattern check) and skip compression for incompressible data (encrypted, already-compressed media). Saves CPU and avoids 0-gain expansions.
- **Trained dictionaries per volume** (zstd `--train`): for 4-16 KB units, dictionaries commonly give large gains on structured data (Facebook reports multi-x on small records; UNVERIFIED for filesystem blocks). With 64 KB+ units the benefit shrinks. Train after the first N MB written, store as a versioned object in the volume metadata, record dictionary id per pack. Verdict: later; first measure compression ratio on realistic ext4/xfs/Postgres images at 16 KB vs 64 KB.

### 3.3 Intelligent-Tiering for packs, price-aware operations  (VERDICT: v1, nearly free)

Write pack objects with `StorageClass=INTELLIGENT_TIERING` on AWS: no retrieval fees, automatic drop to Infrequent/Archive-Instant tiers for cold packs (roughly -40% / -68% storage price, UNVERIFIED current), monitoring fee per 1000 objects is trivial at 8 MB packs (1 TB = ~130K objects). Journal segments and roots stay in Standard. Large packs also cut PUT cost (PUT ~ $0.005/1000: 1 TB as 1 MB objects = ~$5 once, as 8 MB packs under $1). Maintain a per-backend "price card" (storage, GET, PUT, egress) used by the cost advisor, sync scheduler and hedge policy.

### 3.4 Erasure coding across clouds  (VERDICT: reject for v1; maybe later for cold history)

RACS (SoCC 2010) and DepSky (EuroSys 2011) did this to avoid vendor lock-in and tolerate provider failure. For Lunix the arithmetic is bad: a 2+1 code stores 1.5x instead of 2x, but every cold read must fetch k shards, half of which cross a cloud boundary at ~$0.05-0.09/GB egress, and latency becomes max of two cloud RTTs. Mirrors keep a full local-to-region replica, which is the point of the region-mobility use case. S3 already erasure-codes within a region. Reconsider only for **cold historical packs** (snapshot history) where reads are rare and the storage saving compounds. Intra-vendor multi-region EC is a possible far-future cost lever.

### 3.5 Honoring TRIM/discard and optional FS-aware hints  (VERDICT: v1 discard; later FS hints)

- Discard: ublk exposes DISCARD/WRITE_ZEROES. Map it to "extent becomes a hole" in the map (instant, no data written), frees local space immediately, and makes the chunk collectable by the next GC. Pair with a mount-time `fstrim` recommendation / periodic trim in the CSI node plugin because ext4/xfs do not discard by default. Caveat: with retained history (time travel) a discard does not free S3 bytes until the root ages out; document it.
- **Semantic block hints** (semantically-smart disks, Sivathanu et al., FAST 2003; "Life or death at block level", OSDI 2004; type-safe disks, OSDI 2006): parse ext4/xfs on-disk superblock, group descriptors, inode tables and journal location to (a) pin metadata extents in cache, (b) skip compression on journal areas that churn, (c) prefetch inode tables at mount (a deterministic, FS-specific warm set that works even with no prior trace). Hard rule: **hints only, never correctness**. Parsing on-disk bitmaps to infer free blocks (to avoid uploading freed data without TRIM) is unsafe because on-disk state lags the FS journal. v1: none; later: ext4/xfs metadata pinning as an optional plugin with strict versioned superblock detection and fail-open behavior.

## 4. Migration, replication and mobility ideas

### 4.1 Warm-start manifest (heat map + ordered trace + successor table)  (VERDICT: v1)

Mechanism: the daemon maintains a compact per-volume access summary: 2-4 bit decayed heat counter per extent (1 TB at 64 KB extents = 16M extents = 8-16 MB), plus an **ordered first-touch list** for the post-attach window (the "boot trace", as in Windows Prefetch/SuperFetch and Linux readahead/ureadahead), plus the successor table from 2.5. It is published into the volume metadata with each Nth epoch (small object, not part of the data path). On attach to a new node, a prefetcher replays the list in order with a budget and bounded concurrency, demand reads taking priority.

Benefit: the wait after a move drops from "random 20-50 ms faults all over the working set" to "working set streams in at network line rate". Order of magnitude (UNVERIFIED): a 1 TB DB volume with a 5% hot set = 50 GB; at 1-2 GB/s aggregate parallel GET throughput that is 25-50 s, versus hours of demand-fault warm-up. It replaces EBS Fast Snapshot Restore (billed per snapshot per AZ per hour) with a free, always-on equivalent. Evidence base: Slacker (6.4% of image data read at startup), eStargz prioritized files, Nydus, SOCI, Alibaba DADI (ATC 2020) trace-based prefetch, AWS FSR.

Cost: tiny. Risks: stale traces after workload changes (decay + versioned manifest), privacy (an access pattern is metadata; encrypt it like everything else), prefetch stampede on the backend (budgeted by 2.1 AIMD).

### 4.2 Pre-copy migration with fenced handoff (CSI)  (VERDICT: v1 protocol; v1.x polish)

Like live VM migration (Clark et al., NSDI 2005): (1) controller signals "migration intent"; target node attaches the volume READ-ONLY (no writer lease), pulls the manifest and warms the cache while the source keeps serving; (2) source seals a final epoch, writes a `handoff(new_writer)` record via CAS, and stops accepting I/O; (3) target takes the lease by CASing the next segment and starts serving as writer. Downtime = one final epoch PUT + one CAS (low hundreds of ms typically, UNVERIFIED) with a warm cache. If the source dies mid-way the target recovers from the last sealed epoch (same as any crash) and the old writer is fenced anyway (1.4). In async mode, offer "drain-then-handoff" (wait for sync) vs "force-handoff" (accept RPO of the unsealed tail).

### 4.3 Peer-assisted warm-up as just another read source  (VERDICT: later; hook now)

Because chunks are content-addressed and AEAD-verified, the old node can serve its cache to the new node over TCP/QUIC with no trust: the receiver checks the tag/hash and discards bad data. Make the read path abstract over `ChunkSource` (cache, journal, backend, peer) in v1 and ship the peer source later. Value is highest when (a) the new region's bucket is still being seeded (hot data is not there yet) or (b) there is no S3 round trip at all within a cluster. For dirty write-back tail, flush to S3 first (simple, safe) rather than shipping it peer to peer in v1. Egress between regions is billed the same whether peer or bucket-to-bucket, so peer is a latency win, not a cost win.

### 4.4 Rsync-style cross-backend / cross-region sync using the Merkle root  (VERDICT: v1)

Protocol: source sends (root, epoch). Target (or the daemon acting for it) descends: for each node hash, "do I have it?" (a manifest/inventory lookup, not a per-object HEAD) and only the missing subtrees and referenced packs are transferred. Because packs are shared by many chunks, transfer whole packs when utilization is above a threshold and re-pack only sparse ones. This is git push negotiation / casync / restic copy / Dynamo-style Merkle anti-entropy applied to a volume.

Cost levers:
- **Same-vendor server-side copy** (`CopyObject` / `UploadPartCopy`) between buckets moves no data through the daemon; still billed as inter-region transfer.
- **Egress-aware source selection** when several backends hold the data: pick the source by price matrix (same-cloud same-region = free; cloud to Cloudflare R2 = zero egress charge from R2; Bandwidth Alliance pairs). A neutral low-egress hub (R2/B2) used as the cross-cloud transit bucket can cost less than direct cloud-to-cloud hops for repeated moves (UNVERIFIED current prices; make it a price-card computation, not a hard-coded rule).
- **Send compressed+encrypted packs as is**; never re-encode in transit.
- Bandwidth throttle with a $/GB budget, not just a rate limit.

Verification: after sync, compare roots; periodic cheap verify by comparing pack lists and object checksums (below).

### 4.5 Zero-downtime daemon upgrade via ublk USER_RECOVERY  (VERDICT: v1.x)

Use `UBLK_F_USER_RECOVERY` (and `UBLK_F_USER_RECOVERY_REISSUE` so inflight I/O is requeued, not failed) so the block device and its mounted filesystem survive daemon death or restart: the new daemon issues START_USER_RECOVERY, re-attaches queues and END_USER_RECOVERY. Make this the standard upgrade path (and crash path), with the daemon's durable state (journal, dirty-extent map reconstructable from journal replay) designed for fast restart. Window: I/O stalls, it does not error, for the restart time, so keep restart under the guest's timeouts (seconds). Also relevant newer features, version availability UNVERIFIED: `UBLK_F_QUIESCE`, `UBLK_F_UPDATE_SIZE` (live grow notification, useful for volume grow), `UBLK_F_NEED_GET_DATA`, `UBLK_F_AUTO_BUF_REG`, batch I/O command support. Another agent covers ublk detail; the Lunix-specific point is that recovery must not depend on in-memory-only state.

## 5. Integrity, resilience and standout-feature ideas

### 5.1 End-to-end verification, signed roots, rollback detection  (VERDICT: v1 verify-on-fill and signed root; later scrubber)

- Verify on every backend fill (AEAD tag, or hash). Cache hits are covered by the AEAD tag stored with the cached ciphertext or by a per-segment checksum (crc32c/xxh3 is enough for local bit-rot). Sample-verify with a rate knob for unencrypted volumes.
- **Signed commit objects** (Ed25519 or MAC under the volume key) with a monotonic epoch. A hostile or buggy bucket replaying an old root is detected when the local daemon, or a quorum of mirrors, knows a higher epoch. This turns S3 from "trusted" to "verified" storage: ZFS-grade end-to-end integrity, over object storage.
- **Self-healing**: a failed verification on backend A is retried from backend B (hedge target) and the good copy rewritten to A.
- **Cheap scrub** (later): store a full-object checksum on each pack (S3 supports additional checksums like CRC64NVME, SHA-256, retrievable via HeadObject / `GetObjectAttributes` without downloading, UNVERIFIED current feature set). A scrub then LISTs packs and compares checksums + sizes against the manifest with no GET egress; deep scrub (re-read, re-hash) is rate-limited and rare. Catches: operator deleted objects, lifecycle rule accidents, buggy sync, silent object loss.
- **Optional S3 Object Lock / versioning on roots and journal** (WORM): ransomware-proof history. Combined with time travel, a compromised host cannot destroy the ability to mount last night's volume.

### 5.2 Time-travel mount, branches, instant clones  (VERDICT: format v1, CLI v1.x)

`lunix mount --at <timestamp|epoch|FLUSH-seq> --ro` creates a second ublk device over an old root; `lunix branch` creates a writable CoW child (new writer lineage, shared immutable history, own journal). Costs: nearly zero data movement. Use cases: forensic/backup restore, "fork my production DB volume for a test in 2 seconds" (Neon branching, Aurora fast clones, ZFS clone), CI fixtures, rollback after bad migration. Retention policy: keep all roots for T hours, thin to hourly/daily afterwards (GFS-style), GC by mark-sweep; discard of data older than retention is the only thing that frees space. `lunix export` to raw/qcow2 avoids lock-in. For restore granularity beyond epochs, retain journal segments (CDP): any completed-FLUSH boundary is a valid crash-consistent image (1.3).

### 5.3 Sampled miss-ratio curves and a cost advisor  (VERDICT: v1.x, cheap, differentiating)

SHARDS (Waldspurger et al., FAST 2015; also Counter Stacks, OSDI 2014) builds an accurate miss-ratio curve online from a spatially hashed sample (1% sample, tens of KB of memory). Lunix can expose: "with +50 GB of cache your hit ratio goes 71% to 88%, saving $X/month in GETs and Y ms mean latency", and the CSI layer can size cache automatically or feed autoscaling. Also gives per-volume chunk-size / extent-size recommendation from sampled trace replay (what-if on fault granularity). Nothing in ZeroFS, LSVD or Longhorn does this.

### 5.4 Thin initial sync and attach-time import  (VERDICT: v1)

First sync after attaching a backend should skip holes (zero runs, discarded ranges), upload mapped extents largest-first by heat (hot data first, so a failover during initial sync loses the least useful cold tail), and commit a root only after the whole map is covered (or commit an "incomplete seed" marker root that is explicitly not mountable elsewhere). Combine with 4.4 for adding a second region.

### 5.5 Other micro-ideas (kept short)

- Constant-fill detection (not only all-zero): dedup already handles it, but a pre-hash `memchr`-style check avoids hashing/compression entirely on mkfs/lazy-init patterns. v1 (trivial).
- Volume grow: extend the map and call ublk update-size (UBLK_F_UPDATE_SIZE, UNVERIFIED); shrink requires FS shrink first, then discard of the tail. Document, do not automate.
- Journal-wrap safety: when the local journal is full and S3 is slow, apply configured backpressure policy (block vs EIO) at the journal, not at the device size.

## 6. Ideas considered and discarded

| Idea | Why not |
|---|---|
| Content-defined chunking (FastCDC, ATC 2016) for the volume | Block devices are block-aligned; fixed 16-64 KB extents get nearly the same dedup without the CPU and variable-offset map. CDC helps shifted-byte file backup, not block images. |
| Prolly tree for the block map | LBA keys are dense and fixed; fixed-radix Merkle tree already gives deterministic shape and structural sharing. |
| Global cross-tenant dedup | Harnik side channel; convergent encryption confirmation attacks. Use explicit lineage instead (1.5). |
| Erasure coding across clouds (v1) | Egress cost and cross-cloud latency per read (3.4). |
| Bloom/cuckoo filters per backend for "chunk exists on X" | The Merkle diff plus a per-backend pack manifest answers this exactly; filters would add false positives to a correctness path. Keep filters local-index-only. |
| Quorum WAL across sibling nodes | Large distributed-systems surface; Express-tier WAL gets most of the latency win with an object store API. |
| Full ML-based prefetch / learned eviction | Guest FS readahead already covers the sequential case; boot/startup replay and a successor table cover the deterministic case; remaining random access is not predictable. |
| Inferring free blocks from on-disk ext4/xfs bitmaps to skip uploads | On-disk bitmaps lag the FS journal; unsafe. Use TRIM. |

## 7. Ranked top 10 (impact x feasibility)

| Rank | Idea | Impact | Feasibility | Verdict |
|---|---|---|---|---|
| 1 | Journal-first write path with destage-time dedup/compress/encrypt (1.2) | very high | high | v1 |
| 2 | Fixed-radix Merkle map with signed epoch roots; clone/diff/verify (1.1, 5.1) | very high | medium | v1 |
| 3 | Epoch-tagged stall-free crash-consistent cuts (1.3) | very high | medium (needs fuzzing) | v1 |
| 4 | Log-as-lease fencing via conditional PUT (1.4) | very high for CSI safety | high | v1 |
| 5 | Warm-start manifest + pre-copy migration (4.1, 4.2) | very high for UX | high | v1 |
| 6 | Keyed chunk ids, deterministic AEAD, lineage-based sharing (1.5) | high (format-critical) | high | v1 |
| 7 | Segmented-log cache with S3-FIFO/SIEVE + extent fault granularity + ranged GET coalescing (2.2, 2.6) | high | high | v1 |
| 8 | Hedged GETs + AIMD concurrency + flush coalescing (2.1, 2.3) | high (tails, sync mode) | high | v1 |
| 9 | Rsync-style Merkle sync + egress-aware source selection + Intelligent-Tiering (4.4, 3.3) | high (cost) | medium | v1 / v1.x |
| 10 | Time-travel mount / branches / CDP (5.2) | high (feature) | medium (rides on #2, #3) | format v1, CLI v1.x |

Near misses: zero-downtime upgrade via USER_RECOVERY (4.5, v1.x, low effort, high operational value), SHARDS cost advisor (5.3), recompress-on-seal (3.2), locality-aware compaction (3.1), Express-tier WAL (2.4), peer warm-up (4.3).

## 8. Signature features pitch (vs ZeroFS, LSVD, Longhorn, EBS)

1. **"Move a pod across regions and clouds with a warm cache and sub-second handoff."** Pre-copy migration, warm-start manifest, fenced handoff, rsync-style Merkle replication with price-aware routing. Longhorn and EBS cannot leave their cluster or AZ; ZeroFS and LSVD have a cold start on every move.
2. **"Every flush is a restore point; mount your volume as it was at any moment."** Time-travel mounts, instant branches (2-second copy of a 1 TB database volume), and signed history that a ransomware-compromised host cannot rewrite (Object Lock). EBS snapshots are coarse and slow to restore; Longhorn snapshots are cluster-local.
3. **"Verified storage on dumb object stores."** End-to-end signed Merkle roots, rollback detection, self-healing from a mirror, cheap checksum-based scrub, and one 32-byte comparison to prove two mirrors identical. None of the competitors offers cross-vendor mirror verification.
4. **"Your own cost and latency advisor."** Online miss-ratio curves, price-card aware cache sizing, tiered storage classes, egress-aware sync. Turns "S3-backed volume" from a cost risk into a dial with numbers.
5. **"Upgrade the daemon without unmounting anything"** (USER_RECOVERY) plus crash-consistent async mode with a stated, measurable RPO and an honest durability ladder (`async` / `sync-fast-wal` / `sync`), instead of a single "fsync is slow" mode.

## Recommendations for Lunix

1. Lock the format decisions now: keyed-hash chunk ids per dedup domain, id-derived deterministic AEAD, extent-based map with hole runs, Merkle-ized epoch roots with signed monotonic epoch and writer generation, packs with a footer index and write-order layout.
2. Architect as journal -> dirty-extent map -> destage pipeline -> pack/seal. Keep dedup/compress/encrypt out of the ublk completion path. Sync mode waits for seal; async mode does not.
3. Treat conditional PUT as the fencing mechanism on the primary backend; allow CAS-less backends as non-fencing mirrors only.
4. Build the `ChunkSource` abstraction (cache, journal, backend, peer, later) and the price-card/latency EWMA per backend early; hedging, source selection and the advisor all hang off them.
5. Put a crash/fuzz harness (fault injection at every journal/seal/CAS step + fsck-verified golden workloads) on the critical path of v1; the epoch-cut invariant (1.3) is the highest-risk item in this list.
6. Do the cheap cost wins immediately: Intelligent-Tiering packs, large packs, ranged-GET coalescing, recompress-on-seal.
7. Defer: Express-tier WAL, peer warm-up, dictionaries, FS-aware hints, locality compactor, scrubber, erasure coding. Reject: cross-tenant global dedup, prolly trees, CDC, ML prefetch.

## Open questions / UNVERIFIED

- All prices, latencies and request-rate limits quoted (S3 Standard/Express GET/PUT cost and latency, Intelligent-Tiering rates and savings, prefix throughput limits, Express storage multiple, R2/B2 egress arrangements) are from memory and must be re-checked against current vendor pages.
- Backend CAS support matrix (If-None-Match / If-Match on S3, GCS, Azure, R2, B2, MinIO, Ceph RGW, Wasabi): only AWS, GCS, Azure, MinIO behavior is reasonably confident; R2, B2, RGW need confirmation.
- Exact kernel versions and flag names for ublk zero-copy (`UBLK_F_SUPPORT_ZERO_COPY`, `UBLK_F_AUTO_BUF_REG`), `UBLK_F_UPDATE_SIZE`, `UBLK_F_QUIESCE`, batch I/O, and the precise semantics of USER_RECOVERY on the Linux 7.0 dev host. Check `Documentation/block/ublk.rst` and `include/uapi/linux/ublk_cmd.h`.
- Paper details (years/venues listed from memory): Slacker FAST 2016; DADI ATC 2020; Leap ATC 2020; Mithril SoCC 2017; S3-FIFO SOSP 2023; SIEVE NSDI 2024; Kangaroo SOSP 2021; FairyWREN OSDI 2024; SHARDS FAST 2015; Counter Stacks OSDI 2014; Lillibridge FAST 2013; Data Domain FAST 2008; LSVD EuroSys 2022; BarrierFS FAST 2018; optimistic crash consistency SOSP 2013; Venti FAST 2002; Cumulus FAST 2009; RACS SoCC 2010; DepSky EuroSys 2011; DupLESS USENIX Security 2013; Harnik 2010 dedup side channel; Hyperbolic caching ATC 2017. Verify each before citing in a design doc.
- Quantitative claims (hedging p99 improvement, 5%-hot-set warm-up time, pack-size sweet spot, map memory per TB, epoch write amplification, compression gains from dictionaries at 16 KB) are estimates; each needs a benchmark in the Lunix prototype before being promised.
- Whether Lunix should reuse Constellation's tree/commit crates directly (Constellation DESIGN.md mentions a content-addressed namespace tree whose root is the commit) versus a Lunix-specific radix map; the format overlap argues for sharing the chunk store, key hierarchy and pack format, but not the tree.
- Journal-first destage delay vs. async RPO interaction: whether the async interval should bound destage delay or only seal frequency.

## Sources

Local:
- `/home/bra/cvs/replicix/lunix/docs/explanation/research/BRIEF.md`
- `/home/bra/cvs/replicix/constellation/docs/explanation/DESIGN.md` (keyed blake3 addressing, XChaCha20-Poly1305, content-addressed commit tree, snapshots)
- `/home/bra/cvs/replicix/constellation/docs/explanation/DECISIONS.md` (ADR-3 content-addressed chunks, ADR-10 plaintext-hash identity)
- `/home/bra/cvs/replicix/constellation/docs/explanation/GOALS.md` (restic comparison)

Literature and systems (from memory, URLs not fetched in this pass):
- Dean and Barroso, "The Tail at Scale", CACM 2013
- Harter et al., "Slacker: Fast Distribution with Lazy Docker Containers", FAST 2016
- Hajkazemi et al., "Beating the I/O Bottleneck: A Case for Log-Structured Virtual Disks", EuroSys 2022 (LSVD)
- Zhu, Li, Patterson, "Avoiding the Disk Bottleneck in the Data Domain Deduplication File System", FAST 2008
- Lillibridge et al., "Improving Restore Speed for Backup Systems that Use Inline Chunk-Based Deduplication", FAST 2013
- Quinlan and Dorward, "Venti", FAST 2002; Vrable et al., "Cumulus", FAST 2009
- Yang et al., "FIFO Queues are All You Need for Cache Eviction" (S3-FIFO), SOSP 2023; Zhang et al., "SIEVE", NSDI 2024
- McAllister et al., "Kangaroo", SOSP 2021; "FairyWREN", OSDI 2024
- Waldspurger et al., "Efficient MRC Construction with SHARDS", FAST 2015; Wires et al., "Characterizing Storage Workloads with Counter Stacks", OSDI 2014
- Maruf and Chowdhury, "Effectively Prefetching Remote Memory with Leap", ATC 2020; Yang et al., "Mithril", SoCC 2017; Joseph and Grunwald, Markov prefetching, ISCA 1997
- Won et al., "Barrier-Enabled IO Stack for Flash Storage", FAST 2018; Chidambaram et al., "Optimistic Crash Consistency", SOSP 2013
- Sivathanu et al., "Semantically-Smart Disk Systems", FAST 2003; "Life or Death at Block-Level", OSDI 2004; Sivathanu et al., "Type-Safe Disks", OSDI 2006
- Clark et al., "Live Migration of Virtual Machines", NSDI 2005
- Harnik, Pinkas, Shulman-Peleg, "Side Channels in Cloud Services: Deduplication in Cloud Storage", IEEE S&P Magazine 2010; Douceur et al. 2002 (convergent encryption); Bellare, Keelveedhi, Ristenpart, "Message-Locked Encryption", EUROCRYPT 2013; "DupLESS", USENIX Security 2013
- Abu-Libdeh et al., "RACS", SoCC 2010; Bessani et al., "DepSky", EuroSys 2011
- Xia et al., "FastCDC", USENIX ATC 2016
- Alibaba "DADI: Block-Level Image Service for Agile and Elastic Application Deployment", ATC 2020
- Verbitski et al., "Amazon Aurora", SIGMOD 2017
- Systems/docs: SlateDB, Delta Lake transaction log protocol, Neon architecture docs, Dolt prolly trees, restic/Kopia design docs, Nydus and eStargz (containerd) and AWS SOCI docs, AWS EBS Fast Snapshot Restore, AWS S3 conditional writes and S3 Express One Zone and S3 additional checksums announcements, zstd dictionary documentation, Linux `Documentation/block/ublk.rst`
