# P0.7 radix Merkle map prototype (ADR-8): results

Date: 2026-11-11/12. Spike: `spikes/radixmap` (uncommitted). Reproduce on the test host:
`cargo test --release && bash run.sh && python3 summarize.py out` (CPUs 8-15, one thread per run).

## Host and versions

- CPU: AMD EPYC (Zen 5), 16 vCPUs, SMT active=0, 1 NUMA node(s), ~31 GiB RAM, local NVMe SSD, ext4 root
- Kernel 7.0.0 (Ubuntu), Ubuntu 26.04; governor: n/a (virtualised, no cpufreq)
- rustc: rustc 1.96.0 (ac68faa20 2026-05-25)
- io_uring_disabled: 0 (unused by this spike)
- blake3 =1.8.7, lz4_flex =0.11.6, zstd =0.13.4 (crates.io, checked 2026-11-11). No root, no credentials needed. Release build, no `target-cpu=native`.
- Builds `cargo build --release -j 4` under `taskset -c 8-15`. Governor not settable in the VM (no cpufreq); numbers are single-thread wall clock, BLAKE3 without multithreading. Other workloads ran on the host; each cell was repeated 3 times and min-max is shown.
- Unit tests (9, all pass): leaf codec round trips at fan 16/256/512/1024; model leaf size within 4% of leafenc's own `gen('b')`; root independent of insert order and of seal batching; Builder == sets; old root readable after a seal; diff == brute force on a 1 M-entry tree (and nodes read <= 2 k depth); a 10 TiB hole volume seals to exactly depth nodes; cold open reads exactly depth nodes; LRU bounded.

## Depth table (fanout 512)

| volume | chunk | chunks | leaves | depth (levels incl. leaf) | L1 nodes | interior nodes total | interior resident (MiB, 24 KiB/node) | full table @48 B (GiB) |
|---|---|---|---|---|---|---|---|---|
| 1 TiB | 16 KiB | 67108864 | 131072 | 3 | 256 | 257 | 6.0 | 3.00 |
| 1 TiB | 64 KiB | 16777216 | 32768 | 3 | 64 | 65 | 1.5 | 0.75 |
| 1 TiB | 256 KiB | 4194304 | 8192 | 3 | 16 | 17 | 0.4 | 0.19 |
| 8 TiB | 16 KiB | 536870912 | 1048576 | 4 | 2048 | 2053 | 48.1 | 24.00 |
| 8 TiB | 64 KiB | 134217728 | 262144 | 3 | 512 | 513 | 12.0 | 6.00 |
| 8 TiB | 256 KiB | 33554432 | 65536 | 3 | 128 | 129 | 3.0 | 1.50 |
| 10 TiB | 16 KiB | 671088640 | 1310720 | 4 | 2560 | 2566 | 60.1 | 30.00 |
| 10 TiB | 64 KiB | 167772160 | 327680 | 4 | 640 | 643 | 15.1 | 7.50 |
| 10 TiB | 256 KiB | 41943040 | 81920 | 3 | 160 | 161 | 3.8 | 1.88 |
| 32 TiB | 16 KiB | 2147483648 | 4194304 | 4 | 8192 | 8209 | 192.4 | 96.00 |
| 32 TiB | 64 KiB | 536870912 | 1048576 | 4 | 2048 | 2053 | 48.1 | 24.00 |
| 32 TiB | 256 KiB | 134217728 | 262144 | 3 | 512 | 513 | 12.0 | 6.00 |

Confirms the corrected DESIGN §7: 3 levels (leaf + 2 interior) up to 8 TiB at 64 KiB, **4 levels at 10 TiB** (and 32 TiB). At 16 KiB chunks 1 TiB is 3 levels, 8 TiB is 4.

## What was built

