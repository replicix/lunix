# Compact encoding for block-map leaves

Date: 2026-11-11
Author: research agent (Sonnet)
Question: The Lunix block map is a fixed-radix Merkle tree (fanout 512) whose
leaf entries are 48 B (`chunk_id` 32 B + location hint 16 B), about 768 MiB of
leaves per TiB at 64 KiB chunks. The owner wants this much smaller without
losing what ADR-6/7/8/11 buy (verification, dedup, stale-hint recovery,
O(changed) snapshot diff, byte-identical mirrors), and without making the
read path's leaf lookup slow. How do real systems encode block/extent maps,
which integer-compression schemes fit, what happens to integrity if the
id is truncated or dropped, and what do the candidate encodings actually
measure on synthetic 1 TiB maps?

## Summary

- **Of the 48 B per entry, 32 B is a BLAKE3 hash that no codec can compress.**
  Status quo + zstd-1 only reaches 631-649 MiB/TiB (-16% to -18%) on
  full volumes; zstd-3 and lz4 are no better. A leaf stored zstd-compressed
  also cannot be point-looked-up: 16 us per lookup (zstd-1, measured) versus
  18 ns on the raw row, so the leaf cache must hold the 24 KiB decoded form.
- **The hint (segment, offset, clen) is highly compressible because of how
  Lunix writes.** Segments are packed in LBA order within a destage batch, so
  consecutive chunks are usually contiguous in one segment. A *run*
  encoding (seg, start offset, count) with a per-leaf segment dictionary,
  bit-packed fixed-width columns and one `clen` column shrinks the hint
  from 16 B to **2.4 B/entry** sequential and **6.0 B/entry** in the
  fragmented worst case (30% random overwrites, 262 runs per leaf, 123
  segments per leaf).
- **Measured MiB/TiB at 64 KiB chunks** (status quo 768):

  | encoding | a-seq | b-frag | c-clone | d-mixed | e-sparse (50% holes) |
  |---|---:|---:|---:|---:|---:|
  | `runs-bp32` (full id) | 550 | 608 | 555 | 550 | 277 |
  | **`runs-bp16` (16 B id prefix)** | **294** | **352** | **299** | **294** | **149** |
  | `runs-bp8` (8 B id prefix) | 166 | 224 | 171 | 166 | 85 |
  | **`runs-bp0` (no id)** | **38** | **96** | **43** | **38** | **21** |

  At 16 KiB chunks everything is 4x larger (`row32` 3072; `runs-bp16`
  1158-1392; `runs-bp0` 134-368).
- **Lookup on the encoded leaf is cheap enough to cache it encoded:** bit-packed
  runs `runs-bp16`: 121 ns mean (sequential), 181 ns (fragmented, binary search
  over ~262 runs); 50-120 ns without ids. Decoded array lookup is 2.4 ns; full
  leaf decode 0.8-3.4 us. Varint run streams are 10-25x slower to look up
  (1.1-2.7 us: they must scan) and no smaller than bit-packed columns; an
  outer zstd or lz4 over the bit-packed form saves nothing (+0.1-0.5% size) and costs 9-11 us
  more to encode and 250-400 ns extra per lookup. **Do not compress the new format.**
- **Truncating the id to 16 B is safe in every mode; 8 B is not** (section 4).
  With 8 B an unkeyed-mode guest can grind a prefix collision in 2^32 work,
  and an adversarial bucket in keyed mode finds a swappable chunk pair with
  probability 0.5 once a family holds 2^32 distinct chunks (256 TiB at
  64 KiB), 2^-9 at 16 TiB. 16 B gives 2^-65 at 256 TiB. Dedup is unaffected
  for any length because it always uses the full id from the index/footers.
- **Dropping the id entirely (38-96 MiB/TiB) is viable but is a different
  security design:** nothing then binds LBA to content. It needs either
  position-bound AEAD (extent AAD includes its own `(seg, off)`, so
  compaction must re-encrypt), or an authenticated segment table plus a
  footer fetch on cold reads, plus a forwarding table for stale hints. It
  also loses the exact content diff and the stale-hint recovery key.
- **Slot references (`(seg, slot)`) shrink the leaf to 0.9-45 MiB/TiB but
  just move the bytes:** the footer-side `(offset, clen)` table is 82-107
  MiB/TiB (bit-packed, 5.1 B/extent), more than the 38-96 MiB/TiB a leaf with
  clen columns and derived offsets costs in total, and a cold read needs the
  footer first (+1 GET, 23 KiB at 64 KiB chunks, 88 KiB at 16 KiB).
- **Recommendation:** adopt the run + bit-packed column leaf (call it LF2),
  with the id length as a per-volume parameter `id_len in {0, 16, 32}`.
  Ship **`id_len = 16` as the default** (294-352 MiB/TiB, -54% to -62%,
  every ADR-8 property preserved, no new mechanism) and specify `id_len = 0`
  as a "lean" profile (38-96 MiB/TiB, -87% to -95%) gated on two decisions
  (section 8). No outer compression of leaves. Cache leaves encoded.
- The synthetic maps, the codecs and the harness are in
  `lunix/spikes/leafenc/` (Rust, ~1000 lines, all variants round-trip tested
  and lookup-tested against the source entries).

## 1. How real systems encode block and extent maps

Per-entry cost of each system, normalised to "bytes of map per TiB of
addressed space at the system's native granule". "V" = verified from the
primary source fetched for this note; "M" = from memory, not re-fetched.

| System | Granule | Map entry | Content id in the entry? | Notes |
|---|---|---|---|---|
| qcow2 L2 (V) | 64 KiB cluster (default) | 8 B: bit 63 refcount-is-one, bit 62 compressed, bits 9-55 host offset (cluster-aligned) | no | 128 MiB/TiB. Extended L2 (subclusters): 16 B = 8 B standard + 32-bit allocation bitmap + 32-bit zero bitmap, so 32 subclusters per cluster. Compressed cluster: offset (x bits, unaligned) + sector count in the same 64 bits, `x = 62 - (cluster_bits - 8)`. |
| dm-thin (V size, M packing) | 64 KiB-1 GiB, user chosen | btree value is one 64-bit word: block number in the high bits, 24-bit "time" (snapshot epoch) in the low bits (`pack_block_time`, `drivers/md/dm-thin-metadata.c`) | no | Kernel doc sizing rule: metadata = 48 x data / block size, i.e. 48 B per data block, which at 64 KiB blocks is 768 MiB/TiB, the same number as Lunix's current leaf. The 48 B includes btree overhead, the data space map and snapshot sharing, not just the 8 B value. btree nodes are 4 KiB with parallel key and value arrays (struct of arrays inside the node). |
| VDO block map (V) | 4 KiB | **5 B**: 36-bit PBN + 4-bit state (1 unmapped, 1 uncompressed, 14 = slot in a compressed block of up to 14 fragments) | no | 812 entries per 4 KiB page; radix tree with 60 roots. Dedup uses a separate index (UDS) that is only advisory: a hit is verified by reading and comparing the data. This is exactly the "location only in the map, ids elsewhere" design (option 3). 5 B per 4 KiB = 1.25 GiB/TiB; the same 5 B at a 64 KiB granule would be 80 MiB/TiB. |
| ZFS blkptr_t (V) | 512 B-16 MiB records | **128 B**: 3 DVAs x 16 B (vdev, asize, offset, gang), 1 properties word (lsize, psize, comp, cksum type, type, level, flags), physical + logical birth txg, fill count, **256-bit checksum** | yes, 32 B, in the parent | Big because it is a self-validating tree (checksum in parent = Merkle), carries up to 3 ditto copies, and two birth txgs for snapshots/send/dedup. Embedded and "encrypted" variants repurpose DVA words. At 128 KiB records 128 B is ~1 GiB/TiB. This is the fully "ids in the pointer" design Lunix has today. |
| Btrfs EXTENT_DATA (V) | variable extents | item header 25 B + 53 B regular extent (generation, ram_bytes, compression/encryption/encoding, type, disk_bytenr, disk_num_bytes, offset, num_bytes) | no (CRC32C per 4 KiB in a separate csum tree) | 78 B per extent, amortised over extent length; the csum tree is 4 B / 4 KiB = 1 GiB/TiB. |
| bcachefs extent (partly V) | variable | 64-bit `bch_extent_ptr` (offset, 8-bit generation, device, unwritten flag) plus optional `crc` entries (compressed_size, uncompressed_size, offset, csum_type, compression_type, checksum) | optional checksum | Pointers are 64-bit and a key can hold several; the `gen` byte detects stale pointers after bucket reuse (a cheap, non-cryptographic form of "this location was recycled"). The PDF fetch was truncated, so field widths beyond that are M. |
| LSVD (paper read at abstract level only) | sector extents | in-RAM extent map (LBA range -> object seq, offset, len), rebuilt at start from object headers + checkpoints; no persistent tree | no | Object names carry a monotonic sequence number, GC rewrites live data when an object drops under a threshold (same as Lunix segments). Entry layout not verified. |
| Ceph RBD object map (M) | 4 MiB object | 2 bits per object (nonexistent / exists / pending / clean) | no | Location is computed (object name -> CRUSH), so the map holds only existence. The lesson for Lunix is that anything derivable should not be stored; Lunix segments are not computable from LBA, so this is only a limit case. |
| Neon pageserver (M, secondary sources) | key x LSN layers | layer map in RAM; delta layers carry an on-disk B-tree index mapping (key, LSN) -> blob offset; image layers index key -> blob offset | no | Fixed-width B-tree keys, offsets into a blob file. Not verified against source. |
| restic (V) | blob | pack header entry 37 B (type 1 + encrypted length 4 + SHA-256 32) or 41 B with the plaintext length; index JSON `{id, offset, length, uncompressed_length}` | yes (32 B) | The id is the key and the entry is a locator; the index is the only map. |
| Kopia index v2 (V) | content | sorted fixed-size entries: key (fixed `keySize`, e.g. 32 B hash + prefix) + **16-19 B value**: 4 B timestamp, 4 B pack offset (31-bit mask, deleted flag in the top bit), 3 B original length, 3 B packed length, 2 B pack-id index into a per-index pack table (+1 optional byte), optional format id | yes | Variable-width tail bytes appear only when needed (more than 65,536 packs, lengths over 16 MiB, more than one format). Pack names are dictionary-coded (5 B record each). Lookup is binary search over fixed-size records. This is the closest analogue to option 4/5: dictionary the segment, narrow integer fields, optional high-bit bytes. |
| LevelDB/RocksDB block (V for LevelDB entry format, rest M) | key/value | entry = varint32 `shared`, `non_shared`, `value_len` + key delta + value; every K-th key is a **restart point** with `shared = 0`; trailer = array of u32 restart offsets + u32 count; lookup = binary search on restart array, then linear scan of <= K entries. RocksDB: `block_restart_interval` 16 for data, 1 for index blocks; format_version >= 4 delta-encodes index block handles (offset relative to previous handle); optional data block hash index. | n/a | The model for "compressible but still point-lookupable": restart points bound the scan. |

