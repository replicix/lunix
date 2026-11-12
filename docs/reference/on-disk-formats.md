# On-disk and on-bucket formats (phase 1 freeze)

Status: **draft of M1.A0, 2026-11-12, awaiting the independent review and
the maintainer's sign-off** (`crates/lunix-format/FREEZE.md`). Once signed,
every layout here is frozen for the life of the product: later phases add
code, not migrations (ROADMAP). A change after sign-off is a format change
and needs the maintainer.

This file is the byte-level specification that `lunix-format` (M1.A)
implements. It follows [DESIGN.md](../explanation/DESIGN.md),
[DECISIONS.md](../explanation/DECISIONS.md) and the phase 1 plan
(`docs/plans/v1/wip/01-p1-local-volume.md`, M1). Where the spec leaves a
layout open, the choice is made here and listed in
[Decided in A0](#24-decided-in-a0). The expected attach results are in
[attach-matrix.md](attach-matrix.md).

Contents:

1. [Conventions](#1-conventions)
2. [Decided before A0](#2-decided-before-a0)
3. [Primitives (freeze item 1)](#3-primitives-freeze-item-1)
4. [Crypto modes, key purposes and object kinds (items 7, 11)](#4-crypto-modes-key-purposes-and-object-kinds-freeze-items-7-and-11)
5. [Journal file and record (item 2)](#5-journal-file-and-record-freeze-item-2)
6. [Extent (item 3)](#6-extent-freeze-item-3)
7. [Segment and map-pack container (item 4)](#7-segment-and-map-pack-container-freeze-item-4)
8. [Map node (item 5)](#8-map-node-freeze-item-5)
9. [Metadata envelope (item 7)](#9-metadata-envelope-freeze-item-7)
10. [Commits (item 6)](#10-commits-freeze-item-6)
11. [`volume` descriptor (item 8)](#11-volume-descriptor-freeze-item-8)
12. [Keyring (item 9)](#12-keyring-freeze-item-9)
13. [Lease (item 10)](#13-lease-freeze-item-10)
14. [Pins: `snapshots/`, `readers/`, `holds/` (item 7)](#14-pins-snapshots-readers-holds-freeze-item-7)
15. [Opaque bodies: `heat/`, `dicts/`, `gc/space/` (item 7)](#15-opaque-bodies-heat-dicts-gcspace-freeze-item-7)
16. [GC files: `gc/<pass>` and `state/gc/condemned` (item 15)](#16-gc-files-gcpass-and-stategccondemned-freeze-item-15)
17. [Index run (item 13)](#17-index-run-freeze-item-13)
18. [Checkpoint (item 14)](#18-checkpoint-freeze-item-14)
19. [Device record (item 14)](#19-device-record-freeze-item-14)
20. [Local pins file (item 14)](#20-local-pins-file-freeze-item-14)
21. [`volume.json` and `volumes.json` (item 14)](#21-volumejson-and-volumesjson-freeze-item-14)
22. [Bucket and local paths (item 12)](#22-bucket-and-local-paths-freeze-item-12)
23. [Worked examples](#23-worked-examples)
24. [Decided in A0](#24-decided-in-a0)
25. [Open questions for the maintainer](#25-open-questions-for-the-maintainer)

## 1. Conventions

- **Endianness.** Every integer is **little endian**, unsigned unless the
  encoding column says `i8`. The one exception is outside this file: the
  control protocol's frame length (D13) is big endian and is not an on-disk
  format.
- **Offsets** are in bytes from the start of the structure the table
  describes. A field written `[n]` is a byte array of `n` bytes. Variable
  parts follow the fixed part in the order the table lists them.
- **Magics** are byte strings, compared byte for byte (`"LXJR"` is the four
  bytes `4c 58 4a 52`). File-level structures start with an 8-byte magic;
  records, envelopes and trailers use a 4-byte magic.
- **Reserved** fields and bits are written as zero and **rejected if not
  zero**, so a later phase can use them (plan M1.A rule 3).
- **Versions.** Every structure carries a version byte (or `u16`). A reader
  rejects a version it does not know.
- **Strings** are UTF-8 with no NUL byte, stored as a length field (stated
  per structure) followed by the bytes; never NUL-terminated. Identifier
  strings have the character sets of [§22](#22-bucket-and-local-paths-freeze-item-12).
- **Times** are `u64` milliseconds since 1970-01-01T00:00:00Z (UTC), named
  `*_at`, `created`, `deadline`.
- **Hashes.** `BLAKE3-256(x)` is the 32-byte BLAKE3 hash; `BLAKE3-128(x)`
  is its first 16 bytes; `keyed BLAKE3(k, x)` is BLAKE3's keyed mode.
  `HMAC-SHA-256(k, x)` is RFC 2104 with SHA-256. `crc32c(x)` is CRC-32C
  (Castagnoli, reflected polynomial `0x82F63B78`, initial value and final
  XOR `0xFFFFFFFF`; `crc32c("123456789") = 0xE3069283`), stored as `u32`.
- **Bounded decode.** Every count is checked against the remaining input
  and against the maximum stated in the table before anything is
  allocated. Arithmetic on sizes, offsets and sequence numbers is checked
  (DESIGN §15).
- **Canonical encodings.** Where a structure is hashed or MAC'd and could be
  encoded more than one way (sorted lists, bit widths, runs), the canonical
  form is stated and a decoder rejects any other form, so
  `encode(decode(b)) == b` for every accepted `b`.
- **`‖`** is byte concatenation. `align8(x)` rounds `x` up to a multiple of
  8; `round4k(x)` rounds up to a multiple of 4096.
- **Units:** KiB = 1024 bytes, MiB = 2^20, GiB = 2^30. A **block** is 4096
  bytes (the volume block size).

What protects each structure is stated in its section under
**Authenticated by**. "Checksum" means corruption detection only.

## 2. Decided before A0

Copied verbatim from the plan's "Decisions already taken" table, as revised
on 2026-11-12 after review 2 (`wip/01-p1-review-2.md`). A0 does not
re-decide any row.

| id | decision |
|---|---|
| D1 | `map_fanout` is a descriptor field: 256 or 512 legal, default 512. Interior map nodes are stored raw, through the extent codec (`kind = node`, codec 0 in phase 1; codec 1 zstd later needs no format change). The node header carries `level` and `fanout` only, **no codec byte**. |
| D2 | The stored `CCH1` header keeps its 2-byte codec id and 8-byte `ulen`. The AAD and the key-derivation `info` use the KAT vectors' canonical 1/1/4/4-byte widths. The AAD is a canonical re-encoding, not the header bytes (safe: chunk ≤ 1 MiB, codec ≤ 255). |
| D3 | Index runs are bucket objects `index/<pack:016x>` (§6.1 gains the prefix) with their own run format. **Lag rule:** `index_packs[]` of FULL *n* lists the runs that were durable before step C6 of *n*; they cover segments named by commits ≤ *n−1*. The run written at C8 of *n* is named by FULL *n+1*. |
| D4 | All byte layouts are decided in M1.A0, with maintainer sign-off, before any code. |
| D5 | Genesis commit: `commit_seq` 0, `parent_seq` none, `parent_hash` = BLAKE3 of the exact `volume` object bytes. Every later commit has `commit_seq = parent_seq + 1`. The commit counter is independent of the `write_seq` (FULL carries both `commit_seq` and `cut_seq`; `cut_seq` is a `write_seq`). Field names are `commit_seq`, `parent_seq`, `cut_seq` and `write_seq`; the `lunix-format` types are `CommitSeq` and `WriteSeq`, never a bare `Seq`. `chain_floor` is the `commit_seq` of the oldest retained FULL commit. Phase 1 retention is `last:N` FULL commits, default N = 2 (the tip and its predecessor), so the floor moves with every FULL once N FULLs exist; it is not genesis. Mount and fsck verify the chain from the tip down to `chain_floor`; the floor commit's parent may be absent. |
| D6 | `heat/`, `gc/` and `dicts/` freeze **envelope and kind id only**; the body is `{ver: u16, bytes}` defined by the phase or crate that writes it (`gc/` by M17 in phase 1: one sealed create-only object per GC pass, `gc/<pass:016x>`, body `{ver: 1, bytes}`; `dicts/` by `replicix-zdict`, M18a). |
| D7 | Journal lanes: one lane per ublk queue, a global `write_seq` assigned by the caller, lanes merged at recovery. Local `epoch` is 0 in every journal record and every commit. FLUSH is a **group flush** across lanes (interface in M1.B1). Recovery is floor-based: `recover(hint, floor)` with `floor` = the tip FULL's `cut_seq`. |
| D8 | Local file set: `volume` (the exact descriptor bytes), `volume.json` (local settings only), `families/<id>/keyring`, `state/device` (with `boot_id`), `state/checkpoint`, `state/pins`, the host registry `<state_dir>/volumes.json`; plus, new in this plan, the local mirrors of the GC-root objects `snapshots/`, `readers/`, `holds/` (empty in phase 1) and `gc/` (one sealed evidence object per pass, `gc/<pass:016x>`) and the local-only `state/gc/` (condemned list, the only mutable GC state, atomic replace). |
| D9 | The object name inside every metadata AAD is **bucket-relative** (`commits/<seq:016x>`, `segments/<seg:016x>`, ...), never the local path, so local bytes upload verbatim in phase 2 (ADR-7, §13.3). |
| D10 | EPOCH body includes `authority` (optional). Lease body includes `transferred_to` (optional, best effort, ADR-24 step 6). `snapshots/`, `readers/` and `holds/` bodies name their §6.1 fields in the freeze list (`{seq, created, consistency, label}`, `{seq, deadline}`, `{base_seq}`). |
| D11 | **Plain-extent tag.** Every extent carries the 16-byte tag field. On unencrypted volumes it is BLAKE3-128 (first 16 bytes) over **every byte of the extent before the tag**, that is `CCH1 header ‖ chunk_id ‖ clen ‖ payload`, taken as one contiguous slice (`&extent[..len-16]`), **keyed** by a key derived from the integrity key (purpose `extent tag`) when the family has one, unkeyed otherwise. Local reads and fsck verify it. (Decided by the lead on 2026-11-12, review 2 finding 1: this is the spec's wording, DESIGN §6, ADR-6, ADR-11 and GLOSSARY. It covers `clen`, which sits outside the 16-byte `CCH1` header, at the same cost, and matches the encrypted layout, whose AEAD likewise covers its framing.) On encrypted volumes the field is the AEAD tag as before. |
| D12 | **Journal framing and packing (DESIGN §4.1).** Each record is a 64-byte inline header plus its data, copied into the lane's own aligned buffer. Records are packed back to back, 8-byte aligned, inside one lane batch; only the **batch** is padded with zeros to a 4 KiB multiple and written with one `O_DIRECT` write. A zero magic means end of batch. Recovery stops at the first CRC failure or gap. Cost: n 4 KiB writes take ceil(n·4160/4096) blocks; an isolated QD1 4 KiB write costs 8 KiB. |
| D13 | Control protocol: length-prefixed JSON frames (u32 big-endian length, then a JSON object). |
| D14 | `max_io_buf_bytes` is a per-volume attach option (default 1 MiB, minimum 256 KiB). |
| D15 | GC is in phase 1 (M17): roots are the retained commits, the tip FULL plus its DELTAs, and snapshot, reader and hold pins (all pin sets empty in phase 1 but read). Deleting is condemn → grace → recheck → delete, for segments, map packs, index runs and commits below `chain_floor`. `retain_last` (`last:N`, default N = 2: the tip FULL and its predecessor) is a `volume.json` setting; `chain_floor` is the older retained FULL, and commit objects below it are deleted after the grace. |

**Binding corrections from review 2** (plan, M1.A0): the plain-extent tag
input is every byte of the extent before the tag, one contiguous slice,
including `clen`; the `gc/` envelope is one sealed create-only object per
pass, `gc/<pass:016x>`, body `{ver: 1, bytes}`, written once at pass end,
and `state/gc/condemned` is the only mutable GC state. Field names follow
D5. D9 and D10 still write `seq`; in a commit context that is `commit_seq`
(DESIGN terminology block), and the tables below use `commit_seq`.

Related review-2 rules that this file encodes as invariants: mount never
falls back to an older commit by itself (finding 8, [§10.7](#107-chain-rules));
GC never condemns an object above the tip's high-water mark (finding 5) and
the delete-time index invariant (finding 9) ([§17.4](#174-invariants)).

## 3. Primitives (freeze item 1)

| type | width | encoding | legal range | notes |
|---|---|---|---|---|
| `ChunkId` | 32 | bytes | any | Data extent: BLAKE3-256 of the plaintext chunk (plain and integrity modes); keyed BLAKE3 under the data addressing key (encrypted, default profile); HMAC-SHA-256 under it (encrypted, FIPS profile). In a map-node extent the same field holds the node hash ([§8.5](#85-node-hash-and-empty-hashes)). |
| `IdLen` | 1 | u8 | 0, 16, 32 | Map id-prefix length, from the descriptor. 0 (lean) decodes but `volume create` refuses it in v1 (ADR-8). |
| `IdPrefix` | `IdLen` | bytes | | The first `IdLen` bytes of a `ChunkId`. |
| `ObjId` (`SegId`, `PackId`, `RunId`) | 8 | u64 | `epoch << 40 \| counter`, `epoch` < 2^24, `counter` 1 ..= 2^40 − 1 | One id space per volume for segments, map packs and index runs (one counter). 0 means "none" and is never allocated. Ids are never reused. |
| `WriteSeq` | 8 | u64 | ≥ 1 for a record; 0 = before any write | Global write sequence (DESIGN §8.1). `cut_seq` is a `WriteSeq`. |
| `CommitSeq` | 8 | u64 | 0 = genesis | Commit position in the chain; `parent_seq`, `chain_floor`, `since_seq`, `base_seq`, pin and heat names are `CommitSeq`. |
| `Epoch` | 8 | u64 | ≥ 0; < 2^24 while it is used in an `ObjId` | Owner generation. 0 for every local record and commit in phase 1. |
| `DekGen` | 4 | u32 | ≥ 0 | DEK generation (ADR-11). The KAT field `dek_epoch` is this value. |
| `FamilyId` | 16 | bytes | random | Text form: 32 lowercase hex digits. |
| `VolumeUuid` | 16 | bytes | random (RFC 9562 version 4) | Text form: the 36-character lowercase hyphenated form. |
| `Codec` | 2 stored (`CCH1`), 1 canonical | u16 / u8 | 0 raw, 1 zstd, 2 LZ4, 3 zstd with dictionary; 4 ..= 65535 rejected | The registry of replicix-chunkfmt (`CODEC_RAW` … `CODEC_ZSTD_DICT`). Codec 3 is framed and round-tripped in phase 1 and rejected by the decompressor until phase 4. |
| `Level` | 1 | i8 | codec 0 and 2: 0; codec 1 and 3: −7 ..= 22 | The codec level as written. Not needed to decompress. |
| `ExtentKind` | 1 | u8 | 0 data, 1 map node | Never stored in an extent: it comes from the container (`segments/` or `map/`, [§7](#7-segment-and-map-pack-container-freeze-item-4)). |
| `Hint` | 16 | `{seg: ObjId, off: u32, clen: u32}` | `off` < container length; `clen` as in [§6](#6-extent-freeze-item-3) | Location of an extent. `clen` is the payload length; the extent occupies `OVH + clen` bytes ([§6.4](#64-extent-length-and-ovh)). |
| `LineageIdx` | 1 | u8 | 0 in v1 | 0 = this volume; `i` ≥ 1 = `lineage[i-1]` of the descriptor (ADR-25). |
| `TimeMs` | 8 | u64 | | Milliseconds since the Unix epoch, UTC. |

## 4. Crypto modes, key purposes and object kinds (freeze items 7 and 11)

### 4.1 Crypto modes

The descriptor's `crypto_mode` ([§11](#11-volume-descriptor-freeze-item-8))
selects one of three modes. "Encrypted" and "keyed" are read from the
authenticated descriptor, never from an object.

| mode | value | family root secret (in the keyring) | chunk id | extent tag | metadata objects | descriptor MAC |
|---|---|---|---|---|---|---|
| `plain` | 0 | none (no keyring) | BLAKE3-256 | BLAKE3-128, unkeyed | envelope form 0, `mac` = BLAKE3-256 checksum | BLAKE3-256 checksum |
| `integrity` | 1 | integrity key IK (32 bytes) | BLAKE3-256 | BLAKE3-128 keyed with the extent tag key | envelope form 0, `mac` = HMAC-SHA-256 under the integrity HMAC key | HMAC-SHA-256 |
| `encrypted` | 2 | KMK (32 bytes) | keyed (data addressing key) | AES-256-GCM tag | envelope form 1, XChaCha20-Poly1305 under the per-kind metadata key | HMAC-SHA-256 |

"The integrity key" of ADR-11 and D11 is the root IK of an `integrity`
family; every key used with it is derived from it by purpose (below). The
`profile` field is `default` (0) or `fips` (1). `fips` is legal only with
`encrypted`: it changes the purpose derivation and the chunk id to
HMAC/HKDF-SHA-256 (KAT `*_fips_*` vectors) and stores the module nonce in
the extent ([§6](#6-extent-freeze-item-3)). Its metadata sealing form is
reserved (envelope form 2, [§9](#9-metadata-envelope-freeze-item-7)) and not
defined in v1, so a `fips` volume cannot be written in phase 1 (M2 returns
`Unsupported`).

### 4.2 Key purposes (freeze item 11)

Every key is derived from the family root R (the KMK or the IK) as

```
purpose_key(R, purpose, args) = keyed BLAKE3(R, msg)             (default profile)
                              = HKDF-SHA-256-Expand(PRK = R, info = msg, L = 32)   (fips profile)
msg = "lunix kdf v1\0" ‖ family_id(16) ‖ purpose(u8) ‖ args
```

`"lunix kdf v1\0"` is the 13 bytes `6c 75 6e 69 78 20 6b 64 66 20 76 31 00`.

| purpose | id | args | root | used in mode | use | KAT vector |
|---|---|---|---|---|---|---|
| data addressing key | 0x01 | none | KMK | encrypted | keyed `chunk_id` of data chunks | `purpose_addressing` |
| DEK | 0x02 | `dek_gen` u32 | KMK | encrypted | `DEK_gen`, the PRF key of the extent key ([§6.3](#63-aad-key-derivation-info-and-plain-tag-input)) | `purpose_dek0`, `purpose_dek1`, `purpose_msg_dek1` |
| metadata key | 0x03 | object kind u8 ([§4.3](#43-object-kinds)) | KMK | encrypted | XChaCha20-Poly1305 key of envelope form 1 | `purpose_meta_commit` (kind 1), `purpose_meta_lease` (kind 2) |
| descriptor MAC key | 0x04 | none | KMK or IK | encrypted, integrity | HMAC key of the `volume` MAC | `purpose_volume_mac` |
| KMK fingerprint | 0x05 | none | KMK or IK | encrypted, integrity | the fingerprint is the **first 16 bytes** of the output | `purpose_pin` |
| map-node addressing key | 0x06 | none | KMK | encrypted | keyed node hash | none (A0) |
| keyring MAC key | 0x07 | none | KMK or IK | encrypted, integrity | HMAC key of the keyring `mac` | none (A0) |
| integrity HMAC key | 0x08 | none | IK | integrity | HMAC key of envelope form 0 `mac` (commits, lease, pins, footers, opaque bodies) | none (A0) |
| extent tag key | 0x09 | none | IK | integrity | key of the keyed BLAKE3-128 extent tag (D11) | none (A0) |
| reserved | 0x00, 0x0a ..= 0xff | | | | never used | |

A purpose is never used in a mode that the "used in mode" column does not
list. In `plain` mode no key exists.

### 4.3 Object kinds

The object kind id is the `kind` byte of the metadata AAD, of the envelope
header and of the metadata-key purpose. Ids 1 and 2 are fixed by the KAT
(`purpose_meta_commit`, `purpose_meta_lease`).

| id | name | objects | sealed in encrypted mode | MAC'd in plain/integrity mode |
|---|---|---|---|---|
| 1 | `commit` | `commits/<commit_seq:016x>` of kind FULL, EPOCH, CONFIG, TRANSFER | yes | yes |
| 2 | `lease` | `lease` | yes | yes |
| 3 | `delta` | `commits/<commit_seq:016x>` of kind DELTA | yes | yes |
| 4 | reserved | never assigned (was "map pack" in the P0.8 spike; map nodes use the extent scheme) | | |
| 5 | `heat` | `heat/<commit_seq:016x>` | yes | yes |
| 6 | `footer` | the footer of `segments/<seg:016x>` and of `map/<pack:016x>` | yes | yes |
| 7 | `gc` | `gc/<pass:016x>` | yes | yes |
| 8 | `readers` | `readers/<host>-<id>` | yes | yes |
| 9 | `holds` | `holds/<child-uuid>` | yes | yes |
| 10 | `snapshots` | `snapshots/<name>` | yes | yes |
| 11 | `dicts` | `dicts/<id:08x>` | yes | yes |
| 12 | `space` | `gc/space/<commit_seq:016x>` | yes | yes |
| 13 | `keyring` | `families/<family-id>/keyring` | never sealed: used only in the keyring MAC input | MAC'd with the keyring MAC key |
| 0, 14 ..= 255 | reserved | | | |

Not metadata objects: `volume` (own MAC, [§11](#11-volume-descriptor-freeze-item-8)),
extents and map nodes (extent scheme, [§6](#6-extent-freeze-item-3)),
index runs (checksummed, [§17](#17-index-run-freeze-item-13)), `head` and
`sync/*/progress` (advisory, never trusted, not defined here).

### 4.4 Metadata AAD (freeze item 7)

```
meta_aad(id, kind, name) = "lunix meta v1\0" ‖ id(16) ‖ kind(u8) ‖ name_len(u32) ‖ name
```

| part | width | value |
|---|---|---|
| label | 14 | `6c 75 6e 69 78 20 6d 65 74 61 20 76 31 00` |
| `id` | 16 | the `VolumeUuid`; the `FamilyId` for the keyring |
| `kind` | 1 | object kind ([§4.3](#43-object-kinds)) |
| `name_len` | 4 | u32, 1 ..= 1024 |
| `name` | `name_len` | the **bucket-relative** object name (D9): relative to `<prefix>/lunix/v1/<volume-uuid>/` for volume objects (`commits/0000000000000007`), relative to `<prefix>/lunix/v1/` for the keyring (`families/<family-id>/keyring`). Never a local path. |

The same `meta_aad` is the AEAD AAD of envelope form 1 and the prefix of the
MAC input of envelope form 0 ([§9](#9-metadata-envelope-freeze-item-7)).

## 5. Journal file and record (freeze item 2)

The journal is local, plaintext (ADR-11 F11) and checksummed, not
authenticated. A lane owns a ring of preallocated, zero-filled 64 MiB files
`journal/q<lane:02x>-<ordinal:08x>.j`, opened `O_DIRECT`.

### 5.1 Journal file header (block 0)

The header occupies the whole first block (bytes 0-4095), written once when
the file is initialised; records start at offset 4096.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXJNL"` | |
| `ver` | 8 | 2 | u16 | 1 | |
| `lane` | 10 | 2 | u16 | 0 ..= 255 | equals the `<lane>` of the file name |
| `ordinal` | 12 | 4 | u32 | ≥ 1 | equals the `<ordinal>` of the file name; rises each time the lane takes a file into use |
| `volume_uuid` | 16 | 16 | bytes | the volume's uuid | |
| `generation` | 32 | 8 | u64 | ≠ 0 | a fresh random value each time the file is (re)initialised; never visible to the guest |
| reserved | 40 | 20 | | zero | |
| `crc32c` | 60 | 4 | u32 | `crc32c(bytes 0..60)` | |
| padding | 64 | 4032 | | zero | |

File length: exactly 64 MiB (67 108 864 bytes). **Reinitialising a file**
(ring reuse): zero bytes 4096 .. end, `fdatasync`, then write a new header
with the next ordinal and a new `generation`, `fdatasync`, rename to the new
name, directory `fsync`. A file whose header is valid is therefore zero
beyond its last write. A file whose header fails its CRC or does not match
its name (a crash during reinitialisation) holds no valid records and is
reinitialised again at open. **Authenticated by:** nothing (checksum only).

### 5.2 Record header (64 bytes)

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 4 | bytes | `"LXJR"` | a zero `u32` here means "no record" ([§5.3](#53-batch-packing-and-how-a-reader-finds-records)) |
| `ver` | 4 | 1 | u8 | 1 | |
| `kind` | 5 | 1 | u8 | 1 `write`, 2 `hole` | `hole` = DISCARD or WRITE_ZEROES |
| reserved | 6 | 2 | | zero | |
| `write_seq` | 8 | 8 | u64 | ≥ 1 | strictly increasing within a lane, across its files in ordinal order |
| `epoch` | 16 | 8 | u64 | 0 in phase 1 | the owner epoch (D7) |
| `lba` | 24 | 8 | u64 | `lba + len` ≤ device size in blocks | in 4096-byte **blocks** |
| `len` | 32 | 4 | u32 | `write`: 1 ..= 256; `hole`: 1 ..= 2^32 − 1 | in 4096-byte blocks; a write is at most 1 MiB (`max_sectors`) |
| `crc32c` | 36 | 4 | u32 | see below | |
| `gen` | 40 | 8 | u64 | the container generation | journal: the file header's `generation`; memfd sink: the buffer generation; DELTA: 0 |
| `off` | 48 | 8 | u64 | the record's own offset | journal: byte offset of this header in the file; memfd: offset in the ring; DELTA: offset in the decoded records region |
| reserved | 56 | 8 | | zero | |
| data | 64 | `len × 4096` for `write`, 0 for `hole` | bytes | | the written data |

`crc32c` = `crc32c(header bytes 0..64 with the crc32c field set to zero ‖
data)`. The record occupies `align8(64 + data length)` bytes; with v1 record
kinds that is exactly `64 + data length`, a multiple of 64. A record never
crosses the end of its file.

### 5.3 Batch packing and how a reader finds records

Writer (D12): a lane appends records to a **batch**: back to back, each at
an 8-byte-aligned offset. The batch starts at a 4 KiB boundary, is padded
with zeros to a 4 KiB multiple and is written with one `O_DIRECT` write. The
next batch starts at the next 4 KiB boundary after it. There is no batch
header and no separate header block. Several batches of one lane may be in
flight at once, at consecutive file offsets. A batch never crosses the end
of a file; when the remaining space is too small the lane moves to its next
file.

Cost (D12): a batch of *n* 4 KiB writes takes `ceil(n · 4160 / 4096)`
blocks, under 1.6 % plus one block of overhead at n ≥ 32; an isolated QD1
4 KiB write is one record of 4160 bytes and costs two blocks (8 KiB).

Reader (normative; recovery and fsck):

```
p = 4096; prev_write_seq = (last accepted write_seq of this lane, else 0)
loop:
    if p == file length: end of file (continue with the lane's next file)
    m = u32 at p
    if m == 0:
        if p is a multiple of 4096: END OF LANE      (no batch starts here)
        p = round4k(p); continue                      (end of batch: next batch)
    if m != "LXJR": END OF LANE
    validate the header at p: ver, kind, reserved = 0, gen = file generation,
        off = p, len in range, p + 64 + data length ≤ file length,
        crc32c, write_seq > prev_write_seq
    if any check fails: END OF LANE
    accept; prev_write_seq = write_seq; p = p + align8(64 + data length)
```

After every lane ends, recovery keeps records with `write_seq > floor`,
merges the lanes by `write_seq` and stops at the first gap (DESIGN §4.1).
Anything a lane holds after its END OF LANE lies after a gap: recovery
ignores it, and `fsck` may scan on to report it (`ignored_after_gap`).

**Why a record cannot be mis-parsed across a torn batch.**

1. A file with a valid header is zero beyond its last write ([§5.1](#51-journal-file-header-block-0)),
   so a region a torn write did not reach reads as zero, never as old data.
2. A record whose header or data includes a region that was not written
   fails its CRC, which covers header and data, and ends the lane.
3. A zero magic at a 4 KiB boundary ends the lane: a batch that did not
   reach the disk is never skipped over, so a later batch is never read
   past a hole.
4. The reader reads a header only at offset 4096, right after a record it
   accepted, or at the 4 KiB boundary after a zero magic found at a
   non-aligned offset. If whole 4 KiB blocks are written or not, that zero
   is the batch's padding and the boundary is the start of the next batch.
   If the device tears inside a block (at sector granularity), the zero may
   be an unwritten sector inside the batch, and the boundary may fall
   inside the batch: on a genuine record header, whose predecessors were
   lost, so its `write_seq` lies after a gap and the merge does not accept
   it; or inside a record's data.
5. Data cannot pass for a header: the header checks require the magic,
   `gen` equal to the file's random 64-bit generation, which the guest never
   sees, `off` equal to the position, and the CRC. Guest data that imitates
   a header, a record from an earlier incarnation of the file, and a record
   copied to another offset are all rejected.

A stale record (one from a previous pass of the ring) cannot survive
reinitialisation; if one did, its `gen` differs and it ends the lane.

Note for the reviewer: D12 ends a lane at the first CRC failure even when
the damaged record is at or below the floor, so media damage to an already
committed record also hides the lane's later records. The checkpoint's lane
tails let recovery skip whole files below the floor, which narrows this;
`gen` and `off` would let a later rule resynchronise at the next valid
header without a format change.

### 5.4 Reuse of the framing

- **DELTA inline records** ([§10.4](#104-delta-body-kind-2)): the decoded
  records region holds records back to back at 8-byte-aligned offsets with
  no padding and no zero magic; `gen` = 0, `off` = offset in the region; the
  region ends at its length.
- **memfd write buffer** (ADR-26, phase 3): the same record header; `gen` is
  the buffer generation of the keeper-held header, `off` the ring offset.
  The buffer header itself is defined in phase 3 (it is not a phase 1
  freeze item).

### 5.5 Maximum counts

| quantity | maximum |
|---|---|
| lanes | 256 (`lane` 0 ..= 255) |
| data of one record | 1 MiB (256 blocks) |
| hole of one record | 2^32 − 1 blocks; a longer DISCARD is several records |
| file length | 64 MiB; records in 4096 .. 64 MiB |

## 6. Extent (freeze item 3)

An extent is one stored chunk (or map node) inside a segment or map pack.
There is one layout for both volume kinds; the bracketed crypto region
exists only when the descriptor's `crypto_mode` is `encrypted`.

### 6.1 How the extent extends `CCH1`

- Bytes 0-15 are a replicix-chunkfmt `CCH1` header exactly as `chunkfmt`
  writes it: magic `"CCH1"`, version 1, 2-byte codec id, `i8` level, 8-byte
  `ulen`. `chunkfmt`'s header checks accept them unchanged.
- Lunix inserts its own fields (`chunk_id`, `clen`, the crypto region)
  between the `CCH1` header and the `CCH1` payload and appends the 16-byte
  tag. A whole extent is therefore **not** a `CCH1` object, and
  `chunkfmt::decode_object(extent)` must not be called on it.
- The `CCH1` object of an extent is `extent[0..16] ‖ P`, where `P` is the
  payload (plain extent) or the AEAD-opened payload (encrypted extent). It
  decodes with `chunkfmt::decode_object`, or with
  `StreamingDecoder::new(&extent[0..16], …)` fed `P`. The payload framing is
  `chunkfmt`'s: codec 1 is one zstd frame, codec 2 an `lz4_flex` raw block
  with no size prefix (the size is `ulen`), codec 0 the bytes.
- Lunix narrows `CCH1`: `ulen` ≤ 1 MiB, codec ≤ 3, and decompression is
  capped by the volume's chunk size (data) or 64 KiB (nodes), never by
  `ulen` (DESIGN §15).

### 6.2 Layout

| field | offset (plain) | offset (encrypted) | width | encoding | legal range | notes |
|---|---|---|---|---|---|---|
| `magic` | 0 | 0 | 4 | bytes | `"CCH1"` | `CCH1` |
| `ver` | 4 | 4 | 1 | u8 | 1 | `CCH1` format version |
| `codec` | 5 | 5 | 2 | u16 | 0 ..= 3 | `CCH1` codec id |
| `level` | 7 | 7 | 1 | i8 | see `Level` ([§3](#3-primitives-freeze-item-1)) | `CCH1` level |
| `ulen` | 8 | 8 | 8 | u64 | data: = the volume chunk size; node: 1 ..= 65 536 | `CCH1` uncompressed length |
| `chunk_id` | 16 | 16 | 32 | bytes | | data: the chunk id; node: the node hash |
| `clen` | 48 | 48 | 4 | u32 | codec 0: = `ulen`; codec 1-3: 1 ..= `ulen` | payload length in bytes, tag excluded |
| `dek_gen` | — | 52 | 4 | u32 | a generation the keyring can derive | encrypted only |
| `salt` | — | 56 | 24 | bytes | random | encrypted only; drawn inside the seal function on every call (ADR-11) |
| `nonce` | — | 80 | 12 | bytes | default profile: all zero (a reader rejects any other value); fips: the module nonce | encrypted only (F10) |
| `payload` | 52 | 92 | `clen` | bytes | | codec output; ciphertext when encrypted |
| `tag` | 52 + `clen` | 92 + `clen` | 16 | bytes | | present in **every** extent (D11) |

`kind` (data or node), `family_id`, `id_len`, the crypto mode and the
profile are not stored in the extent: they come from the container
([§7](#7-segment-and-map-pack-container-freeze-item-4)) and the descriptor.
Extents are packed back to back in their container with no padding.

### 6.3 AAD, key-derivation `info` and plain tag input

Encrypted extents use canonical re-encodings with the KAT widths (D2). Both
reproduce `aad_hardened` (61 bytes with a 32-byte id) and `info_v2`
(100 bytes) of `bench/crypto-kat/raw/kat-vectors.json` byte for byte.

AAD:

| part | width | source |
|---|---|---|
| `ver` | 1 | header `ver` |
| `codec` | 1 | header `codec` (≤ 255, checked before encoding) |
| `level` | 1 | header `level` as a byte |
| `ulen` | 4 | header `ulen` as u32 (≤ 1 MiB, checked before encoding) |
| `kind` | 1 | `ExtentKind` from the container |
| `dek_gen` | 4 | header `dek_gen` |
| `family_id` | 16 | descriptor |
| `id_len` | 1 | descriptor `id_len` (0, 16 or 32), not the length of the field |
| `chunk_id` | 32 | header `chunk_id`, always all 32 bytes |

`info`:

| part | width | source |
|---|---|---|
| label | 16 | `"lunix extent v2\0"` = `6c 75 6e 69 78 20 65 78 74 65 6e 74 20 76 32 00` |
| `family_id` | 16 | descriptor |
| `kind` | 1 | container |
| `dek_gen` | 4 | header |
| `codec` | 1 | header |
| `level` | 1 | header |
| `clen` | 4 | header |
| `id_len` | 1 | descriptor |
| `chunk_id` | 32 | header |
| `salt` | 24 | header |

Extent key = `keyed BLAKE3(DEK_gen, info)` (default) or
`HKDF-SHA-256-Expand(DEK_gen, info, 32)` (fips). Cipher: AES-256-GCM with the
`nonce` field as nonce (all zero in the default profile), AAD as above,
plaintext = the codec output, ciphertext = `payload`, tag = `tag`.

**Plain tag input** (plain and integrity modes, D11): the contiguous slice
`extent[0 .. 52 + clen]`, that is every byte before the tag (`CCH1` header,
`chunk_id`, `clen`, payload). The tag is the first 16 bytes of
`BLAKE3-256(slice)` in plain mode and of `keyed BLAKE3(extent tag key,
slice)` in integrity mode.

**Authenticated by:** encrypted: the AEAD (header fields through the AAD;
`clen`, `salt`, `dek_gen`, `codec` and `level` also through the key; `nonce`
as the GCM nonce; a non-zero default-profile nonce is rejected before the
AEAD). Plain and integrity: the tag over every byte before it (keyed only in
integrity mode). In every mode the chunk id is authenticated by the Merkle
map and, when the reader asks, by recomputing it (ADR-11 "Integrity").

### 6.4 Extent length and `OVH`

An extent occupies `OVH + clen` bytes, with `OVH` = 68 (plain and integrity:
52 + 16) or 108 (encrypted: 92 + 16). The leaf codec uses `OVH` to derive
offsets within a run ([§8.3](#83-leaf-encoding-rb16)).

### 6.5 Maximum counts

| quantity | maximum |
|---|---|
| data `ulen` | the chunk size, ≤ 1 MiB |
| node `ulen` | 65 536 |
| `clen` | `ulen` |

## 7. Segment and map-pack container (freeze item 4)

`segments/<seg:016x>` (data extents, `ExtentKind` 0) and `map/<pack:016x>`
(map nodes, `ExtentKind` 1) share one container:

```
[extent]*  [footer envelope]  [trailer (64 bytes)]
```

Locally and in the bucket the bytes are identical (ADR-7).

### 7.1 Trailer (last 64 bytes)

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `footer_off` | 0 | 8 | u64 | = length of the extent region | the footer starts here |
| `footer_len` | 8 | 4 | u32 | ≥ 44 | length of the footer envelope |
| `extent_count` | 12 | 4 | u32 | = the footer's `extent_count` | |
| `object_id` | 16 | 8 | u64 | = the id in the object's name | |
| `ver` | 24 | 1 | u8 | 1 | container version; it also versions the node encoding of a map pack |
| `container_kind` | 25 | 1 | u8 | 0 segment, 1 map pack | must match the directory (`segments/` or `map/`) |
| reserved | 26 | 30 | | zero | |
| `crc32c` | 56 | 4 | u32 | `crc32c(bytes 0..56)` | |
| `magic` | 60 | 4 | bytes | `"LXST"` | |

Rule: `footer_off + footer_len + 64` = container length, and the container
length is at most 2^32 − 1 bytes (so every `Hint.off` fits a u32). **A
trailer is not authenticated**; a wrong value only makes the footer fail
its MAC or AEAD.

### 7.2 Footer

The footer region is a metadata envelope ([§9](#9-metadata-envelope-freeze-item-7))
of object kind 6 (`footer`), with AAD name the container's bucket-relative
name (`segments/<seg:016x>` or `map/<pack:016x>`). Form 0 in plain and
integrity mode, form 1 (sealed) in encrypted mode. The envelope body is a
single zstd frame (any level, with the content size in the frame header)
whose decoded bytes are the footer body:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `ver` | 0 | 1 | u8 | 1 | |
| `container_kind` | 1 | 1 | u8 | = trailer | |
| reserved | 2 | 2 | | zero | |
| `extent_count` | 4 | 4 | u32 | 1 ..= `floor(footer_off / 69)` | |
| `object_id` | 8 | 8 | u64 | = trailer | |
| `extents_len` | 16 | 8 | u64 | = trailer `footer_off` | |
| `write_seq_lo` | 24 | 8 | u64 | | lowest `write_seq` of the journal records whose data is in this container; 0 when none (compaction output, map packs) |
| `write_seq_hi` | 32 | 8 | u64 | ≥ `write_seq_lo` | highest such `write_seq`; 0 when none |
| `seg_hash` | 40 | 32 | bytes | `BLAKE3-256(container[0 .. footer_off])` | the segment-level BLAKE3 of DESIGN §6 |
| entries | 72 | `48 × extent_count` | | | sorted by (`chunk_id`, `off`) ascending, strictly |

Entry (48 bytes):

| field | offset | width | encoding | legal range |
|---|---|---|---|---|
| `chunk_id` | 0 | 32 | bytes | = the extent's `chunk_id` (the node hash in a map pack) |
| `off` | 32 | 4 | u32 | offset of the extent in the container |
| `clen` | 36 | 4 | u32 | = the extent's `clen` |
| `ulen` | 40 | 4 | u32 | = the extent's `ulen` |
| `codec` | 44 | 1 | u8 | = the extent's `codec` |
| reserved | 45 | 3 | | zero |

The extents named by the entries, each `OVH + clen` bytes long, tile the
extent region `[0, footer_off)` exactly: no gap, no overlap. The same
`chunk_id` may appear twice (two copies written in one cut). The decoder
caps the zstd output at `72 + 48 × floor(footer_off / 69)` bytes (an extent
is at least 69 bytes) and rejects a larger frame.

**Authenticated by:** the envelope's MAC (form 0) or AEAD (form 1), which
also authenticates `seg_hash` and so the extent region as a whole. Each
extent is also self-authenticating ([§6](#6-extent-freeze-item-3)).

### 7.3 Maximum counts

| quantity | maximum |
|---|---|
| container length | 2^32 − 1 bytes (segment target 16 MiB, DESIGN §6) |
| `extent_count` | `floor(footer_off / 69)` |
| decoded footer body | `72 + 48 × extent_count` |

## 8. Map node (freeze item 5)

Nodes are stored as extents (`ExtentKind` 1) in map packs, codec 0 in phase
1 (codec 1 later needs no format change, D1). The node encoding is the
extent's plaintext `CCH1` object payload.

### 8.1 Node header (3 bytes)

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `level` | 0 | 1 | u8 | 0 = leaf; 1 ..= 15 interior | the leaves are level 0, the root is level `levels − 1` |
| `fanout` | 1 | 2 | u16 | = descriptor `map_fanout` (256 or 512) | |

No codec byte and no version (D1). The node encoding is versioned by the
map pack's container `ver` ([§7.1](#71-trailer-last-64-bytes)).

### 8.2 Tree shape

- `nchunks = ceil(size / chunk_size)`; `levels = max(2, ceil(log_fanout(nchunks)))`
  (leaf plus at least one interior level). `levels` is derived from the
  commit's `size` and is not stored.
- Chunk `ci = byte offset / chunk_size`. Leaf index `li = ci / fanout`, slot
  in the leaf `ci % fanout`. At interior level L the child slot is
  `(li / fanout^(L-1)) % fanout`.
- Entries beyond `nchunks` are holes. Growing wraps the old root as child 0
  of new roots (M5).

### 8.3 Leaf encoding (`rb16`)

A leaf holds exactly `fanout` entries: `hole` or `{lineage_idx, seg, off,
clen, id_prefix}`. It is run-encoded with bit-packed columns (ADR-8,
`spikes/radixmap/src/leaf.rs` plus the lineage column of ADR-25). Bit
fields are packed LSB first into a byte stream; every column below starts on
a byte boundary and its unused high bits in the last byte are zero.
`bits(v)` is the number of significant bits of `v` (`bits(0) = 0`), and
`sb = log2(fanout)` (8 or 9).

| part | offset | width | encoding | legal range / rule |
|---|---|---|---|---|
| node header | 0 | 3 | [§8.1](#81-node-header-3-bytes) | `level` = 0 |
| `nruns` | 3 | 2 | u16 | 1 ..= `fanout` |
| `ndata` | 5 | 2 | u16 | 0 ..= `fanout`: non-hole entries |
| `ndict` | 7 | 2 | u16 | 0 ..= `ndata`: segment dictionary entries |
| `wseg` | 9 | 1 | u8 | = `bits(ndict)` |
| `wsd` | 10 | 1 | u8 | = `bits(max dict seg − base)`, ≤ 64 |
| `woff` | 11 | 1 | u8 | = `bits(max start_off of data runs)`, ≤ 32 |
| `wc` | 12 | 1 | u8 | = `bits(max clen)`, ≤ 32 |
| `wcum` | 13 | 1 | u8 | = `bits(max cum value)`, ≤ 48 |
| `base` | 14 | 8 | u64 | = min dict seg (0 if `ndict` = 0) |
| seg dictionary | 22 | `ceil(ndict × wsd / 8)` | `ndict` values `seg − base`, `wsd` bits each | |
| lineage column | next | `ndict` | u8 each | `lineage_idx` of dictionary entry `i`; all 0 in v1 |
| run table | next | `ceil(nruns × rw / 8)` | records of `rw = sb + wseg + woff + (sb + 1)` bits | |
| `clen` column | next | `ceil(ndata × wc / 8)` | `wc` bits each | `clen` of the k-th non-hole entry |
| `cum` column | next | `ceil(ceil(ndata/16) × wcum / 8)` | `wcum` bits each | `cum[g] = Σ_{j < 16g} (OVH + clen_j)` |
| id column | next | `ndata × id_len` | bytes | `id_prefix` of the k-th non-hole entry |

Run record fields, LSB first: `start` (`sb` bits, the first entry index of
the run), `seg_idx` (`wseg` bits: 0 = hole run, `i` ≥ 1 = dictionary entry
`i − 1`), `start_off` (`woff` bits, the `off` of the run's first entry; 0
for a hole run), `rank` (`sb + 1` bits, the number of non-hole entries
before the run).

Canonical form (required; a decoder rejects anything else):

- The dictionary holds the distinct `(lineage_idx, seg)` pairs of the
  leaf's data entries, sorted ascending by `(lineage_idx, seg)`, each once.
- Runs are maximal. Entry `i` continues the run of entry `i − 1` iff both
  are holes, or both are data with the same `(lineage_idx, seg)` and
  `off_i = off_{i-1} + OVH + clen_{i-1}`. The first run has `start` 0;
  `start` strictly increases.
- Every width is the minimal one stated above; padding bits are zero; the
  leaf ends exactly after the id column.
- A leaf whose entries are all holes is the empty leaf and is never
  stored ([§8.5](#85-node-hash-and-empty-hashes)).

Lookup of entry `i`: find the last run `r` with `start ≤ i`; if `seg_idx` =
0 it is a hole; else `k = rank_r + (i − start_r)`, `seg = base +
dict[seg_idx − 1]`, `lineage_idx = lineage[seg_idx − 1]`, `clen = clen[k]`,
`off = start_off_r + S(k) − S(rank_r)` with `S(k) = cum[k / 16] +
Σ_{16·(k/16) ≤ j < k} (OVH + clen_j)`, `id_prefix = ids[k]`.

### 8.4 Interior node

| part | offset | width | encoding | rule |
|---|---|---|---|---|
| node header | 0 | 3 | [§8.1](#81-node-header-3-bytes) | `level` ≥ 1 |
| children | 3 | `48 × fanout` | entries | |

Child entry (48 bytes):

| field | offset | width | encoding | rule |
|---|---|---|---|---|
| `child_hash` | 0 | 32 | bytes | the child's node hash; the empty hash of level `level − 1` for an absent subtree |
| `pack` | 32 | 8 | u64 (`PackId`) | 0 iff the child is empty |
| `off` | 40 | 4 | u32 | offset of the child's extent in the pack; 0 if empty |
| `len` | 44 | 4 | u32 | whole extent length (`OVH + clen`); 0 if empty |

An interior node whose children are all empty is the empty node of its
level and is never stored. Size: 24 579 bytes at fanout 512, 12 291 at 256.

### 8.5 Node hash and empty hashes

| node | hash input | function |
|---|---|---|
| leaf | the whole leaf encoding (header and body, hints included) | `H` |
| interior | node header (3 bytes) ‖ `child_hash` × `fanout` (**no locations**) | `H` |

`H` is BLAKE3-256 in plain and integrity mode, keyed BLAKE3 under the
map-node addressing key in encrypted mode (default profile), HMAC-SHA-256
under it (fips). The level byte separates leaf (0) from interior (≥ 1)
inputs. Locations are hints, so two stores with the nodes at different pack
offsets have the same root (ADR-8).

Empty hashes: `E0 = H(empty leaf)`, `E_L = H(header(L, fanout) ‖ E_{L-1} ×
fanout)`. The empty leaf is the header, `nruns` = 1, every other field 0,
and one all-zero run record (3 bytes): 25 bytes for both fanouts. An empty
child is referenced as `{E_{L-1}, 0, 0, 0}`; an empty map's root is
`E_{levels-1}` with a zero location. The unkeyed constants (plain and
integrity mode) are in [§23.7](#237-empty-leaf-and-empty-hashes).

**Authenticated by:** the parent's `child_hash` (the root's by the commit)
and the extent tag or AEAD of the node's extent.

### 8.6 Maximum counts

| quantity | maximum |
|---|---|
| entries per leaf | `fanout` |
| dictionary entries | `ndata` ≤ `fanout` |
| levels | 15 (with `size` ≤ 2^56 bytes at 16 KiB chunks and fanout 256: 6) |
| encoded node | 65 536 bytes |

## 9. Metadata envelope (freeze item 7)

Every metadata object (commits, lease, footers, pins, `heat/`, `gc/`,
`gc/space/`, `dicts/`) is stored in this envelope.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 4 | bytes | `"LXM1"` | |
| `env_ver` | 4 | 1 | u8 | 1 | |
| `kind` | 5 | 1 | u8 | [§4.3](#43-object-kinds) | must equal the kind the reader expects from the object's name; for `commits/` either 1 or 3 |
| `form` | 6 | 1 | u8 | 0 = plain body + `mac`; 1 = sealed; 2 reserved (fips AES-GCM form, not defined in v1) | must match the descriptor's mode: 0 for `plain` and `integrity`, 1 for `encrypted` |
| reserved | 7 | 1 | | zero | |
| `body_len` | 8 | 4 | u32 | ≤ the kind's maximum ([§9.1](#91-maximum-body-lengths)) | plaintext body length |
| form 0: `body` | 12 | `body_len` | | | |
| form 0: `mac` | 12 + `body_len` | 32 | bytes | | |
| form 1: `nonce` | 12 | 24 | bytes | random | XChaCha20-Poly1305 nonce |
| form 1: `ciphertext` | 36 | `body_len` | bytes | | |
| form 1: `tag` | 36 + `body_len` | 16 | bytes | | |

- Form 0: `mac = HMAC-SHA-256(integrity HMAC key, meta_aad(volume, kind, name) ‖ envelope[0 .. 12 + body_len])`
  in integrity mode, and `BLAKE3-256` of the same input in plain mode (a
  checksum only).
- Form 1: XChaCha20-Poly1305 under `metadata key(kind)` (purpose 0x03), AAD
  = `meta_aad(volume, kind, name)` exactly (ADR-11), plaintext = the body.
- Every body starts with its own version (`body_ver` u8, or `ver` u16 for
  opaque bodies), which the AEAD or MAC covers.
- **Authenticated by:** the MAC (integrity mode), the AEAD (encrypted
  mode), nothing but a checksum (plain mode). The binding to the volume,
  kind and bucket-relative name is part of both.

### 9.1 Maximum body lengths

| kind | maximum `body_len` |
|---|---|
| 1 `commit` | 4 MiB |
| 3 `delta` | 68 MiB |
| 2 `lease`, 8 `readers`, 9 `holds`, 10 `snapshots` | 4 KiB |
| 6 `footer` | `footer_len − 44` (form 0) or `− 52` (form 1), and the decoded body cap of [§7.2](#72-footer) |
| 5 `heat`, 7 `gc`, 11 `dicts`, 12 `space` | 64 MiB + 8 |

## 10. Commits (freeze item 6)

A commit is a metadata envelope ([§9](#9-metadata-envelope-freeze-item-7))
named `commits/<commit_seq:016x>` (locally `state/commits/<commit_seq:016x>`,
sealed under the bucket-relative name, D9). Object kind 1 (`commit`) for
FULL, EPOCH, CONFIG and TRANSFER; 3 (`delta`) for DELTA. The envelope's MAC
or AEAD authenticates the whole body; on unencrypted volumes that MAC is the
`mac` of DESIGN §8.2 and covers `epoch` and `parent_hash`.

### 10.1 Common header (body bytes 0-71, all five kinds)

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `body_ver` | 0 | 1 | u8 | 1 | |
| `commit_kind` | 1 | 1 | u8 | 1 FULL, 2 DELTA, 3 EPOCH, 4 CONFIG, 5 TRANSFER | DELTA iff envelope kind 3 |
| `flags` | 2 | 2 | u16 | bit 0 `has_parent`; bit 1 `has_heat` (FULL only); other bits zero | |
| reserved | 4 | 4 | | zero | |
| `commit_seq` | 8 | 8 | u64 | = the seq in the object name | |
| `epoch` | 16 | 8 | u64 | 0 in phase 1; never below the parent's | |
| `parent_seq` | 24 | 8 | u64 | `has_parent`: `commit_seq − 1`; else 0 | `has_parent` is clear exactly when `commit_seq` = 0 |
| `parent_hash` | 32 | 32 | bytes | | BLAKE3-256 of the parent commit object's bytes as stored (the whole envelope); for genesis, of the exact `volume` bytes (D5) |
| `created_at` | 64 | 8 | u64 (`TimeMs`) | ≥ the parent's | used by `mount-at --time` and retention |

### 10.2 FULL body (kind 1)

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| common header | 0 | 72 | [§10.1](#101-common-header-body-bytes-0-71-all-five-kinds) | | |
| `cut_seq` | 72 | 8 | u64 (`WriteSeq`) | ≥ the previous cut | 0 for genesis |
| `size` | 80 | 8 | u64 | 4096 ..= 2^56, multiple of 4096 | device size in bytes |
| `chain_floor` | 88 | 8 | u64 (`CommitSeq`) | ≤ `commit_seq`; never below the parent's floor | the oldest retained FULL (D5) |
| `map_root.hash` | 96 | 32 | bytes | | root node hash; `E_{levels-1}` for an empty map |
| `map_root.pack` | 128 | 8 | u64 | 0 iff the map is empty | location of the root node's extent |
| `map_root.off` | 136 | 4 | u32 | | |
| `map_root.len` | 140 | 4 | u32 | | whole extent length |
| `heat_seq` | 144 | 8 | u64 | `has_heat`: the `commit_seq` of `heat/<commit_seq:016x>`; else 0 | `heat?` |
| `n_map_packs` | 152 | 4 | u32 | 0 ..= 1 024 | |
| `n_new_segments` | 156 | 4 | u32 | 0 ..= 262 144 | |
| `n_index_packs` | 160 | 4 | u32 | 0 ..= 4 096 | |
| reserved | 164 | 4 | | zero | |
| `map_packs[]` | 168 | `8 × n_map_packs` | u64 each | sorted ascending, unique | packs this commit introduced (written since its parent commit) |
| `new_segments[]` | next | `8 × n_new_segments` | u64 each | sorted ascending, unique | segments this commit introduced, compaction output included |
| `index_packs[]` | next | `8 × n_index_packs` | u64 each | sorted ascending, unique | every index run of the manifest that was durable before C6 of this commit (D3 lag rule); runs covering this commit's segments come in the next FULL |

The genesis commit is a FULL with `commit_seq` 0, `has_parent` clear,
`parent_hash` = BLAKE3-256 of `volume`, `cut_seq` 0, `chain_floor` 0, the
empty map and empty lists (example [§23.6](#236-full-commits-genesis-and-commit-1)).

### 10.3 Why FULL names its root's location

An interior entry carries its child's location; the root has no parent, so
the commit carries it. Like every location, it is not part of any hash
input other than the commit's own MAC or AEAD.

### 10.4 DELTA body (kind 2)

Phase 2+ writes DELTAs; phase 1 freezes and round-trips the layout.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| common header | 0 | 72 | | | envelope kind 3 |
| `prev_cut_seq` | 72 | 8 | u64 | = the cut of the newest FULL or DELTA below this commit | records cover `(prev_cut_seq, cut_seq]` |
| `cut_seq` | 80 | 8 | u64 | > `prev_cut_seq` | `max write_seq` of the flush group |
| `size` | 88 | 8 | u64 | as FULL | device size at this cut (a resize between FULLs replays) |
| `records_len` | 96 | 4 | u32 | ≤ 64 MiB | stored length of the records region |
| `records_ulen` | 100 | 4 | u32 | ≤ 64 MiB | decoded length; codec 0: = `records_len` |
| `n_refs` | 104 | 2 | u16 | 0 ..= 4 096 | |
| `records_codec` | 106 | 1 | u8 | 0 raw, 1 zstd (one frame) | |
| reserved | 107 | 5 | | zero | |
| `refs[]` | 112 | `48 × n_refs` | entries | | |
| records | next | `records_len` | | | decoded: journal records ([§5.4](#54-reuse-of-the-framing)), `write_seq` strictly increasing within `(prev_cut_seq, cut_seq]` |

Ref entry (48 bytes): `kind` u8 at 0 (1 `segment`, 2 `journal`: reserved
and unused in v1, ADR-28), reserved 7 bytes, `id` u64 at 8 (the `ObjId` of a
segment; for `journal` defined with ADR-29), `hash` 32 bytes at 16
(BLAKE3-256 of the referenced object's bytes as stored).

### 10.5 EPOCH, CONFIG and TRANSFER bodies (kinds 3, 4, 5)

EPOCH:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| common header | 0 | 72 | | | |
| `authority_len` | 72 | 2 | u16 | 0 = absent, else 1 ..= 64 | `authority?` (D10) |
| reserved | 74 | 6 | | zero | |
| `authority` | 80 | `authority_len` | backend id | | |

CONFIG:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| common header | 0 | 72 | | | |
| `config_version` | 72 | 8 | u64 | ≥ 1, rises by 1 per CONFIG or TRANSFER | |
| `n_backends` | 80 | 2 | u16 | 0 ..= 64 | |
| `detached` | 82 | 1 | u8 | 0 none, 1 `local` | `detached?` (ADR-26) |
| reserved | 83 | 1 | | zero | |
| `authority_len` | 84 | 2 | u16 | 1 ..= 64; 0 only with `detached` = 1 and `n_backends` = 0 | |
| `policy_ver` | 86 | 2 | u16 | 0 when `policy_len` = 0 | the policy body is versioned by the phase that defines it (phase 2/5) |
| `policy_len` | 88 | 4 | u32 | 0 ..= 65 536 | |
| reserved | 92 | 4 | | zero | |
| `authority` | 96 | `authority_len` | backend id | names a backend whose `role` is authority | |
| `backends[]` | next | variable | entries below | ids unique, sorted by id | |
| `policy` | next | `policy_len` | bytes | | |

Backend entry: `id_len` u16 at 0 (1 ..= 64), `url_len` u16 at 2
(1 ..= 2 048), `role` u8 at 4 (1 authority, 2 mirror), `state` u8 at 5
(1 Joining, 2 CatchingUp, 3 InSync, 4 Draining, 5 Removed), reserved 2 bytes,
`since_seq` u64 at 8 (`CommitSeq`), then `id` and `url` bytes. Exactly one
entry has role authority unless the volume is detached.

TRANSFER:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| common header | 0 | 72 | | | `epoch` = the new epoch `e + 1` |
| `config_version` | 72 | 8 | u64 | the previous `config_version` + 1 | |
| `reason` | 80 | 1 | u8 | 1 planned (`set-authority`), 2 steal | |
| reserved | 81 | 1 | | zero | |
| `to_len` | 82 | 2 | u16 | 1 ..= 64 | |
| reserved | 84 | 4 | | zero | |
| `to` | 88 | `to_len` | backend id | | |

### 10.6 Maximum counts

| quantity | maximum |
|---|---|
| FULL `map_packs[]` / `new_segments[]` / `index_packs[]` | 1 024 / 262 144 / 4 096 |
| DELTA `refs[]`, records | 4 096; 64 MiB decoded |
| CONFIG `backends[]`, `policy` | 64; 64 KiB |
| commit body | 4 MiB (kind 1), 68 MiB (kind 3) |

### 10.7 Chain rules

- `commit_seq = parent_seq + 1`; commit numbers are contiguous; genesis is
  0 (D5).
- `parent_hash` of commit *n* = BLAKE3-256 of the bytes of commit *n − 1*
  as stored. `epoch` never decreases. `created_at` never decreases.
- Mount and fsck verify the chain from the tip down to `chain_floor`; the
  floor commit's parent may be absent (D5).
- **No automatic fallback** (review 2, finding 8): a tip that fails to
  decode or authenticate is a typed mount error. Falling back would reuse
  the create-only name `commits/<tip>` and lose journal records already
  reclaimed. The repair is offline: `lunix fsck --repair --truncate-tip`
  moves the damaged object aside and makes `tip − 1` the tip.
- Every object a commit names is durable before the commit (ADR-9); a FULL
  names only index runs durable before its C6 (D3).
- The tip's high-water mark is the highest segment or pack id any retained
  commit names; ids above it are never condemned by GC (review 2,
  finding 5) and are orphans only at open (DESIGN §4.4).

## 11. `volume` descriptor (freeze item 8)

`volume` is create-only, plaintext, and MAC'd. The local file `volume`
holds the exact same bytes (D8), so `backend add` uploads it verbatim and
the genesis `parent_hash` stays valid.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXVOL"` | |
| `format_ver` | 8 | 2 | u16 | 1 | |
| `crypto_mode` | 10 | 1 | u8 | 0 plain, 1 integrity, 2 encrypted | [§4.1](#41-crypto-modes) |
| `profile` | 11 | 1 | u8 | 0 default, 1 fips (only with encrypted) | |
| `block_size` | 12 | 4 | u32 | 4096 | |
| `chunk_size` | 16 | 4 | u32 | power of two, 16 384 ..= 1 048 576 | ADR-5 |
| `map_fanout` | 20 | 2 | u16 | 256, 512 | D1 |
| `id_len` | 22 | 1 | u8 | 16, 32 (0 decodes, refused at create in v1) | |
| `codec_policy` | 23 | 1 | u8 | 0 none, 1 auto, 2 lz4, 3 zstd | ADR-12 |
| `codec_level` | 24 | 1 | i8 | none, lz4: 0; auto: 1 ..= 22 (the zstd level after the probe); zstd: −7 ..= 22, not 0 | |
| `lineage_count` | 25 | 1 | u8 | 0 in v1; ≤ 255 | ADR-25 |
| reserved | 26 | 6 | | zero | |
| `volume_uuid` | 32 | 16 | bytes | | |
| `family_id` | 48 | 16 | bytes | | |
| `kmk_fingerprint` | 64 | 16 | bytes | plain: zero; else the first 16 bytes of purpose 0x05 of the root | the KMK fingerprint (the IK's in integrity mode) |
| `created_at` | 80 | 8 | u64 (`TimeMs`) | | |
| `lineage[]` | 88 | variable | entries below | | always empty in v1 |
| `mac` | end − 32 | 32 | bytes | | the descriptor MAC |

Lineage entry: `volume_uuid` 16 bytes, `base_seq` u64 (`CommitSeq`),
`backend_hint_len` u16 (0 ..= 1 024), `backend_hint` UTF-8.

Descriptor MAC: input = `"lunix volume v1\0"` (the 16 bytes
`6c 75 6e 69 78 20 76 6f 6c 75 6d 65 20 76 31 00`) ‖ every byte of the
descriptor before `mac`. `mac = HMAC-SHA-256(descriptor MAC key, input)` in
integrity and encrypted mode (purpose 0x04 of the root), `BLAKE3-256(input)`
in plain mode (a checksum: a plain volume has no key, ADR-11 F7). Length
with no lineage: 120 bytes.

**Authenticated by:** the descriptor MAC in keyed modes; the attach check
([attach-matrix.md](attach-matrix.md)) decides when that MAC must verify.
The descriptor holds no key slots and no size (the size lives in the
commits).

## 12. Keyring (freeze item 9)

`families/<family-id>/keyring` (locally `<dir>/families/<family-id>/keyring`)
is the one mutable key object of a family: written by CAS on the authority,
copied byte-identically to mirrors (ADR-11 F6). A plain family has none.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXKRG"` | |
| `ver` | 8 | 2 | u16 | 1 | |
| `root_kind` | 10 | 1 | u8 | 1 KMK (encrypted family), 2 IK (integrity family) | must match the descriptor's mode |
| `slot_count` | 11 | 1 | u8 | 1 ..= 32 | |
| `dek_gen` | 12 | 4 | u32 | KMK: the current DEK generation; IK: 0 | new extents use it; old extents keep theirs |
| `family_id` | 16 | 16 | bytes | = the path's family | |
| `keyring_seq` | 32 | 8 | u64 | ≥ 1; rises on every change | a keyring below the highest seen is rejected |
| `next_slot_id` | 40 | 4 | u32 | > every `slot_id` | slot ids are never reused |
| reserved | 44 | 4 | | zero | |
| `slots[]` | 48 | variable | entries below | sorted by `slot_id`, unique | |
| `mac` | end − 32 | 32 | bytes | | |

Slot entry:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `slot_id` | 0 | 4 | u32 | ≥ 1 | |
| `kind` | 4 | 1 | u8 | 1 passphrase (Argon2id), 2 key file, 3 KMS | |
| reserved | 5 | 1 | | zero | |
| `params_len` | 6 | 2 | u16 | kind 1: 28; kind 2: 0; kind 3: 1 ..= 1 024 | |
| `wrap_len` | 8 | 2 | u16 | kinds 1-2: 73; kind 3: 1 ..= 4 096 | |
| reserved | 10 | 2 | | zero | |
| `params` | 12 | `params_len` | | | kind 1: `m_kib` u32, `t` u32, `p` u32, `salt` [16]; kind 3: `key_ref` UTF-8 |
| `wrap` | next | `wrap_len` | | | kinds 1-2: `01` ‖ nonce [24] ‖ XChaCha20-Poly1305 ciphertext of the 32-byte root [32] ‖ tag [16]; kind 3: the KMS ciphertext |

- KEK: kind 1 = Argon2id(passphrase, `salt`, `m_kib`, `t`, `p`, 32 bytes);
  kind 2 = the key file's 32 bytes; kind 3 = the KMS, with encryption
  context `{family, slot, key_ref}` (ADR-11).
- Slot AAD (kinds 1-2), matching `slot_aad_pass` of the KAT:
  `"lunix slot v1\0"` ‖ `family_id` ‖ `slot_id` u32 ‖ `kind` u8 ‖ canonical
  params (kind 1: `m_kib`, `t`, `p` as u32 and `salt`; kind 2: nothing;
  kind 3: `key_ref` length as u32 and bytes).
- Argon2id bounds (constants): read: `m_kib` ≤ 1 048 576 (1 GiB), 1 ≤ `t` ≤
  10, 1 ≤ `p` ≤ 16, `m_kib` ≥ 8 · `p`; write: `m_kib` ≥ 19 456 (19 MiB),
  `t` ≥ 2.
- `mac = HMAC-SHA-256(keyring MAC key, meta_aad(family_id, 13,
  "families/<family-id>/keyring") ‖ keyring[0 .. end − 32])`, the key being
  purpose 0x07 of the root a slot unwraps. A reader unwraps a slot, then
  verifies the MAC, then checks `keyring_seq` against `state/pins`.
- **Authenticated by:** each slot by its AEAD (or KMS context); the whole
  body, including `keyring_seq` and `dek_gen`, by the keyring MAC.
- Maximum: 32 slots; whole keyring ≤ 192 KiB.

## 13. Lease (freeze item 10)

`lease` is a metadata envelope of kind 2 named `lease` (phase 2 writes it;
phase 1 freezes the body). `epoch` is in the plaintext of the body, inside
the AEAD or MAC (ADR-11 F12).

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `body_ver` | 0 | 1 | u8 | 1 | |
| `released` | 1 | 1 | u8 | 0, 1 | |
| `host_len` | 2 | 2 | u16 | 1 ..= 255 | |
| `cluster_len` | 4 | 2 | u16 | 0 ..= 255 | 0 = standalone |
| `transferred_to_len` | 6 | 2 | u16 | 0 = absent, else 1 ..= 64 | `transferred_to?` (D10) |
| `epoch` | 8 | 8 | u64 | | a lease below the newest verified epoch is rejected |
| `deadline` | 16 | 8 | u64 (`TimeMs`) | | |
| `owner.host` | 24 | `host_len` | UTF-8 | | |
| `owner.cluster` | next | `cluster_len` | UTF-8 | | |
| `transferred_to` | next | `transferred_to_len` | backend id | | |

**Authenticated by:** the envelope (the `mac` of DESIGN §8.5 on unencrypted
volumes).

## 14. Pins: `snapshots/`, `readers/`, `holds/` (freeze item 7)

Each is a metadata envelope with the bodies of DESIGN §6.1. Locally they are
mirrored under the same names (empty in phase 1, but read by GC, D15).

`snapshots/<name>` (kind 10):

| field | offset | width | encoding | legal range |
|---|---|---|---|---|
| `body_ver` | 0 | 1 | u8 | 1 |
| `consistency` | 1 | 1 | u8 | 1 crash, 2 fs-frozen |
| `label_len` | 2 | 2 | u16 | 0 ..= 1 024 |
| reserved | 4 | 4 | | zero |
| `commit_seq` | 8 | 8 | u64 | a FULL commit |
| `created` | 16 | 8 | u64 (`TimeMs`) | |
| `label` | 24 | `label_len` | UTF-8 | |

`readers/<host>-<id>` (kind 8): `body_ver` u8 = 1 at 0, reserved 7 bytes,
`commit_seq` u64 at 8, `deadline` u64 (`TimeMs`) at 16. 24 bytes.

`holds/<child-uuid>` (kind 9): `body_ver` u8 = 1 at 0, reserved 7 bytes,
`base_seq` u64 (`CommitSeq`) at 8. 16 bytes.

**Authenticated by:** the envelope. Maximum body: 4 KiB.

## 15. Opaque bodies: `heat/`, `dicts/`, `gc/space/` (freeze item 7)

Only the envelope and the kind id are frozen (D6). The body is:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `ver` | 0 | 2 | u16 | ≥ 1 | defined by the writer of the kind |
| reserved | 2 | 2 | | zero | |
| `len` | 4 | 4 | u32 | ≤ 64 MiB | |
| `bytes` | 8 | `len` | | | |

| object | kind | name | `bytes` defined by |
|---|---|---|---|
| `heat/<commit_seq:016x>` | 5 | `heat/…` | phase 4 (ADR-23) |
| `dicts/<id:08x>` | 11 | `dicts/…` | `replicix-zdict` (M18a, ADR-30) |
| `gc/space/<commit_seq:016x>` | 12 | `gc/space/…` | phase 4 (reserved) |
| `gc/<pass:016x>` | 7 | `gc/…` | M17, `docs/reference/gc.md` ([§16](#16-gc-files-gcpass-and-stategccondemned-freeze-item-15)) |

## 16. GC files: `gc/<pass>` and `state/gc/condemned` (freeze item 15)

**`gc/<pass:016x>`** (review 2, finding 7): one sealed, create-only object
per GC pass, written once at the end of the pass (and once by an urgent
pass). Envelope kind 7, AAD name `gc/<pass:016x>`, body = the opaque body of
[§15](#15-opaque-bodies-heat-dicts-gcspace-freeze-item-7) with `ver` = 1 and
`bytes` = the pass's JSON lines, defined by M17.A in `docs/reference/gc.md`.
`pass` is a u64 ≥ 1, one above the highest `gc/` object present (a directory
listing), so no counter needs persisting; an implementation that prunes old
`gc/` objects keeps the highest one. Locally the object lives at
`gc/<pass:016x>`, same bytes, so it uploads verbatim (written tmp,
`fdatasync`, rename, directory `fsync`; a second write of the same pass is
refused).

**`state/gc/condemned`**: local only, never uploaded, not frozen (losing it
is safe: everything is re-marked), replaced atomically. Its envelope is
fixed here so that a version mismatch is detected:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXGCC"` | |
| `ver` | 8 | 2 | u16 | 1 | envelope version |
| `body_ver` | 10 | 2 | u16 | ≥ 1 | defined by `docs/reference/gc.md` |
| `body_len` | 12 | 4 | u32 | ≤ 256 MiB | |
| `body` | 16 | `body_len` | | | the condemned list (M17.A) |
| `crc32c` | 16 + `body_len` | 4 | u32 | `crc32c` of everything before it | |

A reader that finds an unknown version or a bad CRC treats the list as
lost and re-marks. `gc/space/<commit_seq:016x>` stays reserved.

## 17. Index run (freeze item 13)

`index/<pack:016x>` (locally `index/<pack:016x>`): an immutable sorted run
of `chunk_id → (seg, off, clen)` with a block index and a checksum. The id
comes from the volume's `ObjId` counter. Runs are named by FULL commits
(D3).

### 17.1 Layout

```
[block 0] … [block nblocks-1]  [block index]  [covered segments]  [trailer (64 bytes)]
```

Block (exactly 4 096 bytes, at offset `i × 4096`):

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `count` | 0 | 2 | u16 | 1 ..= 85; 85 in every block but the last | |
| reserved | 2 | 2 | | zero | |
| entries | 4 | `48 × 85` | entries below | the first `count` used, the rest zero | |
| reserved | 4084 | 8 | | zero | |
| `crc32c` | 4092 | 4 | u32 | `crc32c(block[0 .. 4092])` | |

Entry (48 bytes): `chunk_id` [32] at 0, `seg` u64 at 32, `off` u32 at 40,
`clen` u32 at 44. Entries are sorted strictly ascending by `chunk_id`
across the whole run: one entry per chunk id (the current copy).

Block index (at `nblocks × 4096`): `nblocks` × 32 bytes, the first
`chunk_id` of each block.

Covered segments (after the block index): `ncovered` × u64, sorted
ascending, unique: the segments whose footer entries the run accounts for
(used by `catch_up`, DESIGN §4.4 step 6).

Trailer:

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `entries` | 0 | 8 | u64 | = Σ block `count` | |
| `run_id` | 8 | 8 | u64 | = the id in the name | |
| `nblocks` | 16 | 4 | u32 | ≥ 1 | |
| `ncovered` | 20 | 4 | u32 | 0 ..= 2^24 | |
| `run_hash` | 24 | 32 | bytes | `BLAKE3-256(run[0 .. trailer offset])` | |
| `ver` | 56 | 1 | u8 | 1 | |
| reserved | 57 | 3 | | zero | |
| `magic` | 60 | 4 | bytes | `"LXIR"` | |

Rule: run length = `nblocks × 4096 + nblocks × 32 + ncovered × 8 + 64`.

### 17.2 Why 4 KiB blocks

An S3-only volume consults the bucket's runs with one ranged GET of one
block per filter hit (DESIGN §13.1); a block is self-checking through its
CRC, without the rest of the run.

### 17.3 Authentication

Index runs are **checksummed, not authenticated or sealed**. They hold only
what the plaintext extent framing already shows (chunk id, offset, `clen`),
and every use is verified: a hint from the index is checked against the
extent header's `chunk_id` and the extent's tag or AEAD before data is
returned (DESIGN §7). A forged or corrupt run can make a read fail, never
return wrong data.

### 17.4 Invariants

- Every entry's `seg` was named by a durable commit when the run was
  written; a `relocate` entry is never persisted before its new segment is
  named by a durable commit (plan M6, review 2 finding 5c).
- **Delete-time index invariant** (review 2, finding 9): a segment that
  stale hints still point into may be deleted only if the index durably
  names the current copy of every live id those hints resolve to, or that
  copy's segment is named by the chain (so `catch_up` restores the entry
  after a crash).
- Maximum: `nblocks` ≤ 2^32 − 1; entries ≤ 85 × `nblocks`.

## 18. Checkpoint (freeze item 14)

`state/checkpoint` is a hint only (DESIGN §4.1): losing it or finding it
stale changes the cost of recovery, never its result. It is 8 192 bytes: two
4 096-byte slots at offsets 0 and 4096. A writer overwrites the slot that
does not hold the newest valid copy (`pwrite` of 4 KiB, `fdatasync`); a
reader takes the valid slot with the higher `slot_seq` and ignores the file
if neither is valid.

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXCKP"` | |
| `ver` | 8 | 2 | u16 | 1 | |
| `lanes` | 10 | 2 | u16 | 1 ..= 256 | |
| `n_stats` | 12 | 2 | u16 | 0 ..= 192 | |
| reserved | 14 | 2 | | zero | |
| `slot_seq` | 16 | 8 | u64 | ≥ 1, rises per write | |
| `volume_uuid` | 24 | 16 | bytes | | |
| `last_commit_seq` | 40 | 8 | u64 | | the tip when written |
| `id_high` | 48 | 8 | u64 | | `ObjId` counter high water (segments, packs, runs) |
| reserved | 56 | 8 | | zero | |
| `lane_tail[]` | 64 | `8 × lanes` | `{ordinal u32, reserved u32}` | | per lane: files with a lower ordinal hold only records ≤ the floor and may be skipped |
| `stats[]` | `64 + 8 × lanes` | `8 × n_stats` | u64 each | | counters; their meaning is defined by `lunix-engine` and is not frozen |
| zero | | | | zero | to 4 092 |
| `crc32c` | 4092 | 4 | u32 | `crc32c(slot[0 .. 4092])` | |

## 19. Device record (freeze item 14)

`state/device` (atomic replace) records the ublk device for the recovery
identity check (DESIGN §3).

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXDEV"` | |
| `ver` | 8 | 2 | u16 | 1 | |
| `nr_queues` | 10 | 2 | u16 | 1 ..= 256 | |
| `queue_depth` | 12 | 2 | u16 | 1 ..= 4 096 | |
| reserved | 14 | 2 | | zero | |
| `dev_id` | 16 | 4 | u32 | | the ublk device id |
| `max_io_buf_bytes` | 20 | 4 | u32 | ≥ 262 144 | D14; checked at recovery with the others |
| `boot_id` | 24 | 16 | bytes | | `/proc/sys/kernel/random/boot_id` as 16 bytes |
| `volume_uuid` | 40 | 16 | bytes | | |
| `dev_sectors` | 56 | 8 | u64 | | size in 512-byte sectors |
| `flags` | 64 | 8 | u64 | | the ublk device flags |
| reserved | 72 | 20 | | zero | |
| `crc32c` | 92 | 4 | u32 | `crc32c(bytes 0..92)` | |

Length: 96 bytes.

## 20. Local pins file (freeze item 14)

`state/pins` (atomic replace) holds the monotonic floors and the local
trust-on-first-use pin (ADR-11 F6, F7).

| field | offset | width | encoding | legal range | notes |
|---|---|---|---|---|---|
| `magic` | 0 | 8 | bytes | `"LUNIXPIN"` | |
| `ver` | 8 | 2 | u16 | 1 | |
| `pin_present` | 10 | 1 | u8 | 0, 1 | |
| `pin_mode` | 11 | 1 | u8 | a `crypto_mode`; 0 if no pin | |
| reserved | 12 | 4 | | zero | |
| `volume_uuid` | 16 | 16 | bytes | | |
| `family_id` | 32 | 16 | bytes | | |
| `max_keyring_seq` | 48 | 8 | u64 | | highest `keyring_seq` seen |
| `max_epoch` | 56 | 8 | u64 | | highest epoch seen |
| `pin_fingerprint` | 64 | 16 | bytes | zero unless a keyed mode is pinned | the KMK fingerprint pin |
| reserved | 80 | 12 | | zero | |
| `crc32c` | 92 | 4 | u32 | `crc32c(bytes 0..92)` | |

Length: 96 bytes.

## 21. `volume.json` and `volumes.json` (freeze item 14)

These are JSON, not byte layouts. Both carry `"version": 1`; unknown keys
are rejected. `volume.json` holds local settings only (never the backend
set, ADR-24); it is replaced atomically.

| key | type | default | range | source |
|---|---|---|---|---|
| `version` | integer | 1 | 1 | |
| `name` | string | — | 1 ..= 64 of `[A-Za-z0-9._-]` | |
| `directory` | string | — | absolute path | |
| `space.free_floor` | integer (bytes) or string `"N%"` | `"10%"` | | DESIGN §2 |
| `space.on_full` | string | `"wait"` | `wait`, `error` | |
| `space.full_timeout_ms` | integer or null | null | ≥ 1 | |
| `space.cache_max` | integer or null | null | bytes | |
| `space.headroom` | integer | 268 435 456 | bytes | plan M8 |
| `commit.interval_ms` | integer | 30 000 | ≥ 1 000 | `T` |
| `commit.dirty_bytes` | integer | 1 073 741 824 | ≥ 1 MiB | `B` |
| `retain_last` | integer | 2 | 1 ..= 1 000 | D15; 1 leaves nothing for `--truncate-tip` |
| `gc_grace_ms` | integer | 60 000 | ≥ 10 000 + 2 × `read_timeout_ms` | M17, review 2 N6 |
| `read_timeout_ms` | integer | 10 000 | ≥ 1 000 | review 2 N6 |
| `gc_rate` | integer | 67 108 864 | bytes per second | M17 |
| `gc_live_threshold` | number | 0.5 | 0 < x < 1 | M17 |
| `gc_step_budget_ms` | integer | 50 | ≥ 1 | M17 |
| `journal_max` | integer | 2 147 483 648 | ≥ 268 435 456 | plan M3 |
| `index_ram_per_chunk` | integer | 16 | bytes | plan M6 |
| `map_dirty_leaves_max` | integer | 20 000 | ≥ 100 | plan M5 |
| `map_cache_max` | integer | 268 435 456 | bytes | plan M5 |
| `read_cache_max` | integer | 134 217 728 | bytes | plan M11 |
| `verify_reads` | boolean | false | | plan M11, D11 |
| `max_io_buf_bytes` | integer | 1 048 576 | ≥ 262 144 | D14 (attach default) |

`docs/reference/gc.md` (M17.A) owns the meaning of the `gc_*` keys and
`retain_last`; this table fixes their names and defaults.

`<state_dir>/volumes.json` (host registry, rebuildable by scanning the
volume directories): `{"version": 1, "volumes": [{"name": …, "uuid": …,
"directory": …}]}`, names unique, `uuid` in the hyphenated form.

## 22. Bucket and local paths (freeze item 12)

Bucket prefix of a volume: `<prefix>/lunix/v1/<volume-uuid>/`; of the
keyring: `<prefix>/lunix/v1/families/<family-id>/keyring`. Local directory:
`<dir>/<volume-uuid>/`; keyring `<dir>/families/<family-id>/keyring`.

Text forms: ids are lowercase hex, zero-padded to the stated width
(`<x:016x>`). `<volume-uuid>` and `<child-uuid>` are the 36-character
hyphenated lowercase form; `<family-id>` is 32 lowercase hex digits. A
backend id is 1 ..= 64 of `[a-z0-9._-]`; `<name>` (snapshot) is 1 ..= 128
of `[A-Za-z0-9._-]`, not starting with `.`; `<host>` is 1 ..= 64 of
`[a-z0-9.-]` and `<id>` is 16 lowercase hex digits.

| bucket-relative name (AAD name) | local path | kind | notes |
|---|---|---|---|
| `volume` | `volume` | — | exact bytes |
| `lease` | — | 2 | phase 2 |
| `head` | — | — | advisory, never trusted |
| `commits/<commit_seq:016x>` | `state/commits/<commit_seq:016x>` | 1, 3 | |
| `segments/<seg:016x>` | `segments/<seg:016x>.seg` (sealed), `segments/<seg:016x>.open` (being written) | footer 6 | |
| `map/<pack:016x>` | `map/<pack:016x>` (sealed), `map/<pack:016x>.open` | footer 6 | |
| `index/<pack:016x>` | `index/<pack:016x>` | — | the local manifest `index/manifest` is local only and not frozen (M6) |
| `heat/<commit_seq:016x>` | `heat/<commit_seq:016x>` | 5 | phase 4 |
| `snapshots/<name>` | `snapshots/<name>` | 10 | |
| `readers/<host>-<id>` | `readers/<host>-<id>` | 8 | |
| `holds/<child-uuid>` | `holds/<child-uuid>` | 9 | |
| `gc/<pass:016x>` | `gc/<pass:016x>` | 7 | |
| `gc/space/<commit_seq:016x>` | `gc/space/<commit_seq:016x>` | 12 | reserved |
| `dicts/<id:08x>` | `dicts/<id:08x>` | 11 | |
| `sync/<backend-id>/progress` | — | — | advisory |
| `families/<family-id>/keyring` (relative to `<prefix>/lunix/v1/`) | `<dir>/families/<family-id>/keyring` | 13 (MAC only) | |
| — | `volume.json`, `LOCK`, `journal/q<lane:02x>-<ordinal:08x>.j`, `cache/`, `state/checkpoint`, `state/device`, `state/pins`, `state/gc/condemned` | — | local only |

Temporary files end in `.tmp` and are deleted at open. The paths module maps
a local path to its bucket-relative name by this table (D9); local-only
paths map to none.

## 23. Worked examples

The layouts below were written by hand from the tables above, not produced
by Lunix code. Hash, CRC and MAC values were computed with the reference
`blake3`, `hmac`/`sha2` crates and a CRC-32C routine checked against
`crc32c("123456789") = e3069283`, the BLAKE3 empty-input hash and RFC 4231
case 1; the same tool reproduces the KAT's `purpose_pin`,
`purpose_volume_mac`, `purpose_meta_commit` and `xchacha_ct`. M1.A decodes
and re-encodes each example byte for byte.

Two example volumes:

| | volume A | volume B |
|---|---|---|
| `volume_uuid` | `a0a1a2a3-a4a5-46a7-88a9-aaabacadaeaf` | `b0b1b2b3-b4b5-46b7-88b9-babbbcbdbebf` |
| `family_id` | `404142434445464748494a4b4c4d4e4f` (KAT) | `505152535455565758595a5b5c5d5e5f` |
| mode | encrypted, default profile | integrity |
| root | KMK = `2021…3e3f` (KAT `kmk`) | IK = `6061…7e7f` |
| chunk size, fanout, `id_len`, policy | 64 KiB, 512, 32, auto (level 1) | 16 KiB, 512, 16, lz4 |
| `created_at` | 1 794 441 600 000 (2026-11-12T00:00:00Z) | same |

Derived keys of volume B (keyed BLAKE3 of `"lunix kdf v1\0" ‖ family ‖
purpose` under IK):

| purpose | value |
|---|---|
| 0x04 descriptor MAC key | `d56ef6bb95bf87c7b14ec9ad25b9eae3132d9120d815b04d6f467207f705d972` |
| 0x05 fingerprint (first 16 bytes) | `92a4ec2689f62aab86bc8f278713183e` |
| 0x08 integrity HMAC key | `aa13505a19bd5e783cbc31582031cb918c1dfaa9aa6b94b27057acb06306f4df` |
| 0x09 extent tag key | `8f5a8aa5bd1c61e6d9b3e275436f35de2673144fc8d12cb7aab4b39398b09322` |

Volume A's keys are KAT vectors: descriptor MAC key = `purpose_volume_mac`,
fingerprint = the first 16 bytes of `purpose_pin`.

### 23.1 Journal file header and a batch with two records (volume B)

Lane 0, file `journal/q00-00000001.j`, `generation` = `0x9e3779b97f4a7c15`.
The guest writes 16 KiB of `0x41` at block 0 (`write_seq` 1), then discards
16 blocks at block 256 (`write_seq` 2); both go into one batch.

File header, offset 0:

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `4c 55 4e 49 58 4a 4e 4c` | `magic` | `"LUNIXJNL"` |
| 8 | `01 00` | `ver` | 1 |
| 10 | `00 00` | `lane` | 0 |
| 12 | `01 00 00 00` | `ordinal` | 1 |
| 16 | `b0 b1 b2 b3 b4 b5 46 b7 88 b9 ba bb bc bd be bf` | `volume_uuid` | volume B |
| 32 | `15 7c 4a 7f b9 79 37 9e` | `generation` | `0x9e3779b97f4a7c15` |
| 40 | 20 × `00` | reserved | |
| 60 | `81 80 9d 89` | `crc32c` | `0x899d8081` over bytes 0..60 |
| 64 | 4 032 × `00` | padding | |

Record 1 (write), file offset 4096 (`0x1000`):

| offset | bytes | field | value |
|---|---|---|---|
| 0x1000 | `4c 58 4a 52` | `magic` | `"LXJR"` |
| 0x1004 | `01` | `ver` | 1 |
| 0x1005 | `01` | `kind` | write |
| 0x1006 | `00 00` | reserved | |
| 0x1008 | `01 00 00 00 00 00 00 00` | `write_seq` | 1 |
| 0x1010 | `00 00 00 00 00 00 00 00` | `epoch` | 0 |
| 0x1018 | `00 00 00 00 00 00 00 00` | `lba` | block 0 |
| 0x1020 | `04 00 00 00` | `len` | 4 blocks (16 384 bytes) |
| 0x1024 | `22 18 f7 cf` | `crc32c` | `0xcff71822` over the header with this field zero, then the data |
| 0x1028 | `15 7c 4a 7f b9 79 37 9e` | `gen` | the file generation |
| 0x1030 | `00 10 00 00 00 00 00 00` | `off` | 4096 |
| 0x1038 | `00 00 00 00 00 00 00 00` | reserved | |
| 0x1040 | 16 384 × `41` | data | |

Record 2 (hole), at `0x1000 + 64 + 16384 = 0x5040` (8-byte aligned):

| offset | bytes | field | value |
|---|---|---|---|
| 0x5040 | `4c 58 4a 52` | `magic` | `"LXJR"` |
| 0x5044 | `01` | `ver` | 1 |
| 0x5045 | `02` | `kind` | hole |
| 0x5046 | `00 00` | reserved | |
| 0x5048 | `02 00 00 00 00 00 00 00` | `write_seq` | 2 |
| 0x5050 | `00 00 00 00 00 00 00 00` | `epoch` | 0 |
| 0x5058 | `00 01 00 00 00 00 00 00` | `lba` | block 256 |
| 0x5060 | `10 00 00 00` | `len` | 16 blocks |
| 0x5064 | `cd e8 65 29` | `crc32c` | `0x2965e8cd` (header only; no data) |
| 0x5068 | `15 7c 4a 7f b9 79 37 9e` | `gen` | |
| 0x5070 | `40 50 00 00 00 00 00 00` | `off` | `0x5040` |
| 0x5078 | `00 00 00 00 00 00 00 00` | reserved | |
| 0x5080 | `00 …` to `0x5fff` | padding | zero to the 4 KiB boundary |

The batch is 16 512 bytes of records, written as one 20 480-byte (5-block)
write at `[0x1000, 0x6000)`. A reader accepts record 1, accepts record 2,
reads a zero magic at `0x5080` (not aligned: end of batch), moves to
`0x6000` and, finding zero there at an aligned offset, ends the lane.

### 23.2 Plain extent (volume B)

Chunk 0 of volume B after destaging record 1: 16 384 bytes of `0x41`,
`chunk_id` = BLAKE3-256 of the chunk =
`c6c4890119d834da404b94c707aa46a579d77f122211d145982c8ceb83e7f566`. Codec 2
(LZ4); the 75-byte LZ4 block is: token `1f` (1 literal, match length
15 + extension), literal `41`, offset `01 00`, 64 × `ff` and `27` (match
length 15 + 64 × 255 + 39 + 4 = 16 378), token `50` (5 final literals),
`41 41 41 41 41`: 1 + 16 378 + 5 = 16 384 bytes.

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `43 43 48 31` | `magic` | `"CCH1"` |
| 4 | `01` | `ver` | 1 |
| 5 | `02 00` | `codec` | 2 (LZ4) |
| 7 | `00` | `level` | 0 |
| 8 | `00 40 00 00 00 00 00 00` | `ulen` | 16 384 = the chunk size |
| 16 | `c6 c4 89 01 19 d8 34 da 40 4b 94 c7 07 aa 46 a5 79 d7 7f 12 22 11 d1 45 98 2c 8c eb 83 e7 f5 66` | `chunk_id` | |
| 48 | `4b 00 00 00` | `clen` | 75 |
| 52 | `1f 41 01 00`, 64 × `ff`, `27 50 41 41 41 41 41` | payload | the LZ4 block above |
| 127 | `5f 22 05 bd 62 15 95 15 ed 21 9a 08 99 87 bb 03` | `tag` | BLAKE3-128 of `keyed BLAKE3(extent tag key, extent[0..127])` |

Length 143 = `OVH` (68) + 75. The same extent on a **plain-mode** volume
has the unkeyed tag `f2 1b 8c 97 3d ce fd 5e 68 95 4f 1c 22 61 58 7a`
(BLAKE3-128 of `extent[0..127]`). The `CCH1` object `extent[0..16] ‖
extent[52..127]` decodes with `chunkfmt` to the 16 KiB chunk.

### 23.3 Encrypted extent (volume A, KAT inputs)

The inputs are those of `kat-vectors.json`: `family` `40..4f`, `chunk_id`
`80..9f`, `dek_epoch` (= `dek_gen`) 7, `salt` `c0..d7`, codec 0 (raw),
level 0, 64-byte payload `00..3f`, nonce zero, `id_len` 32 (volume A),
`kind` data. The KAT feeds the DEK (`00..1f`) directly; in the product it is
purpose 0x02 of the KMK. `ulen` = 64 is below the minimum chunk size: the
example exists so the bytes can be checked against the KAT, and a reader
with volume A's context would reject it on the `ulen = chunk size` rule.

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `43 43 48 31` | `magic` | `"CCH1"` |
| 4 | `01` | `ver` | 1 |
| 5 | `00 00` | `codec` | 0 (raw) |
| 7 | `00` | `level` | 0 |
| 8 | `40 00 00 00 00 00 00 00` | `ulen` | 64 |
| 16 | `80 81 … 9f` (32 bytes) | `chunk_id` | KAT `chunk_id` |
| 48 | `40 00 00 00` | `clen` | 64 |
| 52 | `07 00 00 00` | `dek_gen` | 7 |
| 56 | `c0 c1 … d7` (24 bytes) | `salt` | KAT `salt` |
| 80 | 12 × `00` | `nonce` | zero (default profile) |
| 92 | `8c 1e 4b 19 c5 32 4d 4e c7 f8 e7 35 81 88 47 9a b6 8c ab 77 61 f3 51 45 46 e4 c7 84 21 c7 d8 34 af ff 96 e8 95 64 2b d5 72 35 55 eb 36 5b 31 74 50 b0 38 fe 0a aa 43 17 12 17 90 8c 0d a0 06 76` | payload | ciphertext: the first 64 bytes of KAT `body_hardened` |
| 156 | `d4 c8 c1 dd a5 af 43 e4 79 7f 63 1d d2 03 a6 78` | `tag` | the last 16 bytes of `body_hardened` |

Length 172 = `OVH` (108) + 64. The AAD built from it ([§6.3](#63-aad-key-derivation-info-and-plain-tag-input))
is `01 00 00 40000000 00 07000000 404142…4f 20 808182…9f` = KAT
`aad_hardened`; `info` is KAT `info_v2`; the key is KAT `key_hardened`.

### 23.4 Segment with one extent, footer and trailer (volume B)

`segments/0000000000000001` (epoch 0, counter 1) holds the extent of
§23.2. Total length 380 bytes.

| range | content |
|---|---|
| `[0, 143)` | the extent of §23.2 |
| `[143, 316)` | footer envelope, 173 bytes |
| `[316, 380)` | trailer |

Footer envelope at 143:

| offset (in envelope) | bytes | field | value |
|---|---|---|---|
| 0 | `4c 58 4d 31` | `magic` | `"LXM1"` |
| 4 | `01` | `env_ver` | 1 |
| 5 | `06` | `kind` | footer |
| 6 | `00` | `form` | 0 (integrity mode) |
| 7 | `00` | reserved | |
| 8 | `81 00 00 00` | `body_len` | 129 |
| 12 | `28 b5 2f fd` | zstd magic | |
| 16 | `20` | zstd frame header descriptor | single segment, 1-byte content size, no checksum, no dictionary |
| 17 | `78` | frame content size | 120 |
| 18 | `c1 03 00` | block header | last block, raw, size 120 (`120 << 3 \| 1`) |
| 21 | 120 bytes | footer body | below |
| 141 | `d9 45 f9 bf 84 b0 99 59 46 22 21 ec 6e 2c b1 54 f5 1d 6a d0 fa ce 0e 97 87 8d db 19 75 ee 59 7f` | `mac` | HMAC-SHA-256(integrity HMAC key, `meta_aad(B, 6, "segments/0000000000000001")` ‖ envelope[0..141]) |

Footer body (the 120 bytes at envelope offset 21):

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `01` | `ver` | 1 |
| 1 | `00` | `container_kind` | segment |
| 2 | `00 00` | reserved | |
| 4 | `01 00 00 00` | `extent_count` | 1 |
| 8 | `01 00 00 00 00 00 00 00` | `object_id` | 1 |
| 16 | `8f 00 00 00 00 00 00 00` | `extents_len` | 143 |
| 24 | `01 00 00 00 00 00 00 00` | `write_seq_lo` | 1 |
| 32 | `01 00 00 00 00 00 00 00` | `write_seq_hi` | 1 |
| 40 | `03 92 36 2e cd c4 5e e0 63 46 46 a9 45 1b 18 7b 44 56 25 8c 25 a1 5c 7d 17 2e 79 99 c7 df 06 66` | `seg_hash` | BLAKE3-256 of bytes `[0, 143)` |
| 72 | `c6 c4 … f5 66` (32 bytes) | entry `chunk_id` | |
| 104 | `00 00 00 00` | entry `off` | 0 |
| 108 | `4b 00 00 00` | entry `clen` | 75 |
| 112 | `00 40 00 00` | entry `ulen` | 16 384 |
| 116 | `02` | entry `codec` | 2 |
| 117 | `00 00 00` | entry reserved | |

Trailer at 316:

| offset (in trailer) | bytes | field | value |
|---|---|---|---|
| 0 | `8f 00 00 00 00 00 00 00` | `footer_off` | 143 |
| 8 | `ad 00 00 00` | `footer_len` | 173 |
| 12 | `01 00 00 00` | `extent_count` | 1 |
| 16 | `01 00 00 00 00 00 00 00` | `object_id` | 1 |
| 24 | `01` | `ver` | 1 |
| 25 | `00` | `container_kind` | segment |
| 26 | 30 × `00` | reserved | |
| 56 | `59 ed 78 e9` | `crc32c` | `0xe978ed59` over trailer bytes 0..56 |
| 60 | `4c 58 53 54` | `magic` | `"LXST"` |

Check: 143 + 173 + 64 = 380.

### 23.5 A leaf node (volume B)

Leaf 0 of volume B after the cut at `write_seq` 2: entry 0 is chunk 0 →
segment 1, offset 0, `clen` 75; entries 1-511 are holes (record 2's hole
lies in leaf 0 too). Two runs: `{start 0, seg_idx 1, rank 0}` and `{start 1,
hole, rank 1}`. Widths: `wseg` = bits(1) = 1, `wsd` = 0, `woff` = 0, `wc` =
bits(75) = 7, `wcum` = 0; `rw` = 9 + 1 + 0 + 10 = 20 bits.

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `00` | `level` | 0 (leaf) |
| 1 | `00 02` | `fanout` | 512 |
| 3 | `02 00` | `nruns` | 2 |
| 5 | `01 00` | `ndata` | 1 |
| 7 | `01 00` | `ndict` | 1 |
| 9 | `01` | `wseg` | 1 |
| 10 | `00` | `wsd` | 0 |
| 11 | `00` | `woff` | 0 |
| 12 | `07` | `wc` | 7 |
| 13 | `00` | `wcum` | 0 |
| 14 | `01 00 00 00 00 00 00 00` | `base` | segment 1 |
| 22 | (none) | seg dictionary | 1 × 0 bits |
| 22 | `00` | lineage column | `lineage_idx` 0 |
| 23 | `00 02 10 40 00` | run table | run 0 = `0x00200` (start 0, seg_idx 1, rank 0), run 1 = `0x00401` (start 1, seg_idx 0, rank 1), packed LSB first: `0x0040100200` |
| 28 | `4b` | `clen` column | 75 |
| 29 | (none) | `cum` column | 1 × 0 bits |
| 29 | `c6 c4 89 01 19 d8 34 da 40 4b 94 c7 07 aa 46 a5` | id column | `id_prefix` (16 bytes) |

Length 45. Leaf hash (BLAKE3-256, integrity mode) =
`b2b3b7ec14023024c44cad3ad2595e908b40ebb7fb91d667ddd8153909114469`. Lookup
of entry 0: run 0, `k` = 0, `seg` = 1 + 0, `off` = 0, `clen` = 75, prefix
`c6c4…46a5`; entry 7: run 1, a hole.

The leaf is stored in map pack 2 at offset 0 as a codec-0 extent of 113
bytes (`ulen` = `clen` = 45, `chunk_id` = the leaf hash, keyed tag
`2f 16 49 5d 64 b5 48 2a 66 1e c5 c1 f6 aa 7d 67`). The root (level 1) is
stored after it at offset 113: header `01 00 02`, child 0 = `{leaf hash,
pack 2, off 0, len 113}`, children 1-511 = `{E0, 0, 0, 0}`; 24 579 bytes,
extent length 24 647. Root hash = BLAKE3-256(`01 00 02` ‖ leaf hash ‖ `E0` ×
511) = `69a6249dad0e6c194b6400c39e38e8f97a0c39cb37b005bda3a9b791f516e6ca`.

### 23.6 FULL commits: genesis and commit 1 (volume B)

Volume B: 1 GiB (`size` = 2^30), 65 536 chunks, `levels` = max(2,
ceil(16 / 9)) = 2. Envelope form 0; the AAD name of commit *n* is
`commits/<n:016x>` (24 bytes).

Genesis, `commits/0000000000000000`, 212 bytes:

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `4c 58 4d 31` | `magic` | `"LXM1"` |
| 4 | `01` | `env_ver` | 1 |
| 5 | `01` | `kind` | commit |
| 6 | `00` | `form` | 0 |
| 7 | `00` | reserved | |
| 8 | `a8 00 00 00` | `body_len` | 168 |
| 12 | `01` | `body_ver` | 1 |
| 13 | `01` | `commit_kind` | FULL |
| 14 | `00 00` | `flags` | no parent, no heat |
| 16 | `00 00 00 00` | reserved | |
| 20 | `00 00 00 00 00 00 00 00` | `commit_seq` | 0 |
| 28 | `00 00 00 00 00 00 00 00` | `epoch` | 0 |
| 36 | `00 00 00 00 00 00 00 00` | `parent_seq` | none (0) |
| 44 | `6a 6d 95 2b bf cf 69 09 41 f1 c5 65 9a 31 2f d7 8d 0f af 46 c8 2e cb fb db 18 59 ae 9e 51 b8 f5` | `parent_hash` | BLAKE3-256 of the 120 bytes of descriptor B (§23.8) |
| 76 | `00 dc 0d cd a1 01 00 00` | `created_at` | 1 794 441 600 000 |
| 84 | `00 00 00 00 00 00 00 00` | `cut_seq` | 0 |
| 92 | `00 00 00 40 00 00 00 00` | `size` | 1 073 741 824 |
| 100 | `00 00 00 00 00 00 00 00` | `chain_floor` | 0 |
| 108 | `10 c1 52 fb fa aa b0 63 0d b8 b7 86 ee bf 55 9c bd 7b 38 cb 96 4a 20 46 5d 68 e8 44 12 d6 13 17` | `map_root.hash` | `E1` of fanout 512 (§23.7) |
| 140 | `00 00 00 00 00 00 00 00` | `map_root.pack` | 0 (empty map) |
| 148 | `00 00 00 00` | `map_root.off` | 0 |
| 152 | `00 00 00 00` | `map_root.len` | 0 |
| 156 | `00 00 00 00 00 00 00 00` | `heat_seq` | 0 |
| 164 | `00 00 00 00` | `n_map_packs` | 0 |
| 168 | `00 00 00 00` | `n_new_segments` | 0 |
| 172 | `00 00 00 00` | `n_index_packs` | 0 |
| 176 | `00 00 00 00` | reserved | |
| 180 | `70 77 6d 1e 89 91 ac c3 5d e8 17 63 bb 93 6d df 0f c5 d4 83 fb 50 c5 7c 90 6f 86 aa 53 5c 51 6e` | `mac` | HMAC-SHA-256(integrity HMAC key, `meta_aad(B, 1, "commits/0000000000000000")` ‖ bytes 0..180) |

Commit 1, `commits/0000000000000001`, 228 bytes: the cut at `write_seq` 2
of §23.1, naming segment 1 (§23.4) and map pack 2 (§23.5). No index run
yet: the run for segment 1 is written at C8 of this commit and named by
FULL 2 (D3).

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `4c 58 4d 31 01 01 00 00` | envelope | `"LXM1"`, ver 1, kind commit, form 0 |
| 8 | `b8 00 00 00` | `body_len` | 184 |
| 12 | `01 01` | `body_ver`, `commit_kind` | 1, FULL |
| 14 | `01 00` | `flags` | `has_parent` |
| 16 | `00 00 00 00` | reserved | |
| 20 | `01 00 00 00 00 00 00 00` | `commit_seq` | 1 |
| 28 | `00 00 00 00 00 00 00 00` | `epoch` | 0 |
| 36 | `00 00 00 00 00 00 00 00` | `parent_seq` | 0 |
| 44 | `33 8e 71 76 84 fb e1 6a 5d 22 c7 61 8a 3d 89 3a 25 d6 80 86 89 df 89 3e b6 af 53 d9 42 bb 03 c2` | `parent_hash` | BLAKE3-256 of the 212 genesis bytes |
| 76 | `30 51 0e cd a1 01 00 00` | `created_at` | 1 794 441 630 000 |
| 84 | `02 00 00 00 00 00 00 00` | `cut_seq` | 2 |
| 92 | `00 00 00 40 00 00 00 00` | `size` | 1 GiB |
| 100 | `00 00 00 00 00 00 00 00` | `chain_floor` | 0 (retained FULLs: 0 and 1) |
| 108 | `69 a6 24 9d ad 0e 6c 19 4b 64 00 c3 9e 38 e8 f9 7a 0c 39 cb 37 b0 05 bd a3 a9 b7 91 f5 16 e6 ca` | `map_root.hash` | the root of §23.5 |
| 140 | `02 00 00 00 00 00 00 00` | `map_root.pack` | 2 |
| 148 | `71 00 00 00` | `map_root.off` | 113 |
| 152 | `47 60 00 00` | `map_root.len` | 24 647 |
| 156 | `00 00 00 00 00 00 00 00` | `heat_seq` | 0 |
| 164 | `01 00 00 00` | `n_map_packs` | 1 |
| 168 | `01 00 00 00` | `n_new_segments` | 1 |
| 172 | `00 00 00 00` | `n_index_packs` | 0 |
| 176 | `00 00 00 00` | reserved | |
| 180 | `02 00 00 00 00 00 00 00` | `map_packs[0]` | 2 |
| 188 | `01 00 00 00 00 00 00 00` | `new_segments[0]` | 1 |
| 196 | `4e 3d c3 b9 db 3e d5 ed ee 1f 2b f7 41 4b d1 b5 b7 0e b0 8b ec 33 b9 f5 d5 3e 00 37 7e 84 ca 00` | `mac` | HMAC-SHA-256(integrity HMAC key, `meta_aad(B, 1, "commits/0000000000000001")` ‖ bytes 0..196) |

### 23.7 Empty leaf and empty hashes

Empty leaf (25 bytes): fanout 512 =
`00 00 02 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00`;
fanout 256 = the same with bytes 1-2 = `00 01`.

Unkeyed empty hashes (plain and integrity mode; keyed ones depend on the
family and are not constants):

| level | fanout 512 | fanout 256 |
|---|---|---|
| E0 | `8cb313b371458a0bbc13829da36f70d69deb2652ae2c1b0f6318004fd1419d5e` | `b66d86b2736d18b9419f57f2dd03958a9cbe6a6057c0803b5fbd43cd1e04d91a` |
| E1 | `10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317` | `bb20922a988899f0a8f2929846bc35dee6dd0210ccf6b6157237b80e17abc8e3` |
| E2 | `e76498965d1098049475b7c10a1fbce7f7e15aab71d961f209a77873b7b3e3e8` | `be662761db8a4a4ee24f872b9d38e2d46158c70a75c55874882da98377a05a9b` |
| E3 | `23cd677335c394fd2a4ebd98a7cf7468ef67034fbda6ea526908df597c187110` | `51d932c23141d31f40276976932bb281a64d2669f6743084f48628922c53b772` |
| E4 | `a0a104e5eceb69a6e727686306eb645079756ac921fb419d5a9ea12e3bef7370` | `47f726654ae37276df5bc4c45df72e3d44022357b0d81325c70c0bd1bde11d0c` |

The root of an empty volume is `E_{levels-1}` with `levels` from §8.2:
E1 for volume B (1 GiB at 16 KiB, 2 levels); E2 for 8 TiB at 64 KiB chunks
and fanout 512 (3 levels).

### 23.8 `volume` descriptors

Volume A (encrypted), 120 bytes:

| offset | bytes | field | value |
|---|---|---|---|
| 0 | `4c 55 4e 49 58 56 4f 4c` | `magic` | `"LUNIXVOL"` |
| 8 | `01 00` | `format_ver` | 1 |
| 10 | `02` | `crypto_mode` | encrypted |
| 11 | `00` | `profile` | default |
| 12 | `00 10 00 00` | `block_size` | 4096 |
| 16 | `00 00 01 00` | `chunk_size` | 65 536 |
| 20 | `00 02` | `map_fanout` | 512 |
| 22 | `20` | `id_len` | 32 |
| 23 | `01` | `codec_policy` | auto |
| 24 | `01` | `codec_level` | 1 |
| 25 | `00` | `lineage_count` | 0 |
| 26 | 6 × `00` | reserved | |
| 32 | `a0 a1 a2 a3 a4 a5 46 a7 88 a9 aa ab ac ad ae af` | `volume_uuid` | |
| 48 | `40 41 42 43 44 45 46 47 48 49 4a 4b 4c 4d 4e 4f` | `family_id` | KAT `family` |
| 64 | `16 e7 4f 8a f7 9c 8f d7 56 04 35 a8 95 c2 6a 72` | `kmk_fingerprint` | first 16 bytes of KAT `purpose_pin` |
| 80 | `00 dc 0d cd a1 01 00 00` | `created_at` | 1 794 441 600 000 |
| 88 | `55 92 15 93 48 20 45 80 5e 11 56 1c a0 a5 eb 62 70 77 f1 83 a6 1f 8f 69 19 cb 57 11 87 80 9b f4` | `mac` | HMAC-SHA-256(KAT `purpose_volume_mac`, `"lunix volume v1\0"` ‖ bytes 0..88) |

Volume B (integrity), 120 bytes; it differs from A in these fields:

| offset | bytes | field | value |
|---|---|---|---|
| 10 | `01` | `crypto_mode` | integrity |
| 16 | `00 40 00 00` | `chunk_size` | 16 384 |
| 22 | `10` | `id_len` | 16 |
| 23 | `02` | `codec_policy` | lz4 |
| 24 | `00` | `codec_level` | 0 |
| 32 | `b0 b1 b2 b3 b4 b5 46 b7 88 b9 ba bb bc bd be bf` | `volume_uuid` | |
| 48 | `50 51 52 53 54 55 56 57 58 59 5a 5b 5c 5d 5e 5f` | `family_id` | |
| 64 | `92 a4 ec 26 89 f6 2a ab 86 bc 8f 27 87 13 18 3e` | `kmk_fingerprint` | the IK's fingerprint |
| 88 | `49 51 c7 ff d2 37 69 3e 73 cd 15 e1 7c 69 16 86 9a 54 0c 09 8b c6 fd be 90 34 0c eb a8 08 01 b5` | `mac` | HMAC-SHA-256(B's descriptor MAC key, `"lunix volume v1\0"` ‖ bytes 0..88) |

Full bytes of B:
`4c554e4958564f4c 0100 01 00 00100000 00400000 0002 10 02 00 00 000000000000`
`b0b1b2b3b4b546b788b9babbbcbdbebf 505152535455565758595a5b5c5d5e5f`
`92a4ec2689f62aab86bc8f278713183e 00dc0dcda1010000`
`4951c7ffd237693e73cd15e17c6916869a540c098bc6fdbe90340ceba80801b5`.

## 24. Decided in A0

Each item is a layout choice the spec left open, with the reason. The
maintainer's sign-off covers them all.

**Global**

1. **Little endian, explicit offsets, no implicit padding**; magics as byte
   strings (8 bytes for files, 4 for records, envelopes, trailers). Reason:
   one rule for every decoder; matches `CCH1`, the KAT and the spikes.
2. **Times are `u64` milliseconds since the Unix epoch (UTC).** Reason:
   enough precision for leases and `--time`; no time-zone or leap rules.
3. **Strings are length-prefixed UTF-8 with stated maxima**, identifiers
   restricted to small character sets ([§22](#22-bucket-and-local-paths-freeze-item-12)).
   Reason: bounded decode; safe in object names and paths.
4. **Canonical encodings are enforced by decoders** (sorted unique lists,
   minimal widths, maximal runs, zero padding). Reason: hashes and MACs over
   encodings must be a function of the content (identical roots, ADR-8),
   and golden round trips need `encode(decode(b)) == b`.

**Crypto**

5. **Three crypto modes** — `plain`, `integrity`, `encrypted` — in one
   descriptor byte. Reason: D11 and ADR-11 require both "keyed with the
   integrity key" and "unkeyed otherwise"; the reader must know which from
   the authenticated descriptor, not guess from the keyring.
6. **`fips` only with `encrypted`**, and the FIPS metadata form (envelope
   form 2) reserved. Reason: the FIPS profile exists for encryption
   (ADR-11); BLAKE3 tags and ids are not FIPS-approved, and the AES-GCM
   metadata form is "not built" (ADR-11).
7. **One MAC primitive, HMAC-SHA-256**, for every metadata MAC (descriptor,
   keyring, envelope form 0), and an unkeyed BLAKE3-256 checksum in its
   place in plain mode. Reason: the spec says HMAC for commits and the
   lease and M2 names HMAC-SHA-256; one primitive for all metadata is
   simpler to audit, FIPS-approved, and its cost is irrelevant at metadata
   rates. The extent tag stays BLAKE3-128 (D11).
8. **Purpose ids 0x06-0x09** (map-node addressing, keyring MAC, integrity
   HMAC, extent tag); 0x01-0x05 as in the KAT. The KMK fingerprint is the
   first 16 bytes of purpose 0x05 (the spike's rule). Reason: the KAT fixes
   0x01-0x05 and meta kinds 1 and 2; the rest needed ids.
9. **Object kind ids 3-13**, with 4 retired (the spike used it for map
   packs, which now use the extent scheme) and 13 `keyring` used only in
   the keyring MAC input. Reason: the KAT fixes 1 and 2; retiring 4 keeps
   the spike's 3 (`delta`) and 5 (`heat`).
10. **The family root of an integrity family is IK, and all its keys are
    derived from it by purpose**, like the KMK's. Its fingerprint fills
    `kmk_fingerprint`. Reason: one derivation for both keyed modes; the
    attach check and the pin work the same way.
11. **Metadata AAD uses a u32 name length** before the name (as the P0.8
    spike). Reason: continuity with `spikes/crypto-kat/src/meta.rs`, which
    M2 starts from.
12. **Envelope form 0 MACs `meta_aad ‖ envelope prefix ‖ body`.** Reason:
    the MAC then binds the volume, kind and bucket-relative name exactly as
    the AEAD does in form 1, so moving an unencrypted commit to another name
    fails too.
13. **Index runs are checksummed but neither sealed nor MAC'd.** Reason:
    they repeat what the plaintext extent framing shows; every index hint is
    verified against the extent before data is returned, so a bad run
    costs availability, never integrity; and an S3-only volume needs
    single-block ranged reads (DESIGN §13.1). The sealed-object table of
    ADR-11 does not list them.

**Ids and paths**

14. **One `ObjId` counter per volume for segments, map packs and index
    runs**, counter from 1, 0 = none. Reason: never-reused ids and a single
    high-water mark (orphan sweep, review 2 finding 5) with no per-kind
    bookkeeping; `seg = epoch << 40 | counter` already fixes the shape.
15. **Text forms**: hyphenated uuid, 32-hex family id, `q<lane:02x>` lane
    names, `.seg`/`.open` for segments (DESIGN §4), bare ids for map packs
    and runs, `index/manifest` for M6's local manifest. Reason: DESIGN gives
    the shapes but not the widths.
16. **`gc/` pass ids come from a directory listing** (one above the
    highest). Reason: create-only objects (finding 7) need unique names, and
    the condemned list may be lost.

**Journal**

17. **The file header fills block 0; records start at 4096.** Reason:
    `O_DIRECT` writes are 4 KiB aligned and the header is written once.
18. **`lba` and `len` in 4096-byte blocks.** Reason: the block size is
    fixed; a hole of up to 16 TiB fits one record.
19. **Records carry `gen` (container generation) and `off` (own
    position)**, in the 64 bytes D12 gives the header. Reason: defence in
    depth against stale or misplaced records and guest data that imitates a
    header ([§5.3](#53-batch-packing-and-how-a-reader-finds-records)); the
    same fields serve the memfd sink and DELTA framing.
20. **Batch boundaries are found by the 4 KiB grid plus the zero-magic
    rule; no batch header.** A zero magic at a non-aligned offset ends the
    batch, at an aligned offset it ends the lane. Reason: D12's cost formula
    allows no extra bytes; §5.3 shows that wherever a torn write can make
    the reader look, it either ends the lane, finds a record after a gap, or
    rejects the bytes on `gen`, `off` and the CRC.
21. **`generation` is random per file incarnation, and a reused file is
    zeroed before its new header is written.** Reason: makes "a file with a
    valid header is zero beyond its last write" true, which the torn-batch
    argument needs.

**Extent and container**

22. **`clen` is the payload length without the tag**, and data extents
    have `ulen` = the chunk size exactly. Reason: matches `info_v2`
    (`clen` = 64 for a 64-byte ciphertext) and the leaf's offset arithmetic;
    one chunk size per volume.
23. **The AAD's `id_len` is the descriptor's `id_len`, and the AAD's
    `chunk_id` is always 32 bytes.** Reason: ADR-11 says `id_len` comes
    from the descriptor; M2's gate moves an extent "to another `id_len`".
24. **A default-profile extent must carry an all-zero `nonce`.** Reason:
    otherwise flipping a nonce byte would not fail `open` (M2 tamper
    matrix).
25. **The footer is a metadata envelope (kind 6) around a zstd frame**,
    in both modes; the trailer has a CRC and a magic. Reason: one MAC/AEAD
    path for all metadata; integrity-mode footers are then authenticated
    too.
26. **`seg_hash` covers the extent region; extents tile the region; the
    footer entry is 48 bytes** (3 reserved). Reason: DESIGN names a
    "seg-level BLAKE3" without its range; tiling makes every byte
    accounted for; 48 bytes keeps entries aligned.
27. **`write_seq_lo/hi` record the journal records that contributed data**,
    0 when none. Reason: DESIGN names a "seq range" without its meaning.
28. **Container length ≤ 2^32 − 1.** Reason: `Hint.off` is a u32.

**Map**

29. **Node header = `level` u8 ‖ `fanout` u16 (3 bytes).** Reason: D1 allows
    only those two fields; fanout 512 does not fit a byte.
30. **The leaf body is the spike's `rb16` plus a byte-aligned lineage
    column after the dictionary, and its offset arithmetic uses `OVH`**
    (68 or 108). Reason: ADR-25 costs "one byte per dictionary entry"; the
    spike modelled `HDR` = 64, the real extent overheads are 68 and 108.
31. **Per-level empty hashes**; empty nodes are never stored and are
    referenced with a zero location. Reason: keeps the tree canonical
    (identical content, identical roots) and sparse volumes free; matches
    the spike.
32. **The interior hash input includes the 3-byte header**; the leaf hash
    covers the whole leaf. Reason: the level byte separates leaf and
    interior inputs (the spike used separate prefixes); fanout is constant
    per volume.
33. **The FULL commit carries the root's location** (`map_root.pack/off/len`),
    and `levels` is derived from `size`. Reason: the root has no parent
    entry; deriving depth avoids a redundant field.

**Commits**

34. **The envelope kind is the object kind (1 or 3); the commit kind is in
    the body.** Reason: ADR-11 seals DELTAs under the `delta` key; the
    reader needs the key before it can read the body.
35. **Every commit kind carries `created_at`.** Reason: `mount-at --time`
    and retention order by time over every retained commit.
36. **`map_packs[]` and `new_segments[]` list what the commit introduced;
    `index_packs[]` lists every run of the manifest durable before C6.**
    Reason: the D3 lag rule, and an S3-only reader needs the full set of
    runs (DESIGN §13.1).
37. **DELTA carries `prev_cut_seq`, `cut_seq`, `size` and a records codec.**
    Reason: replay must check that it covers `(last cut, cut]` without a
    gap, and a resize between FULLs must replay.
38. **CONFIG `policy` is a versioned opaque body** (`policy_ver`,
    `policy_len`). Reason: the spec names `policy` without its content,
    which phase 2/5 defines.
39. **Maximum counts** as listed per section. Reason: bounded decode;
    the limits are far above phase 1 use (a FULL may name 4 TiB of new
    16 MiB segments).

**Key objects and local files**

40. **The keyring carries `root_kind`, the current `dek_gen` and
    `next_slot_id`.** Reason: the reader must know what a slot wraps; DEK
    rotation needs an authenticated current generation; slot ids must never
    be reused.
41. **The device record adds `max_io_buf_bytes` and `volume_uuid`; the pins
    file adds `pin_mode` and `family_id`.** Reason: a recovered device must
    keep its buffer size (D14); a pin must record whether the volume was
    keyed, or a plain volume could replace a keyed one under the CLI.
42. **The checkpoint is two 4 KiB slots with `slot_seq`, lane tails as file
    ordinals, and engine-defined counters.** Reason: DESIGN asks for a
    double-slot layout; floor-based recovery needs only "which files may be
    skipped".
43. **`state/gc/condemned` gets a minimal versioned envelope**; its body is
    `gc.md`'s. Reason: the file is not frozen, but a version mismatch must be
    detected rather than misread.

**Attach**

44. **The attach request declares its credential class** (key material,
    integrity credential, or none), and two results are added to M2's list:
    `NeedCredential` (a keyed descriptor attached without a credential) and
    `ModeMismatch` (an integrity credential on an encrypted descriptor).
    Reason: ADR-11 F7 distinguishes key material from "the integrity
    credential of an unencrypted volume", which the daemon cannot tell
    apart from the credential alone. The check order is in
    [attach-matrix.md](attach-matrix.md).

### 24.1 Differences from the spec and plan text

| where | text | resolution here |
|---|---|---|
| plan in this worktree (D11, M2 step 2, freeze item 3) | tag over `CCH1 header ‖ chunk_id ‖ payload` | every byte before the tag, including `clen` (review 2 finding 1, the main checkout's revised D11 and the A0 brief) |
| plan in this worktree (freeze item 15, M7 step 7) | `gc/<n:016x>` "evidence journal", "appended per line" | one sealed create-only object per pass, `gc/<pass:016x>` (finding 7) |
| plan in this worktree (D5, freeze item 1) | `seq`, type `Seq` | `commit_seq`, `write_seq`, types `CommitSeq`, `WriteSeq` (finding 3) |
| D9, D10 (both versions) | `commits/<seq:016x>`, `{seq, …}` | `commit_seq` (DESIGN terminology block); D-rows copied verbatim |
| DESIGN §4.4 step 2 (this worktree) | `fsck` verifies "the whole chain from genesis" | from the tip down to `chain_floor` (D5, finding 2) |
| ADR-11 F7 | a descriptor MAC failure under supplied key material is "refused as a downgrade" | reported as `BadMac`, as M2's error list does; `Downgrade` is kept for a mode mismatch |
| ADR-11 sealed-object table | no row for index runs | checksummed, not sealed (item 13 above) |
| spec field lists (DESIGN §6.1, §8.2, plan freeze items 2, 6, 9, 14) | lists of fields | fields added here: FULL `map_root` location, `created_at` on every kind, DELTA `prev_cut_seq`/`cut_seq`/`size`/records codec, keyring `root_kind`/`dek_gen`/`next_slot_id`, record `gen`/`off`, device `max_io_buf_bytes`/`volume_uuid`, pins `pin_mode`/`family_id`, header CRCs. The plan says M1 may not change a spec field list; these additions are A0's and need the maintainer's sign-off. |

## 25. Open questions for the maintainer

1. **Default crypto mode of `lunix volume create` without `--encrypt`.**
   The format supports `integrity` (an integrity key wrapped in a slot, so
   every attach must supply the credential, and commits, footers and tags
   are keyed) and `plain` (no keyring: checksums only, corruption
   detection). ADR-11 chose the integrity key over corruption detection
   only, while D11 also allows "unkeyed otherwise". Which is the default,
   and is `plain` offered at all? This changes the CLI (M13) and M2's
   keyring creation, not any byte layout.

The sign-off also confirms review 2's N12 (the tag covers every byte before
it), which D11 now states.