- Leaf: 512 entries; the design codec `rb16` (run-encoded, bit-packed columns, 16 B id prefix, restart sums every 16; code copied from leafenc and parametrised on fanout), no outer compression. Plan-literal variant `row48lz4` (48 B rows, 24 KiB, LZ4) measured alongside, because the gate text is phrased for it.
- Interior: 512 x (32 B child hash + 16 B location) = 24 KiB, zstd-1 (DESIGN). Hash = BLAKE3 of the leaf's canonical bytes, or of (level, child hashes) for interior nodes. **Child locations are not hashed** (otherwise the root would depend on pack layout and ADR-8's "identical content, identical roots on any backend" would be false). Verified: 1 TiB built sequentially, by random-order sets and (virtually) from the model all give root `2880ace95506c3d2...`.
- Overlay: hash map leaf-index -> decoded 512 x 48 B leaf (24 KiB per dirty leaf). Seal: encode dirty leaves, hash, write, then walk the old root top-down merging child updates; interior nodes always resident; leaves in a byte-bounded LRU of canonical bytes, looked up in encoded form (lookup 120-180 ns per leafenc).
- Volume model: deterministic leaf = f(index), from leafenc's generators (`gen.rs` verbatim), workload b-frag (30% of entries overwritten into unrelated segments, ~260 runs per leaf, the leafenc worst case). Encoded size 356 MiB/TiB (leafenc: 294-352; with no fragmentation model 294).
- 10 TiB base leaves (3.5 GiB encoded) exceed the 2 GiB disk budget, so at 10 TiB (and for the virtual 1 TiB runs) leaf bytes are **not stored**: a `NodeSource` regenerates leaf `i` from the model on read and the reader hash-verifies it against the interior node like any other node. Interior nodes and every newly sealed node are real. Regeneration time is reported separately (`virt_regen_ms`) and is outside seal time (seal loads nothing: dirty leaves are loaded into the overlay at `set` time). 1 TiB "real" runs store leaves in a pack file and read it with `pread` (page cache warm).

## Build (bulk populate)

| scale / leaf | fill / order | wall s (excl. model gen) | leaf bytes | interior nodes | peak RSS MiB |
|---|---|---|---|---|---|
| 1 TiB rb16, stored | 100 %, sequential | 1.06 (0.65) | 356 MiB | 65 | 13 |
| 1 TiB rb16, stored | 10 %, sequential | 0.11 | 36 MiB | 8 | 12 |
| 1 TiB rb16, stored | 100 %, random order, one seal | 3.0 (2.5): sets 1.7 s + seal 0.76 s | 356 MiB | 65 | 1541 (768 MiB overlay + 768 MiB test source array) |
| 1 TiB rb16, stored | 10 %, random order | 1.1 (0.6) | 49 MiB | 65 | 1541 |
| 2 TiB rb16, stored | 100 % / 10 %, random | 6.2 (5.1) / 2.2 (1.2) | 713 / 98 MiB | 129 | 3079 |
| 10 TiB rb16, virtual | 100 %, sequential | 9.9 (5.9) | 3585 MiB (358.5/TiB) | 643 | 27 |
| 10 TiB rb16, virtual | 10 %, sequential | 1.0 (0.6) | 359 MiB | 66 | 6 |
| 10 TiB row48lz4, virtual | 100 %, sequential | 13.0 (8.9) | 7141 MiB (LZ4 ratio 0.93) | 643 | 27 |

Random insertion order at 10 TiB was **not run** (overlay would be ~7.5 GiB, over the RSS cap): at 1 and 2 TiB cost and RSS scale linearly (3.0 s -> 6.2 s; 768 MiB of overlay per TiB), extrapolating to ~31 s and ~7.5 GiB at 10 TiB. A volume therefore cannot hold "everything dirty" in the decoded overlay: see Findings.

## Gate 1: seal (10 TiB, fanout 512, 1 thread, uniform, one chunk per leaf)

Seal time excludes upload and the `set` phase. Medians of 3 (min-max in `summ.md`); `raw` = uncompressed bytes of all new nodes, `stored` = bytes appended to the pack.