What this survey says about Lunix:

1. Nobody except ZFS (and Lunix today) stores a 32-byte content hash next
   to every location in the map. qcow2, dm-thin, VDO, Btrfs, bcachefs,
   LSVD, Ceph all store 5-8 B per granule and verify elsewhere (separate
   checksum tree, advisory dedup index, header in the data).
2. The densest practical per-entry encodings (VDO 5 B, qcow2 8 B) are
   fixed width and O(1) indexable; they pay no scanning cost because the
   page *is* an array. Kopia and LevelDB show the next step: dictionary the
   container (pack id / shared prefix), narrow the integers, and add
   a small restart structure.
3. Compressed-extent maps (qcow2 compressed descriptor, VDO 14 fragment
   slots, Kopia packed length) all keep the *length* in the entry. Lunix
   needs `clen` for the same reason (the ranged GET length).
4. dm-thin's "48 B per block" is a useful sanity check: Lunix's 48 B/entry
   is in line with a general-purpose thin-provisioning design, not an
   outlier, but it can be made 5-10x smaller because Lunix leaves are
   immutable and written in LBA order.

## 2. Integer compression schemes that fit

A leaf is 512 values per column, small enough that the choice is between
"bitpack a fixed-width column" and "a byte-oriented varint". The
important numbers (all single thread, from the cited papers/READMEs; our
own measurements are in section 7):

