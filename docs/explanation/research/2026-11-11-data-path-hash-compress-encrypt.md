# Lunix CPU-side data path: chunking, hashing, zero detection, compression, encryption, parallelism

Date: 2026-11-11
Author: research agent (Sonnet)
Question: What should the CPU-side data path of Lunix (a ublk/io_uring block-device daemon with content-addressed, deduplicated, compressed, optionally encrypted chunks backed by local FS and S3) look like to reach multi-GB/s per node with low per-IO latency? Covers chunk size / chunking strategy, hash choice, zero detection, codec and level, AEAD and key hierarchy, integrity of the block map, threading model, copy avoidance, and Rust crate choice.

Measurement provenance: numbers marked **[measured]** come from a micro-benchmark I ran on this dev host (scratch dir under /tmp/claude-1000/, not in the repo). Numbers marked **[recalled]** come from papers/READMEs as I remember them and were NOT re-fetched in this session (web fetch was not used); they are listed under UNVERIFIED. Treat both as order-of-magnitude guides.

Host: Intel Core i9-14900K (Raptor Lake, 8P+16E cores / 32 threads, boost up to 6.0 GHz), AES-NI, VAES, VPCLMULQDQ, SHA-NI, AVX2, AVX-VNNI, GFNI, **no AVX-512**; 36 MiB L3, 1 NUMA node. Rust 1.98 stable, `-C target-cpu=native`, release, single thread, data hot in cache (L1/L2), threads not pinned (may land on P-core at ~5.5-6 GHz). Server cores (Xeon/EPYC at 3-3.5 GHz) will be roughly 0.5-0.7x of these single-core numbers; EPYC Zen4/5 and Sapphire Rapids+ have AVX-512 which helps BLAKE3 (about 2x its AVX2 rate).

## Summary

- **Use fixed-size, aligned chunks, not content-defined chunking.** A block device has in-place overwrites at LBA offsets; CDC makes the LBA->chunk map an extent tree with reshaping on every overwrite, and the guest filesystem already aligns data to 4 KiB, so fixed chunks capture almost all the dedup CDC would. CDC buys nothing here and costs CPU (FastCDC ~1-3 GB/s/core extra pass) and map complexity.
- **Two-level layout wins**: a fixed **16 KiB logical chunk** (default; 4 KiB and 64 KiB selectable at volume create, immutable afterwards) as the unit of hash/dedup/compress/map, and **4-16 MiB packed segment objects** on S3 (and in the local log) holding variable-size compressed+encrypted chunk extents. Per-chunk S3 objects are economically absurd: 1 TiB of 16 KiB chunks = 67 M PUTs = ~$335 at $0.005/1000 vs ~$1.3 as 4 MiB segments.
- **Block map size** (per TiB, fully populated): 4 KiB = 268 M entries, 16 KiB = 67 M, 64 KiB = 16.8 M, 256 KiB = 4.2 M, 1 MiB = 1.05 M. At ~32-40 B/entry that is 8.6-10.7 GiB (4K), 2.1-2.7 GiB (16K), 0.54-0.67 GiB (64K) per TiB. 16 KiB is the sweet spot: dedup/compression nearly as good as 4K, map 4x smaller, and the map must be paged/tiered (not RAM-resident) beyond ~1 TiB regardless.
- **Hash: BLAKE3 (256-bit, keyed mode when encrypted) is the content address.** [measured] ~4 GB/s at 4 KiB, ~5.8 GB/s at 16-64 KiB per core; SHA-256 w/ SHA-NI is 2.4-2.5 GB/s, 2-2.4x slower and with no advantage except FIPS. xxh3-128 (~50-70 GB/s) is for in-memory cache keys / write-path pre-filters only, never as a persistent identity or across trust domains.
- **Zero detection is free** if written as an OR-accumulate loop that the compiler vectorizes: [measured] ~100-150 GB/s hot (memory-bound) vs 5 GB/s for a naive `iter().all(|b| *b == 0)` with early exit. Do it first on every chunk; also honor ublk DISCARD/WRITE_ZEROES by punching holes in the map directly without touching data. Zero = absent map entry (sparse).
- **The compressor is the bottleneck, not hash or AEAD.** Per core [measured, synthetic]: BLAKE3 ~5 GB/s, AEAD ~11 GB/s (ring AES-GCM), zstd-1 compress 0.4-9 GB/s depending entirely on data compressibility (it has a fast path for incompressible data), lz4 ~0.7-1 GB/s on mixed data. Budget the node by compressor throughput: for 5 GB/s ingest of moderately compressible data expect ~8-12 cores of compress threads.
- **Default codec: zstd level 1 via context-reusing API (zstd-safe/`zstd::bulk::Compressor`), with an adaptive skip**: lz4_flex/entropy probe on a 4 KiB sample, store raw if saving <~6%. Keep lz4 (fast, 30-50 GB/s decompress) as a per-volume option for latency-critical/NVMe-fast setups, zstd 3 as "balanced", zstd 9+ only for background recompression of cold segments.
- **Default AEAD: AES-256-GCM via aws-lc-rs (or ring)** [measured] 11 GB/s vs RustCrypto `aes-gcm` 2.1 GB/s (5x gap! RustCrypto does not use VAES/aggregated reduction). Use a per-segment subkey (BLAKE3 `derive_key` from the DEK + segment id) with a fixed/counter nonce so nonce reuse is structurally impossible and there is no 2^32 random-nonce limit. Constellation uses RustCrypto XChaCha20-Poly1305 with random 24-byte nonces; that is fine for ~100 MB/s metadata-ish traffic but too slow (~2 GB/s/core) and unnecessary for Lunix segments.
- **Dedup under encryption: keyed BLAKE3 addressing** exactly as Constellation does (`blake3::keyed_hash` with an addressing key derived from the master key). Dedup then works only inside a key domain, and the provider cannot confirm-a-file. Offer an explicit "dedup domain" (shared addressing key) for multi-volume dedup within one tenant; never cross-tenant. Convergent encryption is rejected (equality + confirmation leaks, no better dedup than keyed addressing).
- **Integrity: the S3 bucket is untrusted.** Content address = keyed hash of plaintext; AEAD AAD binds (volume id, segment id, offset, chunk hash); the block map is a Merkle structure whose root (plus a monotonic generation) is MAC'd/signed with a key derived from the master key. This stops chunk swapping; **rollback** to an older consistent root needs a generation counter held outside the bucket (or mirrored bucket quorum) - flagged as open.
- **Threading: ublk queue threads (one per queue, pinned, own io_uring) do I/O, zero-scan, cache lookups and small-chunk decompression inline; hash+compress+encrypt run on a bounded worker pool off the ack path** (ack after the write is durable in the local journal, not after it is compressed). Use mimalloc or, better, pre-allocated hugepage buffer pools. ublk zero-copy is fundamentally incompatible with a pipeline that must read every byte (hash, zero-scan, compress); use `UBLK_F_USER_COPY` or the classic per-IO buffer.

## 1. Chunking

### 1.1 Fixed vs content-defined