| leaf | D | seal ms | raw MiB (leaf+interior) | stored MiB | leaf KiB each | stored / dirty data (64 KiB per chunk) |
|---|---|---|---|---|---|---|
| rb16 | 1 | 0.2 | 0.08 | 0.03 | 11.4 | 0.55 |
| rb16 | 100 | 9.4 | 3.4 | 2.9 | 11.4 | 0.46 |
| rb16 | 1 000 | **59.7** | **23.1** | 20.4 | 11.4 | 0.33 |
| rb16 | 10 000 | **274.9** | 126.2 | 123.0 | 11.4 | 0.20 |
| rb16 | 100 000 | 2 066-2 148 | 1 127 | 1 124 | 11.4 | 0.18 |
| row48lz4 | 1 000 | **71.1** | **35.4** | 31.1 | 22.3 | 0.50 |
| row48lz4 | 10 000 | **375.7** | 249.5 | 229.8 | 22.3 | 0.37 |

Leaf bytes for D = 1 000: rb16 11.1 MiB; row48lz4 21.8 MiB = 0.929 x D x 24 KiB (LZ4 ratio 0.93: ids are incompressible), versus the gate's 1.1 x that = 24.0 MiB. Time split at D = 1 000 (rb16): encode 10 ms, BLAKE3 20 ms (34 %), interior zstd-1 plus leaf compress 22 ms, write 5 ms. At D = 100 000: encode 1.1 s, BLAKE3 0.87 s (42 %), compress 0.12 s. Seal cost is ~20 us per dirty leaf; BLAKE3 share 30-45 %, the rest is serialisation; allocation is not visible. Interior zstd-1 costs ~40 us per node and saves little (hashes are incompressible); it is the largest single item at D = 1 000.

Other patterns (rb16, 10 TiB): Zipf-clustered D = 1 000: 36 ms, 13.5 MiB; D = 10 000: 269 ms; D = 100 000: 2.1 s, 1 124 MiB. Sequential full-leaf rewrite D = 1 000: 16.7 ms, 9.0 MiB (D x 9.2 KiB: a rewritten leaf is one or two runs and is dominated by its 8 KiB of id prefixes); D = 100 000: 1.2 s, 901 MiB for 3.1 TB of rewritten data (0.03 %). Model without fragmentation (leaf 9.2 KiB): D = 1 000 uniform 53.7 ms, 17.8 MiB.

1 TiB (stored leaves, 3 levels): uniform D = 1 000 28 ms / 12.4 MiB raw, D = 10 000 228 ms; (65 L1 nodes only, so the upper-node share is small).

### Flat-table baseline (10 TiB, 48 B rows, LZ4, BLAKE3 per block, root = hash of block-hash list)

| system | D = 100 | D = 1 000 | D = 10 000 | notes |
|---|---|---|---|---|
| radix rb16 | 9.4 ms / 2.9 MiB | 59.7 ms / 20.4 MiB | 275 ms / 123 MiB | |
| flat 1 MiB blocks (plan) | 99.5 ms / 92.5 MiB | 938 ms / 873 MiB | 5 643 ms / 5 252 MiB | 7 681 blocks, 240 KiB manifest |
| flat 64 KiB blocks (extra) | 10.8 ms / 9.6 MiB | 61.2 ms / 61.7 MiB | 552 ms / 568 MiB | 122 911 blocks, 3.8 MiB manifest rehashed and rewritten per seal |

Flat full-table seal: 8.7 s and 7 155 MiB (1 MiB blocks), 6.9 s and 7 137 MiB (64 KiB), excluding model regeneration. Flat table resident at 1 TiB (measured): 772 MiB RSS, so ~7.5 GiB at 10 TiB (extrapolated). The flat rows time block read-modify-write from memory; a real flat table also pays reading the old block.

Advisory rule: with the plan's 1 MiB blocks the tree is 15x faster and writes 43x fewer bytes at D = 1 000, so not "within 2x". With 64 KiB blocks the flat table matches the tree's seal **time** (61 vs 60 ms) but writes 3x the bytes, and its diff needs reading the changed blocks (below). The tree is justified by write amplification and diff, not by seal time alone.

## Gate 2: diff (10 TiB, warm = interior resident, uniform k leaves)