| Scheme | Idea | Speed (published) | Random access | Fit |
|---|---|---|---|---|
| Frame-of-reference + bitpacking | subtract a per-block minimum, store every value in `ceil(log2(max-min+1))` bits | `bitpacking` crate (Rust port of Lemire's simdcomp): BitPacker4x 5.3 G int/s compress, 5.5 G int/s decompress, 5.0 G int/s delta decode on an i5-8250U; 8x (AVX2) 7.0 / 6.5 / 5.6 G int/s; blocks of 128 / 256 integers | **O(1)**: value `i` is at bit `i*w`; one unaligned 64-bit load, shift, mask | Best fit. A scalar `get(i)` is ~1-2 ns; SIMD matters only for full-leaf decode, and a 512-entry column is 4 blocks of 128. |
| PFor / NewPFD / SIMD-FastPFOR (Lemire & Boytsov, arXiv:1209.2137) | bitpack at width `b` that covers most values, store the exceptions separately | SIMD-BP128 "nearly twice as fast as the previous fastest" schemes; SIMD-FastPFOR within 10% of Simple-8b ratio at ~2x its speed | needs exception patching; O(block) | Unneeded: Lunix columns have a tight range (clen < 2^17, offsets < 2^24) and outliers are rare enough that one width per leaf is fine. Reconsider only if clen has long-tail outliers. |
| Stream VByte (Lemire, Kurz, Rupp, arXiv:1709.08990) | control bytes in one stream, data bytes in another so SIMD can shuffle | > 4 G differentially coded int/s on a 3.4 GHz Haswell; up to 2x varint-G8IU | none (needs prefix scan of control stream) | Useful for large sorted posting lists. For 512 values with 2-3 B each, scan cost ~ O(n). `stream-vbyte` 0.4.1 on crates.io needs SSSE3/SSE4.1. Not chosen. |
| LEB128 varint | 7 bits/byte | ~1 G int/s scalar | none | what our `runs-vi*` variants use; 3 B for any clen >= 16,384, i.e. the common case at 64 KiB chunks. |
| Elias-Fano (Vigna, "Quasi-succinct indices", arXiv:1206.4300) | monotone sequence of n values < u in `n(2 + log(u/n))` bits, `select` in O(1) with a rank/select index | designed for inverted indices | O(1) select | Applies to monotone columns: cumulative offsets within a run. For 512 values the bitpacked prefix-sum with a restart every 16 is simpler and as small. Keep as an option for the *segment dictionary* if leaves ever span thousands of segments. |
| Delta / delta-of-delta | store differences (second differences for linear sequences) | free | needs a prefix sum; restart points bound it | Offsets inside a segment are delta-of-delta zero for contiguous runs: `off[i+1] = off[i] + HDR + clen[i]`. That is why run encoding removes the offset column entirely. |
| Run-length / extent encoding | `(start, count)` for contiguous mappings | free | binary search over runs | The winner for sequential data; in fragmented data every entry is its own run and the run record overhead has to be small. |
| Dictionary | small integer index into a per-leaf table | free | O(1) | for segment numbers: a leaf touches 2 segments sequentially, ~120 when fragmented (measured). |
| Generic: zstd 1/3, lz4 | entropy + LZ77 over the encoded bytes | zstd-1 ~0.4-2 GB/s (DESIGN §5) | none, must decompress the whole leaf | Needed only when the leaf still has redundancy (rows, columns, varint). Does nothing on bitpacked columns (measured: <1% gain) and cannot compress a hash. |

Rust crates considered: `bitpacking` 0.9.3 (SSE3/AVX2/NEON, 128/256 int
blocks, with delta variants; blocks are fixed-size so a 512-entry column of
`clen` fits 4x BitPacker4x blocks), `stream-vbyte` 0.4.1, `integer-encoding`
(plain LEB128), `zstd` 0.13, `lz4_flex` 0.11. The benchmark below uses a
hand-written scalar bit reader/writer because (a) point lookup is what
dominates and (b) `bitpacking` has no random-access API (you unpack a whole
128-int block, ~25 ns). It remains the right tool for a bulk decoder if a
leaf must be fully expanded (see section 6.3 for the measured full-decode
cost of the scalar decoder: 3 us per leaf, i.e. ~0.15 G entries/s, which is
well under any I/O budget and does not justify SIMD).

## 3. Design options for Lunix leaves

All sizes below are MiB per TiB of *logical* volume at 64 KiB chunks
(16.8M entries, 32,768 leaves) unless stated, from section 7. "seq / frag"
means workload a / b.

### 3.0 Leaf formats that were implemented and measured

| name | layout | point lookup |
|---|---|---|
| `rowT` | 512 fixed records `[id[T] \| seg u64 \| off u32 \| clen u32]` (T=32 is the status quo, 48 B) | direct index, 1 load |
| `row-slotT` | `[id[T] \| seg u64 \| slot u16]` (10+T B) | direct index; needs footer resolve |
| `colT` | struct of arrays: ids, seg[512], off[512], clen[512] | direct index |
| `col-deltaT` | as `colT` but `seg` zigzag-delta vs previous entry, `off` stored as delta from the expected contiguous offset (`prev.off + HDR + prev.clen`) | **scan** (prefix sums) |
| `runs-bpT` | 19 B header; segment dictionary (min + fixed-width `seg - min`); run table of fixed-width records `[start:9 \| seg_idx:log2(nseg+1) \| start_off:w \| rank:10]`; `clen` column (w = bits(max clen), 17 at most); a `cum` column holding the running sum of `HDR + clen` every 16 data entries; ids for non-hole entries only | binary search over runs, then `clen[rank]` and an offset sum of <= 15 adds from the nearest `cum` restart |
| `runs-bp-slotT` | as `runs-bpT` but `start_off` is the start slot, no `clen`/`cum` columns | binary search; slot = start + delta |
| `runs-viT` | same logical content as `runs-bpT`, LEB128 varints: run `(len<<1\|hole)`, zigzag seg delta, start offset; then a varint `clen` stream; ids | **scan** of runs and of the `clen` stream |

`HDR` = 64 B models the extent header plus AEAD tag; a run continues while
the next entry is in the same segment at `off + HDR + clen` (or `slot + 1`).
Holes are runs. Everything is wrapped optionally in `+zstd1`, `+zstd3` or
`+lz4` (lz4 via `lz4_flex`).

### 3.1 Option 1: status quo, 48 B + generic compression

768 MiB/TiB raw. zstd-1 gives 631 (seq), 649 (frag), 635 (clone), 630
(mixed); lz4 702-712. Only the sparse workload falls (318), and only because
the half-empty leaves contain zero rows. The 32 B hash is 2/3 of the bytes
and incompressible, so the ceiling is -18%. This also corrects DESIGN §7:
"dirty_leaves x 24 KiB (compressed)" is really ~20 KiB.

Lookup in a compressed leaf must decompress all 24 KiB: **16.3 us (zstd-1),
3.2 us (lz4)** against 18 ns for the raw row. So the cache has to hold a
decoded 24 KiB leaf (or pay that on every read), and a cold leaf costs a
20 us decode (zstd-1) before the first lookup.

### 3.2 Option 2: truncated id (16 B or 8 B) + hint

Row layout: 512 (16 B id), 384 (8 B id). With zstd-1 on rows: 370-389 (16 B)
and 234-257 (8 B), but then lookup costs 9.5-13 us. With run + bit-packed
columns: **294-352 MiB/TiB (16 B)** and 166-224 (8 B), at 121-235 ns per
encoded lookup. The id is then 87% (16 B) or 77% (8 B) of seq-leaf bytes.
Security analysis in section 4; short version: 16 B is safe, 8 B is not
safe for unkeyed families or large keyed ones.

### 3.3 Option 3: location only, no id

`row0` 256 (the raw 16 B hint), `runs-bp0` **38 / 96 / 43 / 38 / 21**. This
is 87-95% below the status quo, and seq leaves become 1.2 KiB. What breaks is
not size related: LBA <-> content binding, stale-hint recovery, exact content
diff and cache keying by `chunk_id`. See 4.5 for the three ways to rebuild
verification and 8 for the consequence per document.

Effects asked for in the brief:

- **Verification.** Today the reader compares the recomputed hash of the
  plaintext to the *map's* id. Without it only extent-internal checks remain
  (AEAD, header id == hash of payload). A valid extent from anywhere passes.
- **Snapshot diff.** Still O(changed) over Merkle hashes. Entries compare by
  placement, as they do today because the hint is in the leaf; with an
  id the diff could (but today's design does not) treat a pure compaction
  move as unchanged.
- **Mirror comparison.** Unchanged: byte-identical copies have identical
  roots.
- **Compaction / stale hints.** The `moved{chunk_id -> hint}` record keys on
  the old `(seg, off)` instead (cheaper). Old snapshots cannot fall back to
  the chunk index; they need a forwarding table or the source segment kept.

### 3.4 Option 4: slot reference `(segment, slot-in-footer)`

`runs-bp-slot0`: 0.9 / 45 / 4.9 / 0.9 / 1.7 MiB/TiB (leaf bytes: 30 B
sequential). With a 16 B id: 257-301. A leaf that sequentially filled 512
chunks costs 30 bytes; a fragmented one 1.4 KiB.

But the information is not gone, it has moved to the footer:

- Footer must be in **write order** to make `slot` meaningful (DESIGN §6
  says the footer is *sorted by chunk_id*; that would need a permutation or
  a second table).
- Offset + clen per extent, bit-packed (24 + 17 bits = 5.1 B per extent,
  counting dead extents too): **82 MiB/TiB (seq), 107 (frag), 25 (clone)**.
  `leaf + sidecar` = 83 / 152 / 30 / 83 / 43 against `runs-bp0` 38 / 96 /
  43 / 38 / 21, i.e. **slot is larger in total** in every workload except
  clone (which wins because dedup'd entries share the base image's sidecar).
- The full footer with ids is ~44 B/extent: 704 MiB/TiB (seq), 915 (frag).
  If it is also needed for verification (option B in 4.5) a local copy costs
  as much disk as the old leaves.
- Cold read: read leaf, then **footer (23 KiB at 64 KiB chunks; 88 KiB at 16
  KiB chunks; ranged GET at the tail via trailer), then extent**: +1 serial
  GET per never-seen segment, i.e. +1 RTT (30-100 ms S3 Standard in the
  design's own numbers; not measured here). Warm segments pay a footer
  lookup in RAM/disk (2-20 ns in the harness).
- Benefits: leaf rewrites at commit are tiny (30 B - 1.4 KiB versus 20 KiB),
  and the sidecar is shared by every snapshot instead of repeated in each
  rewritten leaf.
- Verdict: a pure space win only if footers are guaranteed locally cached;
  otherwise it trades 1 GET for bytes. Not recommended for v1. If a
  commit-size win is wanted, the cheaper form is the lean leaf (1.2-3 KiB).

### 3.5 Option 5: run / extent encoding with bit-packed columns

This is the winner. Measured breakdown for a fully sequential leaf
(`runs-bp0`, 1.2 KiB): header 19 B + dictionary ~12 B + 2 run records ~13 B +
`clen` column 512 x 17 bits = 1088 B + `cum` 32 x 25 bits = 100 B. So the
floor for the offset-mode leaf is the `clen` column (2.1 B/entry), not the
run table. In fragmented leaves (262 runs, 123 segments): 50 bits per run
record x 262 = 1.6 KB, `clen` 1.1 KB, `cum` 0.1 KB, dictionary 0.2 KB, i.e.
3.0 KB = 6.0 B/entry.

- **Bit-packed vs varint:** `runs-vi0` is 47 / 109 / 52 / 46 / 25 versus
  `runs-bp0` 38 / 96 / 43 / 38 / 21 (24% larger: `clen` >= 16,384 costs 3
  varint bytes versus 17 bits), and its lookup is 1.2 us (seq) to 2.7 us
  (frag) versus 49-118 ns. Varint+zstd-1 (`runs-vi0+zstd1`) reaches 39 / 92
  but at 3.2-5.8 us per lookup.
- **Restart points** (`cum` every 16 data entries, 25 bits each, 100 B/leaf)
  bound the offset sum to 15 additions. Without them an offset needs a sum
  over the whole run prefix (up to 511 values). The cost is 0.3 B/entry.
- **Delta-coded segment numbers vs dictionary:** the dictionary (min +
  fixed-width offsets, index per run) gives O(1) segment resolution and
  absorbs the `epoch<<40` jump (a leaf spanning epochs only widens each
  dictionary value to ~41 bits, <= 0.6 KiB in the worst fragmented case; the
  harness uses a single epoch). Delta coding would need a prefix sum.
- **Holes** as runs cost one run record each; the sparse workload shows
  half-empty leaves at 149 (16 B) / 21 (lean) MiB/TiB.
- **Encode / decode:** 3-10 us to encode, 0.8-3.6 us to fully decode a leaf
  (scalar code, no SIMD), versus 2 / 4 us for the raw row and 20 us for the
  zstd-1 row. This is fast enough that a dirty leaf costs nothing at a
  commit.
- **Do not wrap in zstd/lz4:** +0.1-0.5% size, +9-11 us encode, +250-400 ns
  per lookup.
- `bitpacking`/SIMD crates are not needed for lookups (they unpack 128
  integers at a time and have no random-access API). They are an option if
  full-leaf decode ever shows up in a profile.

### 3.6 Option 6: column-oriented + zstd versus row + zstd

Row + zstd-1 and column + zstd-1 are within 1-3% of each other
(`row0+zstd1` 94, `col0+zstd1` 97; `row32` 631, `col32` 634). Delta columns
(`col-delta0+zstd1`: 42 / 104 / 49 / 38 / 26) approach `runs-bp0` on size
(38 / 96 / 43 / 38 / 21) because zstd learns "contiguous" from zero deltas,
but lookup needs full decompress + prefix scan (3.9-8.6 us measured for
columns; `col-delta0` raw lookup alone is 450-580 ns) and encode is 10-20 us.
Conclusion: a columnar zstd leaf is a fine *archive* format, but a
bit-packed run leaf is smaller or equal, 50-100x faster for point lookup and
needs no generic compressor.

### 3.7 Additional options considered

- **7. Parametrised id length (`id_len`).** Because the id column is the only
  part that varies in cost (16.8M x id_len bytes: 256 MiB for 16 B, 128 for 8
  B, 512 for 32 B), make it a volume parameter stored in the MAC'd `volume`
  object and a header byte of the leaf. Same code for all.
- **8. Hot/cold split of ids.** Keep the id out of the leaf and in a
  *parallel* per-leaf "id sidecar" object that is fetched only when needed
  (verification can be deferred and batched). Costs the same bytes in the
  bucket but keeps the hot-path leaf at 1-3 KiB. Not measured: it is the
  same trade as option 3 plus a mechanism, and the id must be fetched on
  every cold read anyway if verification is synchronous.
- **9. Sub-clen precision.** `clen` is 55% of a seq leaf with no ids. Storing
  `clen` quantised to 256 B for the GET length (over-read < 256 B) and keeping
  exact offsets via a header walk would cut the leaf by half again (~20
  MiB/TiB) but means a cold read of chunk k inside a run must GET from the
  nearest restart and walk extent headers. Not worth the complexity at
  38 MiB/TiB.

## 4. Security and integrity analysis of truncating or dropping the id

### 4.1 What the 32-byte id in the leaf does today

Facts from the design (DESIGN §5-§7, ADR-6/8/11), restated because the
analysis depends on exactly which of them hold:

- `chunk_id = BLAKE3(plaintext)`, or `BLAKE3-keyed(addressing_key, plaintext)`
  when the family is encrypted.
- Every extent header carries the full `chunk_id`. When encrypted, the AEAD
  AAD includes `chunk_id`, so a bucket cannot forge an extent, and cannot
  move a valid extent's payload under another id.
- The read path verifies: AEAD tag, decompress with a `ulen` cap, then
  recompute the id from the plaintext and compare it with the **map's**
  id (ADR-11 "Integrity"). The map's id is the only thing that binds an
  LBA to a specific content. Extent headers and footers bind id <-> bytes,
  not LBA <-> id.
- Commits are MAC'd (encrypted volumes), so the root hash, and through the
  Merkle tree every leaf byte, is authentic. **For unencrypted volumes
  nothing authenticates the root** (ADR-11 only MACs encrypted
  commits; DESIGN §22 lists the rollback anchor as open). The only
  adversary an unencrypted volume's ids defend against today is therefore
  corruption or an adversary who cannot rewrite the commit chain
  consistently (e.g. someone who can alter one segment object but not the
  commit/map objects), or one whose root is pinned externally by the
  operator.
- Dedup (§5 step 4) looks up the **full** id in the chunk index; the index
  is rebuilt from footers (full ids).
- Stale-hint fallback (§7) looks up the full id in the chunk index.
- Snapshot diff and mirror comparison use Merkle child hashes only. Note
  that because the *hint* is inside the leaf, two leaves with identical
  content but different physical placement already hash differently. The
  root is **not** a content-only identity today; it is a (content,
  placement) identity. Dropping ids does not change this.

### 4.2 Threat models

| Adversary | Capability | Relevant to |
|---|---|---|
| T1 corruption | random bit rot, truncated or misdirected reads (wrong range, wrong object), software bugs | all volumes |
| T2 bucket, encrypted+keyed | reads and writes any object, knows all ciphertext, extent headers, ids (keyed hashes, so no plaintext relation), cannot forge AEAD or commit MAC | encrypted volumes |
| T3 bucket, unencrypted | as T2 but can also compute ids of any plaintext it wants (unkeyed BLAKE3) and can rewrite commits (no MAC) unless the operator pins the root | unencrypted volumes |
| T4 hostile guest | writes arbitrary data into the volume, can grind plaintext to find hash-prefix collisions if the id is unkeyed (it does not know the addressing key if keyed) | multi-user VMs, CI runners |

### 4.3 Option 2: keep a t-byte prefix of the id

The leaf stores `tid = id[..t]`. Read verification becomes: extent header
id (full, authenticated by AEAD AAD when encrypted) must have prefix `tid`,
and the recomputed id of the plaintext must equal the header id (full
comparison against the **extent header**, which the leaf's prefix has
vouched for). So the end-to-end check is still a full-length hash check
of payload against the header, plus a t-byte binding of header to map.

Security of the binding for each adversary:

- **T1.** Random corruption or misdirection passes the binding with
  probability `2^(-8t)`: 2^-64 for t=8, 2^-128 for t=16. Both are
  negligible. A *misdirected read* returns another real extent whose id is
  independent, so the same probability applies.
- **T2 (encrypted, keyed).** The bucket's only move is a **swap**: serve a
  valid extent for chunk Y at the location hinted for chunk X. It succeeds
  iff `Y_id[..t] == X_id[..t]`. The bucket can see all ids, so it can
  search for prefix collisions among the chunks that actually exist.
  Expected number of colliding pairs among `n` stored chunks is
  `n^2 / 2^(8t+1)`:

  | n (distinct chunks in the family) | data at 64 KiB | t = 8 | t = 16 |
  |---|---|---|---|
  | 2^24 | 1 TiB | 2^-17 (8e-6) | 2^-81 |
  | 2^28 | 16 TiB | 2^-9 (0.2%) | 2^-73 |
  | 2^32 | 256 TiB | 0.5 | 2^-65 |
  | 2^36 | 4 PiB | ~2^7 pairs | 2^-57 |

  For 8 B the bucket has a real chance of finding a swappable pair at
  the PiB scale Lunix wants to support (and with 16 KiB chunks the same n
  arrives at 4x lower capacity). A swap silently returns valid but wrong
  data for an LBA: an integrity break, not a confidentiality break.
  16 B is out of reach forever. The attacker cannot create new colliding
  chunks (it has no addressing key), only exploit natural ones.
- **T3 (unencrypted, unkeyed).** The bucket needs a *second preimage of a
  specific known 64-bit prefix* to substitute arbitrary content: ~2^64
  BLAKE3 evaluations. With BLAKE3's tree structure an attacker that
  varies only the last 1 KiB chunk of a 64 KiB message re-hashes ~1/64 of
  the data per trial, so the cost is ~2^64 x (a 1 KiB compress + parent
  merges), roughly 2^70 byte-hashes. That is expensive but not out of
  reach for a funded attacker (Bitcoin mining runs at ~2^67 hashes/s).
  t = 16 gives 2^128. Even so, T3 can rewrite the unauthenticated
  commit chain, so the id is only a defence when the root is pinned
  out of band.
- **T4 (unkeyed).** A hostile guest can grind two chunks with equal
  t-byte prefix in 2^(4t) work: **2^32 for t=8 (hours on one core,
  seconds on a GPU)**, 2^64 for t=16. The colliding pair is stored in the
  guest's own volume; dedup is unaffected because the index and footers
  use full ids. The damage is limited to a T3-style swap (the bucket
  swaps the pair) or to ambiguity in any code path that maps a prefix
  back to a chunk (see 4.6). In keyed mode the guest cannot grind.

**Conclusion for Option 2.** t=16 is safe in every mode and all
adversaries. t=8 is acceptable only for keyed (encrypted) families with a
bounded size (say < 16 TiB distinct data per family) and a stale-hint
fallback that does not depend on the prefix; it is not acceptable for
unkeyed families (T4 grinding costs 2^32).

### 4.4 What the Merkle root still guarantees

The root commits to every leaf byte, hence to every `(tid, hint)` pair. With
a truncated id the commitment to *content* weakens from 256-bit collision
resistance to a `4t`-bit collision bound (for the guest-grind case) and a
`8t`-bit second-preimage bound; the commitment to *placement* is unchanged.
With the root MAC'd (encrypted volumes) nothing else changes for an
outside adversary. With no id at all (Option 3) the root commits only to
placement, and content integrity needs one of the three mechanisms below.

### 4.5 Option 3: no id in the leaf

What breaks, concretely:

1. **Verification of LBA -> content.** The fetched extent can be checked
   against its own header and AEAD, but nothing ties it to this LBA. In
   encrypted mode a bucket can serve any other valid extent for the
   ranged GET (e.g. an older version of the same LBA, a different
   LBA's chunk): silent data corruption by an active bucket. In
   unencrypted mode a misdirected read (T1) of a whole valid extent
   also passes.
2. **Stale-hint fallback** (`chunk id -> chunk index`) cannot work: there
   is no key to look up.
3. **Snapshot diff precision.** Entries compare by placement only; a
   compaction-moved but unchanged chunk shows as changed. This is true
   today too (the hint is in the leaf) but with an id the diff *could*
   ignore pure moves. Keeping `tid` makes "content changed" exact (up to
   2^-8t).
4. **Mirror comparison** is unaffected: mirrors are byte-identical copies, so
   roots match iff the bytes do.
5. **Compaction rewrite** keys `moved{id -> new hint}` by id; without an id
   it must key by the old `(seg, off)` (or `(seg, slot)`), which is fine and
   in fact cheaper, but old snapshots need a forwarding table (4.6).
6. **fsck/rebuild.** Rebuilding the chunk index from footers still works
   (footers hold ids). Re-deriving the map from the index is not possible,
   but it never was.

Three ways to get verification back without an id in the leaf, cheapest
first:

- **A. Position-bound extents (encrypted volumes).** Put the extent's own
  `(segment, offset)` (or slot) into the AEAD AAD together with `chunk_id`.
  A bucket then cannot serve an extent from anywhere but where it was
  written, and the MAC'd root authenticates the leaf's `(seg, off)`.
  This is sound if segment ids are never reused (they are monotonic
  `(epoch<<40)|counter`). Cost: compaction must re-encrypt moved extents
  (new AAD) instead of copying ciphertext; at 11 GB/s AES-GCM per core
  this is minor, but it invalidates ADR-11's "re-packing copies bytes
  verbatim" and requires a new salt per move. Mirrors still copy verbatim.
- **B. Authenticated segment table (all volumes).** The commit carries
  `seg -> BLAKE3(footer)` (32 B per segment, ~1 MiB per TiB) and the leaf
  holds `(seg, slot)`; verification fetches the (locally cacheable)
  footer, checks its hash against the table, takes `full_id` from the
  footer slot and checks the payload against it. This is a Merkle path
  `root -> leaf -> segment table -> footer -> id`. It needs the footer
  before the data is verified (cold: +1 footer GET per segment, section 3.4).
- **C. Header self-location for T1 only.** Put `(seg, off)` in the plain
  extent header and check it equals the hint, plus payload-hash ==
  header id. Defends corruption and misdirected reads, not an adversary.
  Sufficient for unencrypted volumes whose root is not authenticated anyway.

### 4.6 Dedup safety and the stale-hint fallback with a prefix

- **Dedup must keep using full ids.** The write path hashes the plaintext
  (full 32 B), looks up the chunk index (full), and only then derives
  `tid`. A prefix collision between two distinct chunks X, Y produces two
  index entries and two distinct extents, each leaf holds its own
  `(tid, hint)`. Nothing ever dedups on the prefix, so a prefix collision
  cannot cause data to be merged. This holds for any t.
- **The fallback path needs the full id.** After compaction an old
  snapshot's hint may point to a deleted segment. The reader has only
  `tid` and the LBA. Looking up the index by prefix (range scan on the
  sorted run) returns >1 candidate with probability `n / 2^(8t)` per lookup
  (6e-11 at n=2^30 for t=8, never for t=16), and then the reader cannot
  choose without more information and must fail closed (EIO). Rare, but
  an adversary who can grind (T4, unkeyed) can manufacture it at will. Mitigations:
  (a) t >= 16; (b) a forwarding table `moved/<oldseg>` that maps old
  `(seg, slot)` to the new location and is kept while any retained commit
  predates the compaction; (c) retain source segments until no retained
  commit references them (the GC mark already knows).
- **GC marking** only needs `seg` from the hint; it is unaffected by the id.
- **Warm-start heat maps** (ADR-23) are keyed by chunk index, unaffected.

### 4.7 Net assessment

| Variant | T1 corruption | T2 encrypted bucket | T3/T4 unencrypted | Stale-hint fallback | Verdict |
|---|---|---|---|---|---|
| 32 B id (today) | yes | yes | yes if root pinned | yes | baseline |
| 16 B id | yes (2^-128) | yes (swap prob < 2^-57 at 4 PiB) | second preimage 2^128; grind 2^64 | yes | safe |
| 8 B id | yes (2^-64) | marginal above ~16 TiB distinct data | grind 2^32 | ambiguous at scale | only for small keyed volumes |
| no id + position-bound AEAD (A) | yes | yes | n/a (use C) | needs forwarding table | safe when encrypted |
| no id + segment table (B) | yes | yes | yes if root pinned | needs forwarding table | safe, +1 footer GET cold |
| no id + header self-location (C) | yes | no | no (but root unauth. anyway) | needs forwarding table | unencrypted-only |

## 5. What the in-RAM leaf cache should hold

Numbers are leaf sizes from the 64 KiB tables; "LBA covered" assumes 32 MiB
per leaf and a 1 GiB `map_cache_max`.

| representation | bytes per leaf | leaves per GiB | LBA covered per GiB | lookup |
|---|---:|---:|---:|---:|
| status quo decoded row (24 KiB) | 24,576 | 43,700 | 1.3 TiB | 2.4 ns |
| `runs-bp16` encoded (seq / frag) | 9.2 / 11.0 KiB | 114,000 / 95,000 | 3.5 / 2.9 TiB | 121 / 181 ns |
| `runs-bp8` encoded | 5.2 / 7.0 KiB | 201,000 / 150,000 | 6.1 / 4.6 TiB | 64 / 235 ns |
| `runs-bp0` encoded | 1.2 / 3.0 KiB | 883,000 / 348,000 | 27 / 10.6 TiB | 49 / 118 ns |
| any of the above fully decoded to `{seg,off,clen}` (16 B/entry, ids aside) | 8 KiB + ids | | | 2.4 ns |

Recommendation:

1. **Cache the encoded bytes.** A 100-200 ns lookup is 0.1-0.2% of the 150 us
   p99 budget in DESIGN §20 and about 7.5% of one core at the 500K IOPS target
   if *every* read pays 150 ns for the lookup. In exchange, capacity grows
   2.2-2.6x (16 B) to 8-20x (lean) for the same RAM, which is what turns a
   map-miss GET into a hit for a 10+ TiB working set.
2. **Promote hot leaves to a decoded form** in a small second tier (e.g. 1/16 of the
   budget): decode costs 0.8-3.6 us once, then 2.4 ns. A sequential reader
   touches the same leaf 512 times, so decode-on-second-touch pays for itself
   by the ~12th chunk.
3. The decoded form can be kept thinner than the status quo's 24 KiB
   (the id column is only needed on miss/verify paths and can stay in the
   encoded buffer).
4. Cold leaf fetch from the bucket: a 9-11 KiB (16 B) or 1-3 KiB (lean)
   map-pack range instead of ~20 KiB; at S3 first-byte latency size does not
   matter, but per-commit upload and mirror traffic do: a FULL commit that
   dirties N leaves writes N x 9-11 KiB versus N x ~20 KiB.

## 6. Benchmark method

- **Host** (`results/lscpu.txt`, `results/rustc.txt`): AMD EPYC
  (Zen 5), 16 vCPU, 30 GiB, Linux 7.0.0 (Ubuntu), rustc 1.96.0 (2026-05-25),
  `-C target-cpu=native`, release, LTO, `opt-level=3`; crates `zstd` 0.13.4
  (libzstd 1.5.7) and `lz4_flex` 0.11.6 (pure Rust block format). Project
  lived under `~/lunix-leafenc/` with a private `CARGO_HOME`; peak disk use
  44 MB; no sudo, no git.
- **Threading.** Each run pinned with `taskset -c N`. The 64 KiB and 16 KiB
  runs executed *concurrently on different cores* (cores 2 and 3) on an idle
  host (load average ~2): timings may carry a few percent of shared-L3/memory
  noise; sizes are exact.
- **Destage model** (`src/gen.rs`). A global allocator appends extents to
  16 MiB segments (`HDR = 64 B` per extent, ids `(1<<40)|counter`, offsets
  from 0, slot = index within the segment, footers recorded). A batch is a
  set of dirty chunks sorted by LBA and appended in order, which is what
  produces contiguity and what random overwrites destroy.
  - **a-seq**: whole volume written in LBA order. `clen` ~ N(0.45, 0.15) x
    chunk, clamped to [6%, 100%] of the chunk, 8% stored raw (`clen` = chunk).
  - **b-frag**: a, then 30% of chunks (Bernoulli, uniformly spread) rewritten
    in random order and destaged in 1 GiB batches (16,384 chunks at 64 KiB),
    each batch LBA-sorted. Result: 262 runs and 123 distinct segments per
    leaf, 42,233 segments written (5.0 M extents are dead).
  - **c-clone**: 8 VM disks of 128 GiB cloned from one base image (written
    sequentially). Even-numbered VMs: identity mapping onto the base with 15%
    of pieces (geometric, mean 24 chunks = 1.5 MiB) rewritten uniquely.
    Odd-numbered VMs: "reinstalled" - pieces (mean 12 chunks) pointing at
    random positions in the base (dedup hits that are not contiguous with
    their neighbours), 20% unique. Entries copy the base's full hint and id.
    25 runs, 20 segments per leaf.
  - **d-mixed**: sequential, `clen` uniform in [chunk/16, chunk], 25% raw.
  - **e-sparse**: alternating data / hole runs, geometric mean 64 chunks
    (4 MiB) each, ~50% holes; 256 non-hole entries per leaf; all-hole leaves
    would cost zero (none occurred).
- **Ids** are 32 deterministic pseudo-random bytes (splitmix64 expansion of a
  per-chunk seed): the same entropy as BLAKE3 output. Dedup'd entries share
  their seed.
- **Metrics.**
  - Size: every non-empty leaf is encoded and summed (stride 1 for 64 KiB; for
    16 KiB every 4th leaf, scaled x4). MiB/TiB is scaled to a 1 TiB volume.
    B/entry is per non-hole entry. Compressed leaves count only the
    compressed bytes (zstd's frame header, ~9 B, is included; lz4 adds a 2 B
    length). No per-leaf length/hash in the map pack is counted (same for all
    options).
  - Encode / full-decode time: 4096 leaves evenly sampled from the volume,
    one warm pass then min of 3 passes, mean us per leaf.
  - Encoded lookup: 200,000 pre-generated random `(leaf, index)` pairs over
    the 4096 sampled encoded leaves (working set 4096 x leaf size; for the
    24 KiB row this is ~100 MB, beyond L2/L3 share, so the baseline is not
    flattered). *Mean* = min of 3 passes over the full loop (zstd variants
    use 50,000 ops). *p50 / p99* = per-op `Instant` timing over 50,000 ops
    minus the measured timer overhead (20 ns p50): values under ~50 ns are
    quantised and only the mean is reliable. Slot variants include the footer
    resolve (`Vec<Vec<(off,clen)>>`). Every lookup returns segment, offset,
    clen and the first min(T,8) id bytes; the harness folds them into a
    checksum so nothing is optimised away.
  - Decoded lookup: a flat array of 56 B decoded entries; same ops.
  - Correctness: `--check` mode decodes and looks up **every** index of every
    leaf of a 1/64 volume (all workloads, all variants) and asserts equality
    with the source entries, ids included (run again for e after fixing a
    scan bug in `runs-vi`, rc=0). During the timed runs 1 leaf in 97 is
    decoded and every 7th index looked up and asserted.
- **Not modelled:** real BLAKE3 or content; compaction (a-e are all fresh
  writes into a single epoch); multiple epochs; leaf-pack framing; encryption
  of leaves; zero-hole leaf elision in rows (the status quo row stores 48 B of
  zeros per hole, so its sparse figure is 768 as shown, an implementation
  could compress that).

## 7. Results

Full raw output: `lunix/spikes/leafenc/results/r64k.md` (64 KiB chunks, all 5
workloads, 74 variants each, with the per-workload run/segment statistics) and
`results/r16k.md` (16 KiB chunks, every 4th leaf for size). The tables below are
generated from them.

Workload statistics (64 KiB / 16 KiB):

| workload | non-hole entries per leaf | mean runs per leaf | mean segments per leaf | segments written | footer (full, 44 B/extent) MiB/TiB | offset+clen sidecar MiB/TiB |
|---|---:|---:|---:|---:|---:|---:|
| a-seq | 512 | 2.0 / 1.2 | 2.0 / 1.2 | 32,486 / 32,649 | 704 / 2816 | 82 / 328 |
| b-frag | 512 | 262 / 262 | 123 / 122 | 42,233 / 42,442 | 915 / 3661 | 107 / 426 |
| c-clone | 512 | 25.1 / 24.4 | 19.9 / 19.1 | 9,733 / 9,789 | 211 / 844 | 25 / 98 |
| d-mixed | 512 | 2.3 / 1.3 | 2.3 / 1.3 | 42,632 / 42,769 | 704 / 2816 | 82 / 328 |
| e-sparse | 256 | 9.5 / 9.1 | 1.5 / 1.1 | 16,239 / 16,328 | 352 / 1409 | 41 / 164 |

(Footer rows count dead extents too; "sidecar" = 24-bit offset + 17-bit clen.)

### 7.1 Size, 64 KiB chunks, all variants (MiB per TiB of volume)

| variant | a-seq | b-frag | c-clone | d-mixed | e-sparse |
|---|---:|---:|---:|---:|---:|
| `row32` | 768 | 768 | 768 | 768 | 768 |
| `row32+zstd1` | 631 | 649 | 635 | 630 | 318 |
| `row32+zstd3` | 632 | 650 | 635 | 630 | 319 |
| `row32+lz4` | 702 | 712 | 704 | 696 | 353 |
| `row16` | 512 | 512 | 512 | 512 | 512 |
| `row16+zstd1` | 370 | 389 | 375 | 368 | 188 |
| `row16+zstd3` | 371 | 388 | 374 | 369 | 188 |
| `row16+lz4` | 441 | 450 | 444 | 436 | 223 |
| `row8` | 384 | 384 | 384 | 384 | 384 |
| `row8+zstd1` | 237 | 257 | 243 | 234 | 121 |
| `row8+zstd3` | 238 | 256 | 241 | 236 | 122 |
| `row8+lz4` | 311 | 320 | 314 | 303 | 157 |
| `row0` | 256 | 256 | 256 | 256 | 256 |
| `row0+zstd1` | 94 | 115 | 98 | 94 | 50 |
| `row0+zstd3` | 94 | 114 | 97 | 94 | 50 |
| `row0+lz4` | 133 | 152 | 140 | 130 | 69 |
| `row-slot16` | 416 | 416 | 416 | 416 | 416 |
| `row-slot16+zstd1` | 292 | 313 | 296 | 292 | 148 |
| `row-slot0` | 160 | 160 | 160 | 160 | 160 |
| `row-slot0+zstd1` | 18 | 42 | 22 | 18 | 11 |
| `col32` | 768 | 768 | 768 | 768 | 768 |
| `col32+zstd1` | 634 | 652 | 650 | 628 | 316 |
| `col32+zstd3` | 633 | 646 | 634 | 627 | 315 |
| `col32+lz4` | 642 | 672 | 646 | 635 | 326 |
| `col16` | 512 | 512 | 512 | 512 | 512 |
| `col16+zstd1` | 368 | 387 | 384 | 363 | 185 |
| `col16+zstd3` | 368 | 385 | 371 | 363 | 186 |
| `col16+lz4` | 389 | 419 | 394 | 382 | 197 |
| `col8` | 384 | 384 | 384 | 384 | 384 |
| `col8+zstd1` | 235 | 253 | 249 | 230 | 120 |
| `col8+zstd3` | 235 | 251 | 237 | 229 | 120 |
| `col8+lz4` | 256 | 286 | 260 | 249 | 132 |
| `col0` | 256 | 256 | 256 | 256 | 256 |
| `col0+zstd1` | 97 | 116 | 100 | 93 | 52 |
| `col0+zstd3` | 97 | 116 | 100 | 93 | 52 |
| `col0+lz4` | 127 | 157 | 132 | 120 | 67 |
| `col-delta16` | 512 | 512 | 512 | 512 | 512 |
| `col-delta16+zstd1` | 308 | 369 | 315 | 301 | 159 |
| `col-delta0` | 256 | 256 | 256 | 256 | 256 |
| `col-delta0+zstd1` | 42 | 104 | 49 | 38 | 26 |
| `runs-bp32` | 550 | 608 | 555 | 550 | 277 |
| `runs-bp32+zstd1` | 550 | 609 | 556 | 550 | 277 |
| `runs-bp32+zstd3` | 550 | 609 | 556 | 550 | 277 |
| `runs-bp32+lz4` | 552 | 611 | 558 | 552 | 278 |
| `runs-bp16` | 294 | 352 | 299 | 294 | 149 |
| `runs-bp16+zstd1` | 294 | 353 | 300 | 294 | 149 |
| `runs-bp16+zstd3` | 294 | 353 | 300 | 294 | 149 |
| `runs-bp16+lz4` | 295 | 354 | 301 | 295 | 149 |
| `runs-bp8` | 166 | 224 | 171 | 166 | 85 |
| `runs-bp8+zstd1` | 166 | 225 | 172 | 166 | 85 |
| `runs-bp8+zstd3` | 166 | 225 | 172 | 166 | 85 |
| `runs-bp8+lz4` | 167 | 225 | 172 | 167 | 85 |
| `runs-bp0` | 38 | 96 | 43 | 38 | 21 |
| `runs-bp0+zstd1` | 38 | 97 | 44 | 37 | 21 |
| `runs-bp0+zstd3` | 38 | 97 | 44 | 37 | 21 |
| `runs-bp0+lz4` | 38 | 97 | 44 | 38 | 21 |
| `runs-bp-slot16` | 257 | 301 | 261 | 257 | 130 |
| `runs-bp-slot16+zstd1` | 257 | 301 | 261 | 257 | 130 |
| `runs-bp-slot16+zstd3` | 257 | 301 | 261 | 257 | 130 |
| `runs-bp-slot16+lz4` | 258 | 302 | 262 | 258 | 130 |
| `runs-bp-slot0` | 0.9 | 45 | 4.9 | 0.9 | 1.7 |
| `runs-bp-slot0+zstd1` | 1.1 | 45 | 5.2 | 1.2 | 2.0 |
| `runs-bp-slot0+zstd3` | 1.1 | 45 | 5.2 | 1.2 | 2.0 |
| `runs-bp-slot0+lz4` | 1.0 | 45 | 5.1 | 1.0 | 1.8 |
| `runs-vi16` | 303 | 365 | 308 | 302 | 153 |
| `runs-vi16+zstd1` | 304 | 359 | 308 | 302 | 153 |
| `runs-vi16+zstd3` | 304 | 360 | 308 | 296 | 153 |
| `runs-vi16+lz4` | 304 | 366 | 309 | 300 | 153 |
| `runs-vi0` | 47 | 109 | 52 | 46 | 25 |
| `runs-vi0+zstd1` | 39 | 92 | 45 | 37 | 22 |
| `runs-vi0+zstd3` | 40 | 93 | 45 | 37 | 22 |
| `runs-vi0+lz4` | 47 | 109 | 52 | 43 | 25 |
| `runs-vi-slot0` | 0.5 | 45 | 4.0 | 0.6 | 1.1 |
| `runs-vi-slot0+zstd1` | 0.8 | 37 | 4.3 | 0.9 | 1.4 |

### 7.2 Size, 16 KiB chunks (selected variants; 4x the entries)

| variant | a-seq | b-frag | c-clone | d-mixed | e-sparse |
|---|---:|---:|---:|---:|---:|
| `row32` | 3072 | 3072 | 3072 | 3072 | 3072 |
| `row32+zstd1` | 2520 | 2595 | 2540 | 2507 | 1272 |
| `row32+lz4` | 2805 | 2842 | 2810 | 2778 | 1412 |
| `row16` | 2048 | 2048 | 2048 | 2048 | 2048 |
| `row8` | 1536 | 1536 | 1536 | 1536 | 1536 |
| `row0` | 1024 | 1024 | 1024 | 1024 | 1024 |
| `row0+zstd1` | 363 | 446 | 379 | 352 | 192 |
| `row-slot0` | 640 | 640 | 640 | 640 | 640 |
| `col16+zstd1` | 1464 | 1542 | 1532 | 1444 | 740 |
| `col0+zstd1` | 383 | 459 | 394 | 368 | 204 |
| `col-delta0+zstd1` | 160 | 408 | 191 | 146 | 100 |
| `runs-bp32` | 2182 | 2416 | 2204 | 2183 | 1099 |
| `runs-bp16` | 1158 | 1392 | 1180 | 1159 | 587 |
| `runs-bp16+zstd1` | 1160 | 1393 | 1181 | 1160 | 588 |
| `runs-bp8` | 646 | 880 | 668 | 647 | 331 |
| `runs-bp0` | 134 | 368 | 156 | 135 | 74 |
| `runs-bp-slot16` | 1027 | 1210 | 1044 | 1027 | 519 |
| `runs-bp-slot0` | 3.0 | 186 | 20 | 3.1 | 6.8 |
| `runs-vi16` | 1159 | 1407 | 1179 | 1170 | 584 |
| `runs-vi0` | 135 | 383 | 155 | 146 | 72 |
| `runs-vi0+zstd1` | 136 | 355 | 156 | 130 | 73 |

### 7.3 Timing, 64 KiB chunks, selected variants

Columns: enc/dec = us per leaf (encode from entries / full decode into
entries; for wrapped variants this includes compress / decompress); lookup(enc)
= ns per point lookup on the encoded leaf, including decompression for
wrapped variants; lookup(decoded) = ns on the decoded array.

**a-seq**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 768.0 | 48.00 | 2.0 | 3.9 | 18 | 20 | 160 | 2.4 |
| `row32+zstd1` | 630.6 | 39.41 | 39.0 | 19.9 | 16292 | 15931 | 21410 | 2.4 |
| `row32+lz4` | 702.0 | 43.88 | 22.0 | 6.0 | 3227 | 3250 | 4350 | 2.4 |
| `row16` | 512.0 | 32.00 | 2.1 | 3.2 | 14 | 20 | 130 | 2.4 |
| `row8` | 384.0 | 24.00 | 2.3 | 3.4 | 11 | 20 | 130 | 2.4 |
| `row0` | 256.0 | 16.00 | 1.7 | 3.1 | 7 | 10 | 30 | 2.4 |
| `row-slot0` | 160.0 | 10.00 | 1.6 | 2.6 | 44 | 60 | 380 | 2.4 |
| `col0+zstd1` | 97.3 | 6.08 | 11.3 | 5.2 | 3930 | 3950 | 5140 | 2.4 |
| `runs-bp32` | 550.0 | 34.37 | 4.0 | 2.0 | 81 | 70 | 210 | 2.4 |
| `runs-bp16` | 294.0 | 18.37 | 4.1 | 1.5 | 121 | 90 | 290 | 2.4 |
| `runs-bp8` | 166.0 | 10.37 | 4.4 | 1.5 | 64 | 60 | 180 | 2.4 |
| `runs-bp0` | 38.0 | 2.37 | 3.5 | 1.3 | 49 | 40 | 80 | 2.4 |
| `runs-bp-slot0` | 0.9 | 0.05 | 2.5 | 0.9 | 67 | 60 | 220 | 2.4 |
| `runs-vi16` | 303.3 | 18.95 | 3.3 | 2.2 | 1146 | 1160 | 1610 | 2.4 |
| `runs-vi0` | 47.3 | 2.95 | 2.9 | 2.0 | 1184 | 1150 | 1511 | 2.4 |

**b-frag**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 768.0 | 48.00 | 2.1 | 4.0 | 20 | 20 | 190 | 2.4 |
| `row32+zstd1` | 649.4 | 40.59 | 41.6 | 20.3 | 16309 | 16260 | 22351 | 2.4 |
| `row32+lz4` | 712.1 | 44.50 | 23.6 | 5.9 | 3081 | 3090 | 4680 | 2.4 |
| `row16` | 512.0 | 32.00 | 2.0 | 3.3 | 17 | 20 | 140 | 2.4 |
| `row8` | 384.0 | 24.00 | 2.3 | 3.3 | 14 | 20 | 140 | 2.4 |
| `row0` | 256.0 | 16.00 | 1.6 | 3.1 | 7 | 10 | 30 | 2.4 |
| `row-slot0` | 160.0 | 10.00 | 1.6 | 2.4 | 30 | 60 | 270 | 2.4 |
| `col0+zstd1` | 116.0 | 7.25 | 16.9 | 8.9 | 6654 | 6620 | 8020 | 2.4 |
| `runs-bp32` | 608.3 | 38.02 | 10.3 | 3.6 | 228 | 240 | 380 | 2.4 |
| `runs-bp16` | 352.3 | 22.02 | 10.3 | 3.4 | 181 | 150 | 360 | 2.4 |
| `runs-bp8` | 224.3 | 14.02 | 10.5 | 3.6 | 235 | 230 | 480 | 2.4 |
| `runs-bp0` | 96.3 | 6.02 | 9.9 | 3.3 | 118 | 110 | 220 | 2.4 |
| `runs-bp-slot0` | 44.9 | 2.81 | 8.2 | 2.8 | 120 | 140 | 350 | 2.4 |
| `runs-vi16` | 365.1 | 22.82 | 5.8 | 5.0 | 2655 | 2770 | 3630 | 2.4 |
| `runs-vi0` | 109.1 | 6.82 | 5.5 | 4.9 | 2671 | 2630 | 3210 | 2.4 |

**c-clone**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 768.0 | 48.00 | 2.1 | 3.9 | 19 | 20 | 190 | 2.3 |
| `row32+zstd1` | 635.3 | 39.70 | 39.7 | 20.1 | 16479 | 16120 | 21760 | 2.3 |
| `row32+lz4` | 704.0 | 44.00 | 22.2 | 6.0 | 3190 | 3170 | 4560 | 2.3 |
| `row16` | 512.0 | 32.00 | 2.0 | 3.2 | 18 | 20 | 140 | 2.3 |
| `row8` | 384.0 | 24.00 | 2.3 | 3.3 | 11 | 20 | 120 | 2.3 |
| `row0` | 256.0 | 16.00 | 1.6 | 3.1 | 8 | 10 | 30 | 2.3 |
| `row-slot0` | 160.0 | 10.00 | 1.6 | 2.6 | 23 | 40 | 220 | 2.3 |
| `col0+zstd1` | 100.0 | 6.25 | 12.3 | 6.2 | 4478 | 4350 | 6230 | 2.3 |
| `runs-bp32` | 555.4 | 34.71 | 4.6 | 2.1 | 145 | 110 | 310 | 2.3 |
| `runs-bp16` | 299.4 | 18.71 | 4.6 | 1.8 | 129 | 90 | 280 | 2.3 |
| `runs-bp8` | 171.4 | 10.71 | 4.7 | 1.7 | 89 | 80 | 180 | 2.3 |
| `runs-bp0` | 43.4 | 2.71 | 4.1 | 1.6 | 76 | 70 | 110 | 2.3 |
| `runs-bp-slot0` | 4.9 | 0.31 | 3.1 | 1.1 | 48 | 60 | 110 | 2.3 |
| `runs-vi16` | 308.2 | 19.26 | 3.5 | 2.4 | 1216 | 1210 | 1750 | 2.3 |
| `runs-vi0` | 52.2 | 3.26 | 3.1 | 2.2 | 1221 | 1250 | 1650 | 2.3 |

**d-mixed**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 768.0 | 48.00 | 2.0 | 4.0 | 18 | 20 | 140 | 2.2 |
| `row32+zstd1` | 629.5 | 39.35 | 40.1 | 20.0 | 16022 | 15771 | 21291 | 2.2 |
| `row32+lz4` | 696.5 | 43.53 | 23.0 | 6.3 | 3546 | 3500 | 4520 | 2.2 |
| `row16` | 512.0 | 32.00 | 2.1 | 3.2 | 12 | 20 | 120 | 2.2 |
| `row8` | 384.0 | 24.00 | 2.2 | 3.3 | 32 | 20 | 250 | 2.2 |
| `row0` | 256.0 | 16.00 | 1.6 | 3.1 | 7 | 10 | 30 | 2.2 |
| `row-slot0` | 160.0 | 10.00 | 1.6 | 2.5 | 31 | 60 | 290 | 2.2 |
| `col0+zstd1` | 92.8 | 5.80 | 13.0 | 6.9 | 4875 | 4850 | 5900 | 2.2 |
| `runs-bp32` | 550.2 | 34.38 | 4.0 | 2.0 | 95 | 70 | 230 | 2.2 |
| `runs-bp16` | 294.2 | 18.38 | 4.1 | 1.5 | 83 | 70 | 220 | 2.2 |
| `runs-bp8` | 166.2 | 10.38 | 4.2 | 1.3 | 65 | 60 | 170 | 2.2 |
| `runs-bp0` | 38.2 | 2.38 | 3.5 | 1.2 | 52 | 50 | 80 | 2.2 |
| `runs-bp-slot0` | 0.9 | 0.06 | 2.6 | 0.9 | 60 | 60 | 190 | 2.2 |
| `runs-vi16` | 302.2 | 18.89 | 3.4 | 2.4 | 1372 | 1400 | 1840 | 2.2 |
| `runs-vi0` | 46.2 | 2.89 | 3.1 | 2.1 | 1373 | 1340 | 1680 | 2.2 |

**e-sparse**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 767.9 | 96.00 | 1.8 | 3.6 | 25 | 20 | 190 | 2.3 |
| `row32+zstd1` | 317.7 | 39.72 | 23.5 | 13.8 | 9795 | 10020 | 15730 | 2.3 |
| `row32+lz4` | 353.2 | 44.15 | 12.6 | 8.1 | 5110 | 4780 | 12700 | 2.3 |
| `row16` | 512.0 | 64.00 | 1.8 | 3.3 | 19 | 20 | 140 | 2.3 |
| `row8` | 384.0 | 48.00 | 1.8 | 3.3 | 19 | 20 | 140 | 2.3 |
| `row0` | 256.0 | 32.00 | 1.6 | 3.2 | 13 | 20 | 30 | 2.3 |
| `row-slot0` | 160.0 | 20.00 | 1.7 | 2.5 | 19 | 30 | 170 | 2.3 |
| `col0+zstd1` | 52.3 | 6.54 | 10.8 | 4.8 | 3398 | 3360 | 5120 | 2.3 |
| `runs-bp32` | 276.8 | 34.60 | 3.3 | 1.3 | 64 | 50 | 120 | 2.3 |
| `runs-bp16` | 148.8 | 18.60 | 3.3 | 0.8 | 59 | 50 | 101 | 2.3 |
| `runs-bp8` | 84.8 | 10.60 | 3.4 | 0.9 | 56 | 50 | 100 | 2.3 |
| `runs-bp0` | 20.8 | 2.60 | 3.0 | 0.8 | 53 | 40 | 90 | 2.3 |
| `runs-bp-slot0` | 1.7 | 0.21 | 2.6 | 0.7 | 42 | 40 | 100 | 2.3 |
| `runs-vi16` | 152.7 | 19.09 | 2.6 | 1.3 | 361 | 250 | 1090 | 2.3 |
| `runs-vi0` | 24.7 | 3.09 | 2.4 | 1.3 | 386 | 260 | 1110 | 2.3 |


### 7.4 Timing, 16 KiB chunks (workloads a and b)

**a-seq**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 3072.0 | 48.00 | 2.0 | 3.8 | 16 | 20 | 140 | 2.4 |
| `row32+zstd1` | 2519.9 | 39.37 | 41.9 | 19.3 | 15839 | 15630 | 21150 | 2.4 |
| `row32+lz4` | 2804.6 | 43.82 | 24.6 | 6.0 | 3190 | 3190 | 4220 | 2.4 |
| `row16` | 2048.0 | 32.00 | 2.1 | 3.2 | 11 | 20 | 30 | 2.4 |
| `row8` | 1536.0 | 24.00 | 2.2 | 3.3 | 9 | 20 | 40 | 2.4 |
| `row0` | 1024.0 | 16.00 | 1.7 | 3.2 | 29 | 20 | 230 | 2.4 |
| `row-slot0` | 640.0 | 10.00 | 1.6 | 2.5 | 39 | 60 | 330 | 2.4 |
| `col0+zstd1` | 382.8 | 5.98 | 12.7 | 5.6 | 3928 | 3900 | 5000 | 2.4 |
| `runs-bp32` | 2182.3 | 34.10 | 3.9 | 2.1 | 140 | 160 | 300 | 2.4 |
| `runs-bp16` | 1158.3 | 18.10 | 3.9 | 1.6 | 105 | 70 | 280 | 2.4 |
| `runs-bp8` | 646.3 | 10.10 | 4.1 | 1.4 | 140 | 150 | 300 | 2.4 |
| `runs-bp0` | 134.3 | 2.10 | 3.4 | 1.1 | 41 | 30 | 70 | 2.4 |
| `runs-bp-slot0` | 3.0 | 0.05 | 2.5 | 0.8 | 109 | 140 | 290 | 2.4 |
| `runs-vi16` | 1159.2 | 18.11 | 3.2 | 2.1 | 917 | 940 | 1210 | 2.4 |
| `runs-vi0` | 135.2 | 2.11 | 2.9 | 1.7 | 847 | 840 | 1060 | 2.4 |

**b-frag**

| variant | MiB/TiB | B/entry | enc us/leaf | dec us/leaf | lookup(enc) mean ns | p50 | p99 | lookup(decoded) ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `row32` | 3072.0 | 48.00 | 2.1 | 3.9 | 20 | 20 | 150 | 2.3 |
| `row32+zstd1` | 2594.8 | 40.54 | 44.4 | 20.2 | 16440 | 16210 | 22211 | 2.3 |
| `row32+lz4` | 2841.6 | 44.40 | 26.1 | 5.8 | 3110 | 3070 | 4330 | 2.3 |
| `row16` | 2048.0 | 32.00 | 2.2 | 3.2 | 12 | 20 | 130 | 2.3 |
| `row8` | 1536.0 | 24.00 | 2.3 | 3.4 | 13 | 20 | 140 | 2.3 |
| `row0` | 1024.0 | 16.00 | 1.6 | 3.2 | 14 | 10 | 130 | 2.3 |
| `row-slot0` | 640.0 | 10.00 | 1.7 | 2.5 | 38 | 60 | 310 | 2.3 |
| `col0+zstd1` | 459.4 | 7.18 | 18.0 | 7.5 | 6591 | 6610 | 9660 | 2.3 |
| `runs-bp32` | 2415.7 | 37.75 | 9.8 | 3.6 | 223 | 240 | 380 | 2.3 |
| `runs-bp16` | 1391.7 | 21.75 | 9.7 | 3.5 | 198 | 210 | 370 | 2.3 |
| `runs-bp8` | 879.7 | 13.75 | 9.8 | 3.5 | 213 | 220 | 410 | 2.3 |
| `runs-bp0` | 367.7 | 5.75 | 9.2 | 3.0 | 117 | 110 | 170 | 2.3 |
| `runs-bp-slot0` | 185.9 | 2.91 | 8.0 | 2.9 | 121 | 140 | 340 | 2.3 |
| `runs-vi16` | 1406.9 | 21.98 | 5.7 | 4.8 | 2456 | 2460 | 3020 | 2.3 |
| `runs-vi0` | 382.9 | 5.98 | 5.2 | 4.8 | 2425 | 2390 | 2891 | 2.3 |


### 7.5 Reading the results

1. **Size is dominated by what is incompressible.** `runs-bp32` -> `runs-bp16` ->
   `runs-bp8` -> `runs-bp0` removes 256 MiB each time id bytes drop by 16
   (16.8M entries). The id column is `16.8M x id_len` and nothing else changes:
   at 16 B ids are 87% of a sequential leaf and 73% of a fragmented one.
2. **Fragmentation costs 58 MiB (lean) / 58 MiB (16 B) more per TiB**: 96 vs
   38, 352 vs 294, i.e. +58 MiB for 262 runs per leaf. Even the worst case is
   -54% (16 B) / -87% (lean) against the status quo.
3. **Clones are almost free** to encode (+5 MiB over seq) because dedup'd
   regions come in long contiguous pieces in the base. The random-piece
   (reinstalled) VMs set the 25 runs/leaf.
4. **Sparse volumes** halve everything (holes cost a run record, not an entry).
5. **16 KiB chunks** quadruple entry count; `runs-bp16` is 1158-1392 MiB/TiB
   (status quo 3072), and lean is 134-368. The `clen` column is 15 bits instead of 17.
6. **Lookup vs size.** The only encodings that are both small *and*
   point-lookupable without decompression are `runs-bp*`. Everything built on
   generic compression is 10-16 us (zstd) / 2-3 us (lz4) per lookup; varint runs are
   0.4-2.7 us.
7. **Fragmented lookup is ~2-3x the sequential cost** (181 vs 121 ns for
   `runs-bp16`) because the binary search covers 262 instead of 2 runs.
   A 1-in-8 restart table over runs would halve that; not needed.
8. **Timer caveat.** Row-layout lookups (7-20 ns) are at the resolution of the
   timer; read the means.

## Recommendations for Lunix

1. **Adopt the run + bit-packed column leaf, "LF2", for the map leaf.**
   Exact layout in 3.0 (`runs-bp`): header; per-leaf segment dictionary;
   run records `[start:9 | seg_idx | start_off | rank:10]`; `clen`
   column; a `cum` restart every 16 data entries; then the id column
   (`id_len` bytes per non-hole entry). Leaves stay 512 entries, immutable,
   content-hashed. **No outer zstd/lz4** on leaves (saves < 0.5%, costs 9-11 us
   encode and 250-400 ns per lookup). Encode 3-10 us, full decode 1-4 us,
   encoded point lookup 50-235 ns.
2. **Make the stored id length a per-volume parameter**, `id_len in {0, 16, 32}`,
   recorded in the MAC'd `volume` object and the leaf header (the leaf hash
   already commits to it). **Default 16.** Reasons: it keeps verification of
   LBA -> content, the stale-hint fallback (index lookup by 16 B prefix is
   unambiguous: n/2^128), and exact content-change diffs; it is safe in keyed
   and unkeyed modes; and it needs no new mechanism. Do not ship 8 B.
   `id_len = 32` is available for a FIPS / paranoid profile (550-608 MiB/TiB,
   still -21% to -28%, i.e. it is strictly better than today with no
   security trade).
3. **Expected size at the default (`id_len = 16`), 64 KiB chunks:**

   | workload | status quo | LF2 id16 | saving | LF2 lean (id_len 0) |
   |---|---:|---:|---:|---:|
   | sequential fill | 768 | 294 | -62% | 38 |
   | fragmented (30% random overwrites) | 768 | 352 | -54% | 96 |
   | VM clone / dedup | 768 | 299 | -61% | 43 |
   | mixed compressibility | 768 | 294 | -62% | 38 |
   | 50% sparse | 768 | 149 | -81% | 21 |

   Realistic mid-life volumes sit between a and b: **300-350 MiB/TiB, about
   2.3x smaller**. Per 16 KiB-chunk volume: 1158-1392 (status quo 3072).
   Leaf size on disk and in a cold GET: 9-11 KiB instead of 24 KiB (20 KiB
   with zstd).
4. **Lean profile (`id_len = 0`: 38-96 MiB/TiB, 8-20x smaller than the
   status quo) is worth specifying now but should be switched on only after
   two decisions** (details in section 8): position-bound extent AAD (and
   hence re-encrypting compaction) for encrypted volumes, plus extent-header
   self-location checks for unencrypted ones; and forwarding records for
   stale hints. Its payoff is real at scale (a 100 TiB volume: 76.8 GiB of
   leaves today, ~29-34 GiB at 16 B, ~4-10 GiB lean) but not needed for
   correctness of the first release.
5. **Do not adopt slot references** (option 4) for v1: total bytes are larger
   than offset mode, the footer must be written in slot order, and cold reads
   gain a serial footer GET.
6. **Cache encoded leaves**, with a small decoded tier for hot leaves
   (section 5). Capacity per GiB of RAM: 1.3 TiB of LBA today, 2.9-3.5 TiB
   at 16 B, 10-27 TiB lean.
7. **Do not change fanout or tree shape.** 512-entry leaves keep the same
   32 MiB LBA span, same depth, same snapshot-diff granularity. (With 1-3
   KiB lean leaves a fanout of 2048 might be attractive; out of scope.)

## 8. What changes in ADR-8, DESIGN §7 and neighbours

(Proposed text only; this note does not edit other documents.)

**ADR-8.**
- Replace "An entry is `chunk_id` (32 B) plus a location hint (16 B), or hole"
  with: a leaf is a *run-encoded, bit-packed* block of 512 entries; each
  non-hole entry resolves to `{id_prefix[id_len], seg, off, clen}`; `id_len`
  is a volume parameter (default 16 B).
- Add to **Rejected** the truncation lengths < 16 B and the slot reference;
  restate "*Leaves holding locations only, with no chunk id*" as "rejected as
  the default; available as the lean profile once position-bound extents and
  forwarding records exist".
- "Evidence" section: link this note.

**DESIGN §7.**
- Entries bullet: as above. Hot-path bullet: the node cache holds encoded
  leaves plus a small decoded tier.
- Cost bullet: "dirty_leaves x 24 KiB (compressed)" becomes "dirty_leaves x
  9-11 KiB (default) or 1-3 KiB (lean)"; the existing 24 KiB figure was also
  optimistic (zstd only reaches ~20 KiB).
- Hints bullet: on a stale hint the reader looks up the **16 B prefix** in the chunk
  index (sorted by id, prefix range scan, unambiguous); with `id_len = 0`
  it consults the forwarding records written by compaction instead.
- Add: leaf header = version, `id_len`, nruns, ndata, widths; decoders are
  bounds-checked and fuzzed (DESIGN §15 rules; every column length derives
  from header counts and widths, so the decoder must validate
  `sum(sizes) == len` and `run starts` strictly increasing, `rank` consistent,
  `slot/off` within range; the harness' `rd` already bounds reads).

**DESIGN §5 step 8 / §6 / §10.**
- Step 8: set the leaf entry to `{id[..id_len], hint}` after the full-id
  dedup lookup. Dedup, the chunk index, footers and extent headers keep the
  full 32 B id (unchanged).
- §6 footer: unchanged for `id_len > 0`. If option 4 is ever adopted, footers
  must be in write order.
- §10.1 step 3: the "cache-slab entry by `chunk_id`" lookup needs the full id;
  with a 16 B prefix key the slab (keyed by the 16 B prefix + verified against
  the extent header's full id) works; with `id_len = 0` the slab must be keyed
  by `(seg, off)`.
- §11 compaction: `moved{...}` keyed by old `(seg, off)` rather than `chunk_id`
  (works for any `id_len`; cheaper).

**ADR-11.** Only for the lean profile: add `(seg, off)` of the extent itself to
the AEAD AAD, state that compaction re-encrypts moved extents with a fresh
salt (mirrors still copy bytes verbatim), keep "never reuse a segment id".

## Open questions / UNVERIFIED

1. **Real data.** The generators are plausible, not measured from a real
   ublk workload: clen distribution, 8% raw, geometric piece lengths, 30%
   random overwrite. Phase 0 should replay a real VM image trace through
   the same encoders; the harness's `Alloc` is the only thing to replace.
2. **Compaction history.** All workloads are fresh writes in one epoch. After
   months of compaction, runs shorten (compactor groups by LBA locality, which
   should keep them, but this is unmeasured) and leaves span epochs (dictionary
   values cost up to ~41 bits each, <= 0.6 KiB/leaf worst case).
3. **Run-record trimming.** In fragmented leaves the run record is 50 bits and
   `start` + `rank` are 19 of them. Storing `clen` per chunk index (width 0 for
   holes only when the leaf has none) or deriving `rank` would take another
   ~10-15% off the fragmented numbers; not implemented.
4. **Cache keying without ids** (DESIGN §10.1 slab by `chunk_id`) needs a design
   pass before the lean profile.
5. **Encoded-lookup CPU at 500K IOPS** was extrapolated (150 ns x IOPS), not
   measured end-to-end; a per-volume decoded tier may be unnecessary.
6. **S3 first-byte latency for the footer GET** (option 4) is from the
   design's own PUT numbers, not measured.
7. **Timing noise.** The two runs shared the host and L3; the timer p50 is
   20 ns, so lookups < 50 ns are only meaningful as means. A repeat on a
   quiesced host with `perf stat` would tighten them.
8. **Primary-source gaps.** LSVD's extent-map encoding (paper PDF not
   parseable here), Neon's delta-layer B-tree layout, Ceph's RBD object-map
   bit width, bcachefs field widths beyond what the PDF fragment showed, the
   dm-thin value packing, and RocksDB's default restart intervals are from
   memory or secondary sources. None affects the recommendation.
9. **BLAKE3 grinding cost** (4.3, T3) is an order-of-magnitude estimate that
   assumes the attacker exploits BLAKE3's tree structure by varying only the last
   1 KiB; it is not a cited cryptanalysis result. The recommendation (16 B)
   does not depend on it.
10. **Is the unencrypted-volume threat model in scope?** ADR-11 only MACs
    encrypted commits and DESIGN §22 leaves the rollback anchor open; if
    unencrypted volumes are explicitly "corruption-only", the lean profile
    with header self-location (4.5 C) is already sufficient for them.
11. **Interaction with ADR-23** (warm-start heat maps keyed by chunk index) and
    with mirror re-hinting: not analysed beyond 4.6.

## Sources

Local:
- `/home/bra/cvs/replicix/lunix/docs/explanation/DESIGN.md` §5-§7, §10, §11, §15, §20
- `/home/bra/cvs/replicix/lunix/docs/explanation/DECISIONS.md` ADR-6, 7, 8, 11
- `/home/bra/cvs/replicix/lunix/spikes/leafenc/` (source: `src/gen.rs`, `src/codec.rs`, `src/main.rs`; raw output in `results/r64k.md`, `results/r16k.md`, `results/lscpu.txt`, `results/rustc.txt`, `results/check_e.out`)

Fetched (V):
- qcow2 specification: https://www.qemu.org/docs/master/interop/qcow2.html
- dm-thin provisioning (metadata sizing): https://docs.kernel.org/admin-guide/device-mapper/thin-provisioning.html
- dm-vdo design (block map): https://docs.kernel.org/admin-guide/device-mapper/vdo-design.html
- ZFS `blkptr_t` layout: https://raw.githubusercontent.com/openzfs/zfs/master/include/sys/spa.h
- Btrfs on-disk format: https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html
- restic design (pack format, index): https://raw.githubusercontent.com/restic/restic/master/doc/design.rst
- Kopia index v2: https://raw.githubusercontent.com/kopia/kopia/master/repo/content/index/index_v2.go
- LevelDB table format / block builder: https://github.com/google/leveldb/blob/main/doc/table_format.md , https://github.com/google/leveldb/blob/main/table/block_builder.cc
- Lemire, Boytsov, "Decoding billions of integers per second through vectorization": https://arxiv.org/abs/1209.2137
- Lemire, Kurz, Rupp, "Stream VByte": https://arxiv.org/abs/1709.08990
- Vigna, "Quasi-succinct indices": https://arxiv.org/abs/1206.4300
- `bitpacking` crate README (benchmarks): https://docs.rs/crate/bitpacking/0.9.3/source/README.md ; `stream-vbyte` 0.4.1: https://docs.rs/crate/stream-vbyte/latest
- LSVD paper (abstract level): https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf (EuroSys '22, DOI 10.1145/3492321.3524271)
- bcachefs principles of operation (partial): https://bcachefs.org/bcachefs-principles-of-operation.pdf
- Neon architecture overview (secondary): https://neon.com/docs/introduction/architecture-overview