| Aspect | Fixed aligned chunks | CDC (FastCDC) |
|---|---|---|
| LBA -> chunk lookup | O(1) arithmetic into radix/paged array | extent tree (B-tree on LBA ranges) |
| In-place overwrite | replace one map slot | split/merge extents, re-chunk neighbours, shifts hash boundaries |
| Dedup on block-aligned data (VM/FS images, DB pages) | ~equal; guest FS allocates in 4 KiB units so duplicate data is 4K-aligned | marginally better only for unaligned shifts (e.g. tar/backup streams inside the volume) |
| CPU | none | extra Gear-hash pass, FastCDC ~1-3 GB/s/core [recalled, ATC'16 reports ~10x Rabin, 1.2-3x over Gear/AE variants], serial per stream |
| Chunk size variance | none; predictable RMW | variable; RMW unbounded |
| Read of random 4 KiB | map slot -> one extent | tree walk |

Literature anchors [recalled]: Meyer & Bolosky (FAST'11) show whole-file dedup on desktop file systems gets about 3/4 of the savings of block-level dedup and that fixed 8 KiB blocks are within a few percent of CDC for most workloads; Jin & Miller (SYSTOR'09) show fixed-size chunking works well for VM images because they are block-aligned. ZFS zvols (the closest analogue: block device on content-hashed blocks) use fixed `volblocksize`, default 8 KiB, changed to 16 KiB in OpenZFS 2.2 [recalled]. Ceph RBD uses fixed 4 MiB objects and has no dedup.

Verdict: **fixed chunks**. CDC could only matter if Lunix were used as a backup target (stream inside a volume) - that is the Constellation file-level job (its `cdc` layout is "future" in DESIGN.md §chunking), not Lunix's.

### 1.2 Metadata size per TiB (fully written)

Entry = hash (32 B, or 16 B if truncated, see §2.5) + segment id/offset (8-12 B) + compressed length/flags (4 B). Say 32-48 B. Computed:

| Chunk | Entries / TiB | Map @ 32 B | Map @ 48 B |
|---|---|---|---|
| 4 KiB | 268,435,456 | 8.0 GiB | 12.0 GiB |
| 16 KiB | 67,108,864 | 2.0 GiB | 3.0 GiB |
| 64 KiB | 16,777,216 | 512 MiB | 768 MiB |
| 256 KiB | 4,194,304 | 128 MiB | 192 MiB |
| 1 MiB | 1,048,576 | 32 MiB | 48 MiB |
| 4 MiB | 262,144 | 8 MiB | 12 MiB |

Implications: 4 KiB granularity with a 100 TiB volume is a 800 GiB map: not viable in RAM; even 16 KiB is 200 GiB at 100 TiB. The map must be a paged structure (leaf pages of e.g. 4-16 KiB holding ~128-512 entries, fan-out tree or radix keyed by chunk index), with hot pages cached, dirty pages journaled, and checkpointed to S3 as immutable Merkle pages (Constellation's `mtree` crate and plan 27 "merkle-packed-checkpoints" are the template). Sparse regions (zero) cost nothing: absent subtrees.

### 1.3 Write amplification for small random writes

The ublk request is typically 4 KiB (FS block) up to 512 KiB (`max_sectors`). With chunk C and a write of w < C:

- **Local-cache hit**: read old chunk (decompress if stored compressed), merge, mark dirty - RAM/NVMe cost only. The *durable* write is a journal append of just the w bytes (+ header), so ack-path amplification is ~1x regardless of C.
- **At flush / segment seal (what hits S3)**: the whole chunk is rewritten: amplification = C / (bytes modified within the chunk during the coalescing window), times 1/compression ratio. 4 KiB random writes into 16 KiB chunks: up to 4x before compression (~2x after at 2:1), but databases and filesystems re-dirty hot pages repeatedly, and async mode lets many 4K writes to the same chunk coalesce. Into 1 MiB chunks: up to 256x; 4 MiB: 1024x (this is why Ceph RBD 4 MiB objects need an object cacher/journal).
- **Cold-cache RMW is the real killer**: a partial write to a chunk that exists only on S3 needs the old chunk to compute the new content hash: one S3 GET (first-byte 10-100 ms) on the write path. Mitigations: (a) ack from the journal and do the RMW fetch asynchronously (read-your-writes served by overlaying journal patches on the base when it arrives); (b) prefetch neighbours; (c) per-chunk "delta overlay" records until compaction. Smaller chunk = smaller fetch and fewer cold RMWs per byte of working set.
- Sub-chunk journal records mean the daemon can keep **4 KiB-granular dirty tracking** (a 4-bit mask per 16 KiB chunk) without 4 KiB map granularity.

### 1.4 S3 request cost vs layout (us-east-1 list price [recalled]: PUT $0.005/1000, GET $0.0004/1000, storage $0.023/GB-month)

Fully writing 1 TiB:

| Layout | PUT count | PUT cost |
|---|---|---|
| 1 chunk = 1 object, 16 KiB | 67.1 M | ~$335 |
| 64 KiB objects | 16.8 M | ~$84 |
| 1 MiB objects | 1.05 M | ~$5.2 |
| 4 MiB objects/segments | 262 k | ~$1.3 |
| 8 MiB segments | 131 k | ~$0.65 |
| 16 MiB segments | 65 k | ~$0.33 |

Reading 1 TiB cold in 16 KiB ranged GETs = 67 M GETs = ~$27 (plus latency: one RTT each), so reads need readahead/segment-level fetch: fetch a 1-4 MiB aligned range of a segment holding spatially related chunks (the segment writer should pack chunks in LBA order within a flush batch to preserve locality). Segment size trade-off: bigger = cheaper PUTs, better multipart throughput, but more GC write amplification (live-chunk compaction) and coarser upload-failure retry. 8 MiB default; 4-16 MiB range.

### 1.5 Compression ratio vs chunk size

[measured] with a synthetic word-salad corpus (not representative of real data; shows trend only), zstd ratio vs input size: level 1: 4 KiB 2.62, 64 KiB 2.97, 1 MiB 3.03; level 3: 2.85 / 3.19 / 3.24; lz4: 1.69 / 2.05 / 2.07. i.e. going 4K->64K gains 10-15% ratio, 64K->1M under 3%. Real-world: ZFS experience says larger recordsize compresses better (lz4/zstd cap out by 64-128 KiB). Dictionaries (zstd, trained on the volume's content) recover much of the small-chunk loss (zstd docs: dictionaries matter most below ~16 KiB). Compression ratio is thus a mild argument for 16-64 KiB over 4 KiB; dedup granularity is an argument the other way (dedup ratio for VM images falls as chunk grows because a changed 4K block invalidates the whole chunk). 16 KiB balances.

### 1.6 Variable-size compressed extents in packed segments

Segment object layout (sketch): `[header][chunk extent]*[footer index]`, where an extent = `{codec,u8; uncompressed_len; compressed_len; ciphertext; tag}` appended back to back with no sector rounding (a local-FS cache gets this for free; a block-aligned SSD layer would want 512 B/4 KiB padding for O_DIRECT - choose per tier). The map entry stores `(segment_id, offset, clen)`; a read issues a ranged GET of exactly `clen`+overhead (or a larger aligned window for readahead). The footer index (hash, offset, clen, ulen per extent) lets a segment be GC'd and re-verified without the map. Constellation already uses this model for metadata ("packs.rs", zstd'd nodes, `decode_gate.rs`) and a `magic, format_ver, codec_id, level, uncompressed_len` per-blob header - reuse the header idea (DESIGN.md §chunks).

Local tier: same format - an append-only local segment log means write-through/write-back upload is "close the segment, PUT it" with zero re-encoding, and cache eviction is segment-granular or chunk-granular via the index.

## 2. Hashing

[measured] single thread, i9-14900K, AVX2 (no AVX-512), 16 KiB-ish regimes:

| Hash | 4 KiB | 64 KiB | 1 MiB | Notes |
|---|---|---|---|---|
| BLAKE3 | 4.0 GB/s (1.0 us) | 5.8 GB/s (11 us) | 5.2 GB/s | 256-bit; SIMD (AVX2/AVX-512/NEON) in `blake3` crate by runtime detection |
| BLAKE3 keyed | 3.75 GB/s | 5.8 GB/s | 5.9 GB/s | same cost; 256-bit key |
| xxh3-64 | 46 GB/s | 72 GB/s | 66 GB/s | non-crypto |
| xxh3-128 | 48 GB/s | 68 GB/s | 57 GB/s | non-crypto |
| SHA-256 (`sha2` crate, SHA-NI) | 2.5 GB/s | 2.4 GB/s | 2.5 GB/s | |
| SHA-256 (ring) | 2.45 | 2.4 | 2.6 | same, SHA-NI |
| BLAKE3 64 MiB, 1 thread / rayon `update_rayon` | 4.55 GB/s / **42 GB/s** | | | multi-threaded tree hashing |

(Hash cost for 16 KiB chunk ~3.5 us at ~4.7 GB/s; BLAKE3's 1 KiB-chunk tree parallelism is why it speeds up with size inside one core: 4 KiB underfills the SIMD lanes. With AVX-512 expect roughly 1.5-2x.)

### 2.1 Which for what

- **Content address (persistent, S3 key, dedup identity): BLAKE3-256.** Cryptographic collision resistance (128-bit), 2x faster than SHA-256 per core, tree mode allows incremental verification and parallel hash of very large buffers (not needed for 16 KiB chunks; useful for whole-segment verification: `update_rayon` hit 42 GB/s on this box).
- **Encrypted volumes: keyed BLAKE3** (`blake3::keyed_hash(addr_key, chunk)`) or HMAC-SHA256. Same speed. Provider cannot test for known plaintext; equal plaintext in the same key domain still dedups. Derive `addr_key` and DEK from the master key with distinct contexts (Constellation `derive_from_master` with purpose bytes does this; use BLAKE3 `derive_key` with context strings in Lunix).
- **xxh3-128 / xxh3-64: in-memory only** - hash-table keys for the chunk cache, "have I seen this chunk in this flush batch" prefilter, bloom filters, dirty tracking. Reasons not to use it as identity: not collision-resistant against an adversary (crafted collisions are known to be easy for XXH3-64; 128-bit is better but still unproven), no keyed mode with security claims (it has a seed/secret but is not a MAC). If a tenant can write arbitrary data to the volume (they can; it is a block device) they can attempt to poison a non-crypto cache key. Safe pattern: cache lookup by xxh3 -> compare against stored full BLAKE3 or full data before use; or key the cache by the BLAKE3 address directly and use xxh3 only to pick the bucket.
- **SHA-256: only where something external demands it**: FIPS mode (BLAKE3 is not FIPS-approved), or `x-amz-checksum-sha256`. For S3 transport integrity prefer the hardware-accelerated **CRC32C / CRC64-NVME** checksum algorithms S3 supports (CRC64NVME added Dec 2024 [recalled]) computed on the sealed segment: ~30+ GB/s with PCLMULQDQ/VPCLMULQDQ, near free, and SDKs now default to CRC checksums. Use `UNSIGNED-PAYLOAD`/streaming signing over HTTPS to avoid SigV4 payload SHA-256 over every PUT body (or use the checksum trailer).
- **Cheap local integrity (journal/cache records): CRC32C** (crc32fast / `crc32c` crate, SSE4.2 `crc32` instruction ~20+ GB/s) for torn-write detection in the local journal; this is not a security feature.

### 2.2 Collision and truncation

Birthday bound for n chunks with a b-bit hash: p ~ n^2 / 2^(b+1). For 100 TiB at 16 KiB, n = 6.7e9 ~ 2^32.6: with b=256 negligible; with b=128 p ~ 2^(65-129) = 2^-64, still negligible *against non-adversarial data*. Adversarial analysis for 128-bit truncated BLAKE3: a collision attack costs 2^64 work - infeasible-ish today but not comfortable for a long-lived multi-tenant system. Recommendation: store 256-bit addresses in S3 keys and on-disk indexes, but allow the in-memory/paged map to carry a 128-bit prefix plus the full hash verified on first fetch (decode-and-verify rule below) if map size dominates. The map entry then shrinks to ~24-28 B.

### 2.3 Verify-on-read

Recompute the (keyed) BLAKE3 of every chunk fetched from S3 or cold local media and compare to the map entry. At 3.5 us per 16 KiB it is <5% of a 100 ms S3 GET and ~10-20% of a local NVMe 4K read - cheap enough to be on by default for S3 fetches, optional (sampled, or scrub-only) for local-hot reads. This is the actual defense against bit rot and a corrupt/malicious bucket; the AEAD tag alone does not prove the chunk is the one the map refers to (see §5.4).

## 3. Zero detection and discard

- **Scan** [measured]: naive `buf.iter().all(|&b| b == 0)` = 5 GB/s (LLVM does not vectorize early-exit loops); an OR-accumulating loop over `chunks_exact(64)` of u64 words gets ~100-150 GB/s hot, i.e. memory-bandwidth bound, 0.1-0.2 us for 16 KiB. Also trivial: `data[0]==0 && data[1..] == data[..len-1]` (memcmp-style overlap trick) or `slice.iter().fold(0u64, |a, w| a | w)` over aligned u64/u128/u256. Do it **before** hashing/compressing; a hit skips ~100% of the pipeline. On the read side, absent map entry -> memset (or the ublk `memset` of the user buffer) with no I/O.
- **Partially zero chunks** need no special handling: zstd/lz4 compress 4 KiB of zeros inside a 16 KiB chunk to a few bytes. Keep a "chunk is all-zero" constant hash (= hash of 16 KiB zeros, precomputed per chunk size and key) mapped to "no entry".
- **Kernel hints**: ublk exposes `UBLK_IO_OP_DISCARD`, `UBLK_IO_OP_WRITE_ZEROES` (also `UBLK_IO_OP_WRITE_SAME`/`SECURE_ERASE` in the UAPI header). Advertise via `UBLK_PARAM_TYPE_DISCARD` (`max_discard_sectors`, `max_discard_segments`, `discard_granularity`, `discard_alignment`) and set the write-zeroes limits. With these, `mkfs`, `fstrim`, and `mount -o discard` give Lunix explicit "this range is dead" signals: remove map entries (refcount-decrement chunks), no data scan needed. For ranges not chunk-aligned: full chunks are punched out, edge chunks are partial writes of zeros (RMW). Guarantee `discard_zeroes_data` semantics (reads return zeros) - easy with sparse map; this lets ext4/xfs `mkfs` skip zeroing (`lazy_itable_init`, `nodiscard` irrelevant) and makes thin-provisioning real.
- **Sparse map**: absent subtree = zero; a volume of N TiB created empty costs O(1). Shrink = discard tail. Thin growth = extend the radix root.
- Caution: a discard that arrives, then a crash before the map change is journaled, must not resurrect data in async mode; journal the discard record before ack (see crash-consistency notes).

## 4. Compression

### 4.1 Codec numbers

[measured] per core, `zstd` crate 0.13 (libzstd C, `bulk::compress` creating a context per call - so 4 KiB numbers are pessimistic; use reusable contexts), `lz4_flex` 0.11 (pure-Rust block format). "random" = incompressible; "textish" = my synthetic word-soup (ratio shown in label; real data like filesystems/DB pages/logs will vary, and often compress faster than this adversarial corpus).

Incompressible data (compress GB/s / decompress GB/s):

| | 4 KiB | 64 KiB | 1 MiB |
|---|---|---|---|
| zstd -5 (fast) | 2.1 / 5.1 | 13.3 / 40 | 11.0 / 44 |
| zstd 1 | 1.3 / 5.5 | 9.2 / 40 | 9.8 / 46 |
| zstd 3 | 0.97 / 6.6 | 5.0 / 37 | 7.7 / 42 |
| zstd 9 | 0.33 / 5.8 | 2.4 / 40 | 1.9 / 49 |
| lz4_flex | 5.1 / 52 | 15.4 / 34 | 23.5 / 32 |

(zstd and lz4 detect incompressible runs and fall back to stored/skip-ahead; "decompress" of stored blocks is just memcpy.) So **the cost of trying to compress incompressible data is small with zstd-fast/lz4, ~5-10 us for 64 KiB at levels 1-3**, which weakens the case for elaborate pre-probes; still, a 4 KiB probe avoids even that and avoids the 12 us/4 KiB at level 9.

Compressible synthetic data ("textish"):

| | 4 KiB comp (ratio) | 64 KiB comp (ratio) | 1 MiB comp (ratio) | decomp 4K / 64K / 1M GB/s |
|---|---|---|---|---|
| zstd -5 | 0.61 GB/s (1.90) | 0.55 (2.54) | 0.55 (2.63) | 1.5 / 1.8 / 2.2 |
| zstd 1 | 0.38 (2.62) | 0.52 (2.97) | 0.44 (3.03) | 0.9 / 1.5 / 1.9 |
| zstd 3 | 0.37 (2.85) | 0.47 (3.19) | 0.41 (3.24) | 1.2 / 1.8 / 2.1 |
| zstd 9 | 0.04 (2.93) | 0.06 (3.36) | 0.08 (3.41) | 1.2 / 2.0 / 2.0 |
| lz4_flex | 0.98 (1.69) | 0.87 (2.05) | 0.71 (2.07) | 3.0 / 4.1 / 3.8 |

This synthetic corpus is nearly worst-case for entropy coding (random tokens drawn from a small vocabulary -> tiny matches, many Huffman literals), so absolute speeds are ~3x lower than typical real data. Reference points from the zstd README on Silesia (i7-9700K, single thread) [recalled]: zstd 1: 2.89x, ~510 MB/s comp, ~1550 MB/s decomp; zstd --fast=1: 2.1-ish x, ~700-ish MB/s; lz4 default: 2.10x, ~780 MB/s, ~4.9 GB/s decomp; lz4hc: ~2.7x at ~40-90 MB/s; snappy ~2.1x, ~560 MB/s, ~1.8 GB/s decomp. On real data, **zstd-3 gets ~+10-15% ratio over zstd-1 at -20% speed; zstd-9+ is 5-10x slower for +5% more.** lz4 decompress is 2-3x faster than zstd decompress, which matters for the read path on warm local NVMe (NVMe 4K read ~80 us; decompressing 16 KiB with zstd ~10 us is 12% of the latency, ~1 us for lz4).

Snappy: dominated by lz4 on every axis (ratio similar, lz4 decompresses faster, lz4_flex is pure Rust). Skip it. lz4hc: use only for background recompression where decompress speed matters more (lz4hc output decompresses at lz4 speed; ratio ~ zstd-1 minus 10-15%).

### 4.2 Adaptive compression

Goal: don't burn CPU on incompressible data (encrypted/already compressed VM contents, media, compressed DB pages).
1. Zero-scan (free).
2. For chunk sizes >=16 KiB: compress with **lz4/zstd-fast on the first 4 KiB sample**; if ratio < 1.06 (6% saving), mark `codec=raw` for the chunk and skip. (Btrfs's heuristic uses byte-set size / Shannon entropy on a 2 KiB sample of the extent; entropy estimate via 256-bin byte histogram costs ~1-2 GB/s scalar, not clearly cheaper than a 4 KiB lz4 trial at 5+ GB/s, so prefer the lz4 trial. A tiny histogram trick: count distinct high-frequency bytes; if > 200 distinct bytes of 2 KiB sample at near-uniform frequency, skip.)
3. Else compress the full chunk with the volume codec; if `clen >= ulen - slack` store raw. **Slack is smaller than for sector-aligned stores** (ZFS needs 12.5% = 1 sector granule): with packed variable-size extents any saving is real bytes saved, but a saving below ~6% does not repay decompress latency; use 6% default, tunable.
4. Remember the decision per LBA region (a 1-bit "last time incompressible" hint) so sequentially written already-compressed files skip the probe entirely; re-probe occasionally.
5. Per-volume `compression=off|lz4|zstd:N|auto`; `auto` = lz4 probe then zstd 1-3 (cheap ratio gain) with backpressure: if the compress worker queue exceeds a depth threshold, drop to lz4 or raw for new chunks (Constellation does "incompressible guard: if compressed >= original, store raw", DESIGN.md §chunks; add the dynamic level).
6. Level changes never rewrite existing chunks (Constellation does the same: "never recompresses existing objects"); an offline/background `recompress` pass over cold segments can use zstd 9-19.

### 4.3 Dictionaries and small chunks

For 4-16 KiB chunks a trained zstd dictionary (`zstd --train`, 64-112 KiB dict, per-volume, stored in S3 metadata, versioned by dict id in the chunk header) improves ratio notably for structured data (filesystem metadata, DB pages, logs: often +20-40% at 4 KiB [recalled, zstd docs]). Costs: per-context `CDict`/`DDict` load (do once per worker thread), dict must live forever or be re-referenced (immutable, content-addressed, replicated with the volume). Optional phase-2 feature; train from a sample of the first N MiB written. The bigger win is simply not using 4 KiB chunks.

### 4.4 Parallelism inside compression

- Parallelize **across chunks** (embarrassingly parallel, each worker owns a reusable zstd `CCtx`; do not use zstd's internal `nbWorkers` MT for 16 KiB inputs - overhead dominates; Constellation says "zstd internal MT only for big single blobs", same rule).
- Reuse contexts: allocating a `CCtx` per call (as `zstd::bulk::compress` does) costs microseconds - visible in my 4 KiB numbers (1.3 GB/s vs 9 GB/s at 64 KiB for incompressible). Use `zstd::bulk::Compressor::with_dictionary` / `zstd-safe::CCtx` held in a thread-local; same for `DCtx`.
- Batch for cache locality: process a batch of N chunks per task (e.g. 64 chunks = 1 MiB) to amortize wakeups.
- Decompression is serial per chunk but chunks are independent: a large read (e.g. 1 MiB = 64 chunks) fans out across workers; a single 4 KiB read decompresses inline.

### 4.5 Crates

| Crate | What | Notes |
|---|---|---|
| `zstd` 0.13 / `zstd-safe` | bindings to C libzstd 1.5.x via `zstd-sys` | fastest, full feature set (dicts, levels, negative levels, MT, advanced params). `zstd-safe` exposes `CCtx`/`DCtx`/`CDict`/`DDict` for reuse; the high-level `zstd` crate is a convenience wrapper (what Constellation uses, `zstd = "0.13"`). Build needs a C compiler. |
| `lz4_flex` 0.11 | pure Rust, `unsafe`-optional (`safe-encode/safe-decode`), block + frame | ~same speed as C lz4 default level (my 16-23 GB/s on incompressible, 3-4 GB/s decomp on compressible); no HC mode. Good default for fast path; safe-decode build for untrusted S3 input. |
| `lz4` / `lz4-sys` | C liblz4 | gives LZ4HC; heavier build. |
| `ruzstd` | pure Rust zstd (decoder mature, encoder basic) | 2-3x slower than C decode [recalled]; useful only for no-C builds. |
| `snap` | snappy | skip. |

Decompression bomb guard: always pass the expected `ulen` (from the map/segment index, itself authenticated) as the max output; Constellation has a `decode_gate.rs` and bomb tests in `format.rs` doing this - port the idea.

## 5. Encryption

### 5.1 AEAD options [measured, one core, 4 KiB / 64 KiB / 1 MiB, includes a buffer clone ~free]

| AEAD / crate | 4 KiB | 64 KiB | 1 MiB |
|---|---|---|---|
| AES-256-GCM, **ring** 0.17 | 10.3 GB/s | 11.3 | 11.1 |
| AES-256-GCM, RustCrypto `aes-gcm` 0.10 | 2.1 | 2.1 | 2.1 |
| AES-256-GCM-SIV, RustCrypto | 1.9 | 2.2 | 1.9 |
| ChaCha20-Poly1305, ring | 2.3 | 2.9 | 2.9 |
| ChaCha20-Poly1305, RustCrypto | 1.85 | 2.3 | 2.1 |
| XChaCha20-Poly1305 | ~= ChaCha20-Poly1305 minus an HChaCha block (RustCrypto only) | | |

Conclusions:
- Hardware AES (AES-NI + PCLMULQDQ, with VAES+VPCLMULQDQ aggregated implementations in ring/BoringSSL/aws-lc) is **4-5x faster** than any software-pipelined alternative on x86 here. RustCrypto `aes-gcm` has no VAES/aggregated-reduction path as of 0.10 (UNVERIFIED for newer releases), so it leaves ~80% on the table. Use **aws-lc-rs or ring** for the bulk path.
- ChaCha20-Poly1305 is the choice only where there is no AES hardware (older ARM, some RISC-V). On Graviton/Apple (ARMv8 crypto ext.) AES-GCM is also fast; no reason to prefer ChaCha on server hardware.
- AES-GCM-SIV (nonce-misuse resistant) costs ~5x more than ring GCM here due to the RustCrypto implementation; its security margin matters for random nonces at scale, which the per-segment-subkey design (below) removes.
- Per-core AEAD at 10+ GB/s means encryption is ~10% of the budget of compression; never the bottleneck. For 5 GB/s it needs <0.5 core.

### 5.2 Nonce management (the real AES-GCM risk)

GCM with 96-bit random nonces is safe only up to ~2^32 messages per key (NIST SP 800-38D); a 1 PB volume of 16 KiB chunks is 2^36 chunks, and rewrites multiply that. Avoid by design:
- **Per-segment subkey**: `K_seg = BLAKE3::derive_key("lunix v1 seg", DEK || segment_id [|| generation])`; encrypt every chunk extent in that segment with nonce = 96-bit extent counter (0,1,2,...). Each (key, nonce) pair is unique by construction because segment ids are never reused (UUID/ULID or monotonic id plus volume epoch). No random nonces; no birthday bound; one AES key schedule per segment (cheap).
- Alternative: one AEAD envelope over the whole sealed segment (single tag) - fewer tags but forces fetching a whole segment to verify one chunk; per-extent tags (16 B overhead, 0.1% at 16 KiB) allow ranged GET + decrypt of just one extent. Prefer per-extent.
- AAD for each extent = `volume_uuid || segment_id || extent_offset || chunk_hash || codec_id || ulen`: ciphertext cannot be moved between positions or volumes (Constellation passes "the immutable object identity as AAD", e2e.rs `encrypt_object`).

### 5.3 Convergent encryption vs random nonces / keyed addressing

- **Convergent encryption** (key = H(plaintext), as in Tahoe-LAFS / Bitcasa): deterministic, cross-user dedup, but leaks equality and allows confirmation-of-file attacks (provider hashes a known file and checks for its ciphertext/address) and learn-the-remaining-information attacks for low-entropy blocks. A block device has highly structured, guessable blocks (FS metadata, zeroed regions, known OS images) so the leak is practical. **Reject.**
- **Keyed addressing + randomized/unique-nonce encryption** (Constellation's approach): identity = keyed hash under a secret; ciphertext uses a unique nonce; provider sees only random-looking keys and sizes. Dedup holds within the key domain. Leaks: equality of chunks *within* the volume/domain (two same-length objects with the same address key name... in fact identical chunks have identical address so one object - the provider can see that a chunk is referenced from many places only via access patterns), plus chunk size distribution (compressed length reveals compressibility: a known CRIME-like side-channel in compress-then-encrypt; for storage-at-rest this is generally accepted, but pad/bucket lengths if the threat model wants it - option `pad=64`).
- Compress-then-encrypt order is mandatory (ciphertext is incompressible); Constellation does this.
- The dedup domain is the addressing-key scope: per-volume default, optional shared key across a tenant's volumes (cross-volume dedup of base images, the main real-world dedup win), never global.

### 5.4 Key hierarchy

```
passphrase / KMS ──(KEK)──► wrapped Master Key (KMK)         stored in volume metadata (S3 + local)
   KMK ──derive(addr)──► addressing key   (keyed BLAKE3, chunk identity)
   KMK ──derive(dek, epoch)──► DEK_epoch   (segment subkeys derive from this)
   KMK ──derive(map-mac)──► map MAC / signing key
```
- Constellation (read from `crates/store-s3/src/e2e.rs`, DESIGN.md §8): random 256-bit master key, wrapped under an Argon2id KEK (64 MiB, t=3, p=1, parameters persisted), every data key derived with `blake3::keyed_hash(master, "constellation/e2e/v1/" || purpose || partition)` so `passwd` rewraps one envelope and rotates nothing; XChaCha20-Poly1305 with a random 24-byte nonce and version byte; keys held `Zeroizing` and mlock'd. Lunix should reuse this pattern and ideally the same crate code (Constellation is modifiable; factor `e2e.rs` into a shared crate) but swap the segment cipher to AES-256-GCM+subkey for speed, keeping XChaCha as the cipher for small metadata objects where the random-nonce simplicity matters more than speed.
- **KMS mode**: KEK = cloud KMS key (AWS KMS / GCP / Vault transit); the KMK is stored KMS-wrapped (`envelope encryption`); unwrapped once at attach/mount. Multi-region move (the pod-migration use case) needs a KEK that is available in the destination region: either passphrase KEK, or wrap the KMK under several KEKs (key slots, LUKS-style) - one slot per KMS/region, added/removed live like S3 backends.
- **Rotation**: (1) KEK rotation = rewrap KMK: instant. (2) DEK epoch rotation: bump `epoch`; new segments use the new DEK; old segments readable by retained epochs; background re-encrypt on GC/compaction. (3) KMK rotation (full) requires re-addressing everything (addressing key changes -> all chunk identities change) - treat as a "migrate volume" operation, not online. This is the cost of keyed addressing; document it.
- Hardware: mlock + `MADV_DONTDUMP`, `zeroize` on drop, no core dumps, keys never logged; keep key schedules per worker thread.

### 5.5 Integrity and authenticating the block map

Threat: S3 (or a compromised mirror) is *untrusted for integrity and freshness*. Layers:
1. **Per-extent AEAD tag + AAD binding** (above) -> detects bit flips, truncation, extent relocation, cross-volume substitution.
2. **Content address = keyed BLAKE3 of plaintext**, recomputed after decrypt+decompress and compared to the map entry -> a validly encrypted but *wrong* chunk (the attacker replays an older legitimate chunk from the same volume at a different position, which passes the AEAD if AAD did not bind the hash) is rejected. Bind the hash in the AAD AND verify (belt and braces; the verify is 3.5 us).
3. **Merkle block map**: leaf pages hold (chunk hash, segment ref); interior nodes hash their children (keyed BLAKE3 or HMAC); the **root hash + generation + volume uuid is MAC'd** (key from KMK) and stored as the "superblock/commit" object. Constellation's `mtree` / checkpoint design (plan 27) is exactly this; reuse it so a snapshot/commit is one root. A reader verifies a leaf page by path from the root (log N fetches, cached).
4. **Freshness / rollback**: an attacker with bucket write access can serve an older (valid) root. Options: monotonic generation stored in a second trust anchor (KMS-held counter, a tiny DynamoDB/etcd cell, the k8s CSI object), or quorum across mirrored backends (the multi-backend feature gives this for free: refuse a root older than max seen generation across backends, persist `last_seen_generation` locally and in the CSI object). Without an anchor, only "consistent snapshot, possibly stale" can be guaranteed.
5. For unencrypted volumes the same Merkle structure with plain BLAKE3 gives integrity but not authenticity (no secret). Offer an optional signing key (Ed25519) or HMAC key without confidentiality ("integrity-only mode").

## 6. Parallelism and the ublk/io_uring interplay

### 6.1 Pipeline stages and where they run

```
 ublk queue thread (pinned, io_uring per queue)          worker pool (bounded)               background
 ───────────────────────────────────────────────         ───────────────────────────         ──────────
 WRITE: recv req ─► zero-scan ─► journal append (NVMe) ─► ack
                                  └─(batch)─► [hash ─► dedup lookup ─► compress ─► AEAD] ─► segment writer ─► S3 PUT
 READ : recv req ─► map/cache lookup ─ hit raw ─► copy to user ─► ack
                                       └ hit compressed, small ─► inline decompress ─► ack
                                       └ miss/large ─► submit fetch (local NVMe/S3) ─► completion ─► verify ─► decrypt ─► decompress ─► ack
```
Key principle: **the write ack is gated by the local journal's durability (FLUSH/FUA) only.** Hash/dedup/compress/encrypt/upload happen after, from a staging buffer, so their latency (tens of us) is off the critical path and throughput is a function of worker threads. In *sync S3* mode FLUSH additionally waits for segment upload; the compute path then matters, but only per-FLUSH, and batching amortizes it.

Per-IO latency budget (16 KiB chunk, one P-core, hot cache; this host): zero-scan 0.15 us; BLAKE3 3.5 us; zstd-1 compress ~3-30 us (data dependent; ~3 us if incompressible); AEAD 1.5 us (4 KiB: 0.4 us); decompress zstd ~3-10 us (lz4 ~1 us); verify-hash 3.5 us. A cache-hit 4 KiB read decompressing a 16 KiB zstd chunk costs ~5-10 us of CPU - well under an NVMe read; an LRU of **decompressed hot chunks** in RAM removes it for hot data (Constellation caches decompressed chunks by default, DESIGN.md ~L1630).

### 6.2 Thread-per-core io_uring vs tokio work-stealing

- ublk's data-plane model (one `io_uring` per queue, `UBLK_IO_FETCH_REQ` / `COMMIT_AND_FETCH_REQ` as uring commands via IORING_OP_URING_CMD) is naturally thread-per-queue. Pin each queue thread to a core (ideally one near the NVMe/NIC NUMA node; set the ublk `UBLK_F_CMD_IOCTL_ENCODE`/queue affinity to match blk-mq's queue->CPU map) and run a single-threaded runtime/event loop (raw `io-uring` crate, `tokio-uring`, `glommio`, `monoio`). Avoid cross-thread wakeups on the fast path; shard the chunk cache and map by chunk index modulo queue count where possible, or use read-mostly lock-free structures (`arc-swap`, sharded `dashmap`).
- **tokio work-stealing** is a good fit for the S3 side (many concurrent HTTP/2 connections, hyper/reqwest, aws-sdk) - which is a different workload and probably needs tokio anyway (Constellation is tokio-based). Reasonable architecture: ublk queue threads = raw io_uring loops; S3 + control plane = tokio multi-thread runtime; hand-off via bounded SPSC/MPSC channels (`crossbeam`/`rtrb`/`flume`), with completion delivered back to the queue thread via eventfd registered in its ring (`IORING_OP_READ` on an eventfd, or `IORING_OP_MSG_RING`).
- **Compression/encryption worker pool**: plain OS threads (rayon or a custom pinned pool), not tokio blocking threads, sized `ncpu - nqueues - io_threads`; unbounded work-stealing is fine for CPU-bound chunk batches because tasks are uniform. Use bounded queues for backpressure to throttle the ublk acks (when staging is full, delay completion rather than OOM).
- **Inline vs offload on the queue thread**: decompress/verify/decrypt of a single <=64 KiB chunk inline (<15 us, cheaper than a cross-core hop of ~2-5 us wake + cache miss + resched); offload > 64 KiB or batches >1 MiB and anything with a deep compress. Rule: inline if estimated CPU < ~20 us and queue depth is low; offload if the queue thread has backlog (adaptive, measure per-queue idle).
- SMT: the P-core 2-thread SMT gives only ~+15-25% on SIMD-heavy BLAKE3/AES work; don't count hyperthreads as cores when sizing the pool (14900K E-cores: ~half the per-core rate of P-cores for these kernels).

### 6.3 Batching

- Coalesce adjacent dirty chunks into one hash/compress/encrypt task; submit multiple SQEs per `io_uring_enter`; use `IORING_SETUP_SINGLE_ISSUER | DEFER_TASKRUN | COOP_TASKRUN` on queue rings to cut interrupts/IPI (kernel 6.x; present on Linux 7.0); SQPOLL is generally unnecessary and burns a core.
- Segment writer: fill an 8 MiB staging segment from many workers via per-worker sub-buffers merged in order (or an MPSC of finished extents) - the sealing PUT is one multipart-or-single PUT with CRC checksum.
- Hash several chunks at once: BLAKE3 has `hash_many` internals but the public API doesn't expose multi-message batching; for 16 KiB chunks single-message SIMD within the 16 chunks of 1 KiB is already efficient (16 KiB = 16 BLAKE3 leaf chunks fills AVX2 8-wide lanes twice). 4 KiB (4 leaf chunks) underuses SIMD: 4 GB/s vs 5.8: one more reason for 16 KiB over 4 KiB.

### 6.4 Copies: ublk user-copy vs zero-copy vs compression

- ublk transfer modes: (1) default - kernel copies the request payload into a per-tag user buffer that the daemon registered (one copy each way, `memcpy`-speed 40-90 GB/s [measured clone]); (2) `UBLK_F_USER_COPY` - daemon `pread`/`pwrite`s (or io_uring read/write) on `/dev/ublkcN` with an offset encoding (qid, tag, offset) to move data to/from request pages; this allows copying only the needed bytes and skipping the payload entirely for zeroes/discards; (3) `UBLK_F_SUPPORT_ZERO_COPY` - the request's pages are exposed to io_uring as a registered buffer (`IORING_OP_URING_CMD` register/unregister buffer, fixed-buffer `READ_FIXED`/`WRITE_FIXED` linked ops) so a direct passthrough (e.g. to a loop-like file backend) never touches the data in userspace. Newer kernels add auto buffer registration (`UBLK_F_AUTO_BUF_REG`, 6.15+) [recalled, verify on 7.0].
- Zero-copy is **incompatible with Lunix's write path**: userspace *must read every byte* to zero-scan, hash, compress and encrypt, so the copy is unavoidable; and it is incompatible with compressed storage on the read path (the payload lands decompressed in the request pages; decompressing straight *into* the request buffer is the zero-copy that matters). Zero-copy could help only for (a) cache-hit reads of **uncompressed** chunks stored in a local file (io_uring `READ_FIXED` straight into the request's registered buffer) and (b) write-through of raw chunks to the local journal in `raw` codec (`WRITE_FIXED` out of the request buffer, hashing done separately from an mmap/read - but that reads the data anyway). Expected gain small compared to the other work; design the data path so the codec step can decompress directly into the destination buffer (user-copy `pwrite` from a scratch buffer, or decompress into the per-tag buffer) = 1 copy per read, and compress out of the per-tag buffer = 0 extra copies on write (hash + zero-scan + compress each stream the same 16 KiB that is hot in L1/L2: do them back-to-back on the same core to hit cache; 16 KiB fits in L1d 48 KiB).
- Memory: pre-allocate chunk-sized buffer pools (page-aligned, `MAP_HUGETLB`/THP `madvise(MADV_HUGEPAGE)` 2 MiB pages for staging segments and the decompressed-chunk cache) to avoid malloc/page-fault cost and TLB misses; register them with io_uring (`IORING_REGISTER_BUFFERS`) for fixed-buffer local I/O. `O_DIRECT` for the local journal and segment files (avoid page-cache double buffering; the daemon has its own cache); `fallocate` preallocation.
- Allocator: for control-plane/metadata objects use **mimalloc** (or jemalloc); both reduce fragmentation and cross-thread free costs vs glibc malloc. Hot path should not allocate at all: per-thread free lists of chunk-sized buffers, reusable `Vec<u8>` with capacity retention, zstd contexts per thread.
- NUMA: pin queue threads + worker pool + buffer pools to the node that owns the NVMe/NIC; interleave only the large shared map cache; `numactl`-style first-touch allocation. This host has one node; datacenter dual-socket systems need this (cross-socket memory traffic can halve hash/compress throughput).

### 6.5 Throughput budget (target: 5 GB/s sustained node ingest, async S3, 16 KiB chunks)

Per-core rates assume server cores ~3 GHz (0.6x of the measured i9 numbers), typical compressible data:

| Stage | Per core | Cores for 5 GB/s | Notes |
|---|---|---|---|
| ublk queue copy + journal append | ~10 GB/s | 0.5 (+ NVMe bw) | queue threads, 1 per queue |
| zero-scan | 60+ GB/s | <0.1 | |
| BLAKE3 | ~3.5 GB/s (AVX2), ~6 (AVX-512) | 1.0-1.5 | |
| dedup lookup (map/cache) | n/a | ~0.3 | 300 k lookups/s at 5 GB/s/16 KiB; hash-table + paged-map hits |
| zstd-1 compress | 0.4-1.5 GB/s typical, 5+ if incompressible | **4-12** | the budget driver |
| lz4 compress | 0.8-2 GB/s | 3-6 | |
| AES-256-GCM (ring/aws-lc) | ~6.5 GB/s | 0.8 | |
| CRC32C/CRC64 of segment | 20+ GB/s | 0.25 | |
| **Total (zstd-1)** | | **~8-16 cores** | 16-32 hardware threads on a modern server; S3 upload NIC (5 GB/s = 40 Gb/s) is the other limit |

After dedup (e.g. 30% hit) and compression (2x) the S3 egress is ~1.75 GB/s, so a 25-Gbit NIC suffices. For the **latency path** (not throughput) a 4 KiB write adds zero CPU to the ack. Read path of a cache-hit warm chunk: <10 us CPU.

## Recommendations for Lunix

**Default pipeline** (per-volume, immutable at create unless noted):

1. **Chunking**: fixed 16 KiB logical chunk (options 4 KiB, 64 KiB; refuse CDC). ublk logical block size 4096 (`logical_bs=physical_bs=4096`) with `UBLK_PARAM_TYPE_DISCARD`. Dirty tracking at 4 KiB sub-chunk granularity in the journal, merge to full chunks at flush.
2. **Segments**: 8 MiB (4-16 MiB) immutable packed segment objects, extent = `{codec, ulen, clen, ciphertext, tag}`, footer index, LBA-ordered packing within a flush batch; CRC64NVME/CRC32C transport checksum; same format as the local append-only cache.
3. **Hash**: BLAKE3-256, keyed (addressing key) if encrypted; xxh3-128 only inside in-memory tables with full-key verification. SHA-256/HMAC-SHA256 as a "FIPS profile".
4. **Zero**: OR-accumulate scan first; map absent = zero; honor DISCARD/WRITE_ZEROES in the map; verify nothing resurrects across crash (journal the discard).
5. **Compression**: `auto` = lz4 4 KiB probe -> skip if saving <6% else zstd level 1 (default), context-reusing `zstd-safe`; levels selectable (lz4 | zstd -N..19). Fallback to lz4/raw under worker backpressure. Raw if `clen > ulen*0.94`. Optional dictionary later. Background recompression of cold segments at zstd 9-15.
6. **Encryption (optional)**: AES-256-GCM via aws-lc-rs, per-segment subkey (BLAKE3 derive_key of DEK epoch + segment id), counter nonce, per-extent tag, AAD = volume, segment, offset, chunk hash, codec. Keyed-BLAKE3 addressing. KMK wrapped by Argon2id passphrase or KMS key slots; DEK epochs for rotation. Reuse/extract Constellation's `e2e.rs` for key wrapping; keep XChaCha20-Poly1305 for small metadata objects.
7. **Integrity**: verify-on-fetch (AEAD tag, then decompress with ulen cap, then recompute keyed hash), Merkle block map with MAC'd root + generation, generation anchor outside the bucket (CSI object / KMS counter / cross-backend max).
8. **Threads**: pinned ublk queue threads on raw io_uring (SINGLE_ISSUER|DEFER_TASKRUN) for I/O + zero-scan + journal + inline small decompress; tokio multi-thread runtime for S3/control; OS-thread worker pool (rayon-like, cores - queues) for hash/compress/encrypt/decompress of big or batched work; bounded queues with backpressure to the ack; hugepage buffer pools; mimalloc elsewhere. `UBLK_F_USER_COPY` (verify on 7.0) rather than zero-copy.

**Alternatives and trade-offs**
- *4 KiB chunks*: best dedup for VM fleets with many near-identical images; costs 4x map (8-12 GiB/TiB), BLAKE3 SIMD underfill (4 vs 5.8 GB/s), 4x the segment-index entries; only choose if dedup ratio matters more than RAM/CPU.
- *64 KiB chunks*: best for sequential/large-file workloads and cheap map (0.5-0.75 GiB/TiB); 4x more RMW cost per random 4K write and worse dedup.
- *Latency profile "nvme-fast"*: lz4 (or raw), no probe, AES-GCM; decompress ~1 us.
- *Space profile "cold"*: zstd 3-9 at 64 KiB+ chunks, dictionaries, 16 MiB segments.
- *Compliance profile*: HMAC-SHA256 (2.4 GB/s, 2x BLAKE3 cost) + AES-256-GCM through the aws-lc-rs FIPS module; no BLAKE3 (not an approved algorithm); zstd/lz4 are not crypto so unaffected.
- *Integrity-only*: Merkle + plain BLAKE3 + optional Ed25519-signed root, no encryption.
- *Convergent encryption*: rejected (see §5.3).
- *Multi-threaded BLAKE3 per chunk*: pointless below ~1 MiB; use only for segment-level verification/scrub (`update_rayon` 42 GB/s on this host).

### Rust crates (maturity / SIMD / FIPS)

| Crate | Use | Maturity & notes |
|---|---|---|
| `blake3` 1.x | hash, keyed, derive_key | Reference impl by the BLAKE3 authors; runtime AVX2/AVX-512/SSE4.1/NEON dispatch, `rayon` feature for `update_rayon`; `Hasher` is `no_std`-capable. Not FIPS. Already a Constellation dependency. |
| `xxhash-rust` (xxh3 feature) | cache keys | pure Rust, AVX2/NEON intrinsic paths; measured 46-72 GB/s. `xxh3`/`twox-hash` alternatives. |
| `sha2` 0.10 (`asm`/cpufeatures) | SHA-256 | uses SHA-NI at runtime (measured 2.5 GB/s, same as ring). RustCrypto, no FIPS validation. |
| `ring` 0.17 | AES-GCM, ChaCha20-Poly1305, SHA-2, HMAC | BoringSSL-derived asm; measured the fastest AES-GCM; maintenance mode/bus-factor history (revived 2024-25 under new maintainer [recalled]); **no** XChaCha, no GCM-SIV; not FIPS validated. |
| `aws-lc-rs` | drop-in ring-like API + FIPS | AWS-LC (BoringSSL fork) with optional **FIPS 140-3 validated** module (`fips` feature, builds `aws-lc-fips-sys`; needs CMake/Go on some paths); VAES/VPCLMULQDQ paths; AES-GCM, ChaCha20-Poly1305, SHA-2, HMAC, HKDF, AES-GCM-SIV presence UNVERIFIED. Used by rustls by default now. Best default for production. |
| RustCrypto `aes-gcm`, `chacha20poly1305` (incl. XChaCha20Poly1305), `aes-gcm-siv` | portable AEAD | clean API, constant-time, audited (NCC for AES-GCM/ChaCha); runtime AES-NI/PCLMUL, AVX2 ChaCha; but 2-5x slower than ring for AES-GCM here. Constellation uses XChaCha20Poly1305 from this family. |
| `argon2` | password KDF | as Constellation. |
| `zstd` / `zstd-safe` | compression | see §4.5; `zstd-sys` bundles libzstd, version tracks 1.5.x. |
| `lz4_flex` | compression | pure Rust, safe-mode available; no HC. |
| `crc32fast`, `crc32c`, `crc64fast-nvme` | checksums | PCLMUL/SSE4.2 accelerated; `crc64fast-nvme` matches S3's CRC64NVME [recalled name]. |
| `mimalloc`, `tikv-jemallocator` | allocator | both mature. |
| `io-uring` (tokio-rs), `libublk` (ublk-org) | ublk/io_uring | `libublk` is the Rust ublk binding by ublk's maintainer; see other Lunix notes. |
| `zeroize`, `secrecy`, `region`/`libc::mlock` | key hygiene | |

## Open questions / UNVERIFIED

- All **[recalled]** figures (FastCDC speedups, zstd/lz4/snappy README ratios, S3 pricing, ZFS volblocksize default, dictionary gains, CRC64NVME) were not re-fetched; confirm before putting in public docs.
- My benchmark used a synthetic "textish" corpus; real ratios/speeds on ext4/xfs metadata, DB pages, VM images, and logs must be measured (suggest running `zstd -b`/`lzbench` over a sampled block-device dump: dd of a real root FS image into 16 KiB chunks). Hot-cache, unpinned, P-core boosted; real systems are slower. `zstd::bulk::compress` allocates a context per call, so 4 KiB zstd numbers understate a reused-context implementation.
- RustCrypto `aes-gcm` being 5x slower than ring: confirm against current release and whether `RUSTFLAGS=-C target-feature=+vaes,+vpclmulqdq` (or the unstable VAES support) changes it; confirm aws-lc-rs matches ring (expected) and whether it exposes AES-GCM-SIV / XChaCha20-Poly1305 (believed not XChaCha).
- ublk on Linux 7.0: exact flag set (`UBLK_F_USER_COPY`, `UBLK_F_AUTO_BUF_REG`, shared-memory zero-copy additions, multi-queue affinity ioctls) and recommended queue depth should be confirmed against `include/uapi/linux/ublk_cmd.h` of the running kernel and `Documentation/block/ublk.rst`.
- BLAKE3 with AVX-512 / on server CPUs not measured here (no AVX-512 on 14900K).
- Rollback/freshness anchor design (monotonic generation outside S3) is a design decision shared with the multi-backend and CSI work; not resolved here.
- Cold-cache RMW on S3-only chunks: delta-overlay vs synchronous fetch needs a latency/complexity decision with the cache/journal design.
- Whether to put the 128-bit-truncated hash in map entries (space) vs full 256-bit (security margin) is unsettled.
- Dedup ratio vs chunk size on real Lunix target workloads (container images, DB volumes, VM disks) is not measured; build a trace-driven simulator over real volume dumps (compute dedup ratio at 4/16/64 KiB, with/without compression) before locking the default.
- GC/refcount for deduplicated chunks inside immutable packed segments (live-fraction tracking, compaction write amplification) is the main cost of the two-level scheme and is out of scope here.

## Sources

Local code and docs:
- /home/bra/cvs/replicix/constellation/crates/store-s3/src/e2e.rs (Argon2id KEK, master key, `derive_from_master` via keyed BLAKE3 with purpose bytes, XChaCha20-Poly1305 random 24 B nonce envelope, AAD = object identity)
- /home/bra/cvs/replicix/constellation/docs/explanation/DESIGN.md (§chunks L161-190: chunk size, blake3 plaintext hash identity, codec header, compress-then-encrypt, incompressible guard, worker-pool compression; §8 L1725-1745: E2E, keyed addressing; L1630 decompressed chunk cache)
- /home/bra/cvs/replicix/constellation/crates/store-s3/src/{packs.rs,format.rs,decode_gate.rs}, crates/mtree, docs/plans/v1/done/27-merkle-packed-checkpoints.md, docs/security-audit-2026-09.md
- /home/bra/cvs/replicix/constellation/Cargo.toml (blake3 1, zstd 0.13)
- Benchmark harness (scratch, not in repo): /tmp/claude-1000/-home-bra--claude-brainaction-spares-spare-wsibSN/edcf18bd-0d41-43af-a9e7-e0e795a97276/scratchpad/bench/ (src/main.rs, out.txt)

External (from memory; not fetched this session):
- FastCDC: Xia et al., "FastCDC: a Fast and Efficient Content-Defined Chunking Approach", USENIX ATC 2016.
- Meyer & Bolosky, "A Study of Practical Deduplication", FAST 2011. Jin & Miller, "The Effectiveness of Deduplication on Virtual Machine Disk Images", SYSTOR 2009.
- BLAKE3 spec and crate: https://github.com/BLAKE3-team/BLAKE3 , https://docs.rs/blake3
- xxHash/XXH3: https://github.com/Cyan4973/xxHash ; Rust: https://docs.rs/xxhash-rust
- zstd benchmarks and dictionary docs: https://github.com/facebook/zstd (README, `lib/zdict.h`); lz4: https://github.com/lz4/lz4 ; `lz4_flex`: https://docs.rs/lz4_flex ; `zstd-safe`: https://docs.rs/zstd-safe
- ring: https://github.com/briansmith/ring ; aws-lc-rs: https://github.com/aws/aws-lc-rs (FIPS feature); RustCrypto AEADs: https://github.com/RustCrypto/AEADs
- NIST SP 800-38D (GCM nonce limits); RFC 8452 (AES-GCM-SIV); RFC 9106 (Argon2).
- Linux ublk: Documentation/block/ublk.rst and include/uapi/linux/ublk_cmd.h; https://github.com/ublk-org/libublk-rs
- AWS S3 pricing and additional checksum algorithms (CRC32C, CRC64NVME): https://aws.amazon.com/s3/pricing/ , https://docs.aws.amazon.com/AmazonS3/latest/userguide/checking-object-integrity.html
- OpenZFS volblocksize default change (2.2); Btrfs compression heuristic (fs/btrfs/compression.c, heuristic.c).