| k | tree ranges-only ms | interior nodes read | bound 2 k depth | entry-level diff ms (leaves cold / warm) | flat 1 MiB: blocks read, block compare ms | flat 64 KiB |
|---|---|---|---|---|---|---|
| 1 | 0.002 | 6 | 8 | 0.0 | 1, 0.6 | 1, 0.0 |
| 100 | 0.14 | 192 | 800 | 1.7 / 0.7 | 100, 27 | 100, 1.6 |
| 1 000 | **1.04** | **998** | 8 000 | 18.7 / 8.5 | 948, 260 | 998, 16.6 |
| 10 000 | 1.55 | 1 286 | 80 000 | 196 / 91 | 5 650, 1 580 | 9 726, 162 |

The tree returns exactly the dirtied leaves (asserted for every run, as is nodes read <= 2 k depth). Flat manifest comparison is 0.01-0.2 ms but only tells which blocks differ; locating rows needs both versions of every changed block. row48lz4 leaves: same ranges-only numbers; entry-level k = 10 000 is 590 ms because 24 KiB leaves must be LZ4-decoded.

## Gate 3: cold open

Empty cache, node source with 25 ms per GET (slept), random chunk: 10 TiB **4 GETs = depth**, first `get` 100.5 ms (3 probes); 1 TiB (stored) 3 GETs, 75.3 ms. A second lookup in the same leaf costs 0 GETs; compute-only (latency 0) first get 0.1-0.2 ms.

## Gate 4: resident memory (10 TiB, 1 M uniform random lookups after cold open, no latency)

| leaf cache bound | peak RSS MiB | cache used MiB | leaf GETs | hit rate | lookups/s |
|---|---|---|---|---|---|
| 64 MiB | 90 | 64 | 982 446 | 1.8 % | 30.7 k (incl. regeneration) |
| **256 MiB** | **294** | 256 | 929 940 | 7.0 % | 31.7 k |
| unbounded | 3 489 | 3 434 (312 144 leaves) | 312 144 | 68.8 % | 87 k |

Interior nodes (643, 15 MiB) are always resident and included. 294 MiB <= 256 + 64 MiB (no overlay in this sweep). Overlay cost is 24 KiB per dirty leaf in this design: D = 100 000 uniform dirty leaves peak at 2.65 GiB RSS. 1 TiB stored-leaf sweep: 64 MiB -> 74 MiB RSS; 256 MiB -> 270 MiB (cache + 14 MiB); unbounded 368 MiB (all 32 768 leaves, 358 MiB).
Note: peak RSS for seal cells after the first pattern (uniform) carries allocator retention from earlier cells (glibc keeps freed heap); the uniform series, run first with increasing D, is clean (D = 1 000: 85 MiB, 10 000: 530 MiB, 100 000: 2.65 GiB).

## Gate 5: full-population size at 10 TiB

48 B per chunk uncompressed: 167 772 160 chunks x 48 B = 8.05 GB (7.5 GiB); that is why the leaf cache is bounded. Encoded (rb16) leaves total 3 585 MiB (3.5 GiB, 358 MiB/TiB), interior 15 MiB resident; row48lz4 7 141 MiB.

## Fanout sweep (10 TiB, rb16, uniform; seal ms / stored MiB; Zipf in `summ.md`)

| fanout | depth | leaf KiB | D = 100 | D = 1 000 | D = 10 000 | interior resident (full) |
|---|---|---|---|---|---|---|
| 256 | 4 | 5.7 | 5.8 ms / 1.56 | 49.0 / 13.1 | 240 / 78.4 | 2 571 nodes x 12 KiB = 30 MiB |
| 512 | 4 | 11.4 | 9.4 / 2.86 | 60.0 / 20.4 | 277 / 123.0 | 643 x 24 KiB = 15 MiB |
| 1024 | 3 | 22.9 | 14.7 / 5.04 | 60.9 / 28.3 | 407 / 229.9 | 161 x 48 KiB = 7.6 MiB |

Fanout 256 writes 45 % / 36 % / 36 % fewer bytes than 512 at D = 100 / 1 000 / 10 000 (uniform; 47 % fewer at Zipf D = 1 000), at the same depth (4) at 10 TiB, doubling resident interior (30 MiB) and the L1 fan-in. The amendment rule is conditional on a gate failing; none failed, so no amendment (see Verdict). Gates 2-4 were not run at fanout 256 or 1024.

## Verdict

| # | rule (10 TiB, fanout 512, 1 core) | measured | result |
|---|---|---|---|
| 1 | D = 1 000: <= 100 ms, bytes <= 1.1 x D x 24 KiB x LZ4 ratio + upper nodes, <= 40 MiB total raw; D = 10 000 <= 1 s | rb16 59.7 ms, 23.1 MiB; row48lz4 71.1 ms, 35.4 MiB (leaf 21.8 <= 24.0); D = 10 000 275 / 376 ms | pass |
| 2 | diff k = 1 000 <= 200 ms warm, reads <= 2 k depth | 1.04 ms, 998 <= 8 000 | pass |
| 3 | cold open + first get = depth reads | 4 = 4 | pass |
| 4 | RSS <= cache + overlay + 64 MiB at 256 MiB | 294 <= 320 MiB | pass |
| 5 | report full-population size | 7.5 GiB flat; 3.5 GiB encoded | reported |

**ADR-8: accepted (gate)**, fanout 512, with the amplification caveat below. Margins are 1.7x (seal D = 1 000), 5x (D = 10 000), 190x (diff).

Against DESIGN §7 ("dirty_leaves x ~10 KiB encoded plus a few upper nodes"): measured 11.4 KiB per uniform dirty leaf (worst-case model), plus upper nodes that are 45 % of bytes at D = 1 000 (509 distinct L1 nodes touched, 19 KiB stored each) and 8 % at D = 10 000. DESIGN §20 has no map target; seal CPU (~20 us per dirty leaf) is negligible against the destage budget.

## Findings that change or qualify the design

1. **Write amplification of the map at scattered writes is 0.18-0.55 of the dirty logical bytes** (11.4 KiB of leaf per dirtied 64 KiB chunk, i.e. roughly +35-70 % over the compressed chunk data at ratio 0.5), worst when every dirty leaf has one dirty chunk. A volume with a 1 000 chunks/s scattered-write rate and a 60 s commit interval seals 60 000 leaves: ~0.7 GB and ~1.2 s per commit, ~11 MB/s of map upload. Acceptable, but this is the number the "map-delta records in DELTA commits" mitigation would cut by ~100x (a one-entry delta is tens of bytes vs 11.4 KiB + upper nodes); not needed for the gate, worth keeping for sync mode where a commit is per fsync (D is small there, 0.2-9 ms seal, 11-14 KiB leaf plus 3-4 interior nodes of ~19 KiB each: ~70-90 KiB per fsync).
2. **The decoded overlay is the memory risk, not the cache**: 24 KiB per dirty leaf (2.1x the encoded 11.4 KiB, and 500x a single 48 B entry). 100 000 dirty leaves = 2.4 GiB; all leaves of a 10 TiB volume would be 7.5 GiB. Phase 1 needs an overlay cap that forces a seal (or an overlay of sparse (slot, entry) deltas over the cached encoded leaf), and `B_max` throttling should be stated in dirty leaves.
3. **Location hints must be excluded from the interior node hash** (the 16 B location is not content); DESIGN §7 "node hash = BLAKE3 of the canonical encoding" should say so. Without it roots differ per backend.
4. Interior nodes are incompressible (hashes): zstd-1 costs ~40 us per node (largest item of a D = 1 000 seal) for a small gain; leaving interior nodes uncompressed or LZ4 is a free ~35 % seal-time cut at small D.
5. Depth at 10 TiB is 4 (already corrected in DESIGN §7); a cold first read at 10 TiB is 4 serial GETs (100 ms at 25 ms) unless the interior nodes are warmed from the local pack store, which DESIGN already does (interior always resident; 15 MiB).
6. LZ4 over 48 B rows saves only 7 %; the design's decision to drop outer compression of leaves is confirmed (rb16 is 50 % of the LZ4-compressed row size).

## DESIGN.md lines contradicted / to qualify (not applied)

- §7 "Node identity: node hash = BLAKE3 of the canonical encoding": for interior nodes the hash covers child hashes only, not child locations.
- §7 "Interior nodes are zstd-compressed": measured as costly with little gain (finding 4).
- §7 "bounded by the async interval": the bound must also be on overlay memory (finding 2).

## Deviations from the plan and not run

- Leaf = design codec `rb16` (ADR-8 as amended) as the primary; plan's 24 KiB/LZ4 leaf run as `row48lz4` for uniform seal, diff, build only.
- 10 TiB base leaves regenerated, not stored (disk budget); seal/diff/sweep numbers are unaffected because regeneration is outside seal time and each regenerated leaf is hash-verified. Cold-open and sweep at 10 TiB therefore include regeneration (~40 us) per leaf GET; the 1 TiB stored-leaf sweep (110-775 k lookups/s) shows the disk-backed (page cache) rate.
- Random-order bulk build only at 1 and 2 TiB (extrapolated to 10 TiB, see above); 10 % random fill is random chunks per leaf (~51 of 512), not random leaves.
- D = 100 000 seals write to a discarding sink (disk budget); write time there is 2 ms, i.e. not representative; D <= 10 000 write to a real pack file (page cache, no fsync, not durable).
- Zipf = Zipf(0.9) over 1 GiB regions, bursts of ~4 leaves, one chunk per leaf; seq = D whole leaves rewritten.
- Cold-open latency is slept per GET; the 1 M lookup sweep uses latency 0 (it would take 7 hours at 25 ms), so its lookups/s is not S3-bound.
- Gates 2-4 not run at fanouts 256 and 1024. Flat baseline at 10 TiB regenerates blocks from the model and times only compress, hash, manifest; flat diff times decompress+compare of changed blocks. No `rust-toolchain.toml` (host is 1.96, Constellation pins 1.98).
- 4 TiB, 8 TiB and 32 TiB were not built (the depth table is computed only).
- `cargo clippy` was not run.

## Proposed decision record (for DECISIONS.md, ADR-8)

**Gate outcome (2026-11-12): accepted (gate).** Evidence: `bench/radixmap/RESULTS.md`. At 10 TiB, fanout 512, 64 KiB chunks, one core: seal of 1 000 uniformly scattered dirty leaves 59.7 ms writing 23.1 MiB raw (20.4 MiB stored) and of 10 000 leaves 275 ms; diff of 1 000 differing leaves 1.0 ms reading 998 nodes (bound 8 000); cold open + first get 4 reads = depth; 256 MiB leaf cache gives 294 MiB peak RSS over 1 M random lookups; full 48 B/chunk table would be 7.5 GiB (encoded leaves 3.5 GiB). The flat-table alternative writes 43x more bytes per seal at 1 MiB blocks (3x at 64 KiB blocks) and cannot localise a diff without reading the changed blocks. Amendments to record: (1) interior node hash covers child hashes only, never locations; (2) the mutable overlay must be capped (forced seal at N dirty leaves) because a dirty leaf costs 24 KiB decoded vs 11.4 KiB sealed; (3) interior nodes need not be compressed (hash-dominated); (4) fanout 256 (leaf 5.7 KiB) writes 36-45 % fewer bytes at scattered writes at the cost of 2x resident interior nodes (30 MiB at 10 TiB): not adopted by the gate rule, left to the maintainer; (5) map-delta records in DELTA commits remain the mitigation if scattered-write seal volume (0.18-0.55 of dirty data) proves too high in phase 1.

## Cleanup

Remote: spike directory, packs (460 MiB peak) and test temp dirs removed; no processes left; nothing else on the host touched (only `~/lunix-p0/radixmap` created; crates.io downloads went to the shared cargo registry cache). No sudo/apt used. Raw CSVs: `raw/` (19 KiB); `summ.md` has all medians.
