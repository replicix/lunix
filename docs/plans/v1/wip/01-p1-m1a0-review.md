# Review of M1.A0: frozen byte layouts, attach matrix, FREEZE.md

Reviewer: independent, 2026-11-12. Reviewed: branch `p1/m1-a0` at `bb0fb33`
(`docs/reference/on-disk-formats.md`, `docs/reference/attach-matrix.md`,
`crates/lunix-format/FREEZE.md`) against DESIGN.md, DECISIONS.md, GLOSSARY.md,
ROADMAP.md, plan 01 (M1.A0, M1.A, M2, M3), review 2, `kat-vectors.json`,
`bench/crypto-kat/RESULTS.md`, `bench/radixmap/RESULTS.md`, the leaf-encoding
note, and replicix-core `chunkfmt` (`format.rs`, `codec.rs`). Hashes, MACs,
CRCs and the AEAD of the worked examples were recomputed on the remote test
host (Rust `blake3` 1.8.7, `hmac` 0.12.1, `sha2` 0.10.9, a CRC-32C routine
checked against `e3069283`; Python `cryptography` for AES-GCM, `lz4` and
`zstandard` for the payload framings). Nothing was built or run locally.

## Verdict: **ready with fixes**

No blocker. Every freeze item has a table, every field has an offset or a
variable-length rule and a range, the attach matrix covers all 162
combinations (72 + 36 + 3 merged rows × 18), and every cryptographic value in
§23 reproduces byte for byte. Six majors are text or reader-rule fixes plus one
freeze-time reservation (finding 5); none changes a value already verified
except the optional reservations (5, 9, 10), which would move offsets in §23.5
and §23.4 and need the examples recomputed.

## Findings

Severity: **B** blocker, **M** major (fix before sign-off), **m** minor.

### A. Gate items (check 1)

1. **M — The decisions table is not verbatim.** §2 D9 reads
   `commits/<seq:016x>`; the plan (worktree and main checkout) reads
   `commits/<commit_seq:016x>`. The sentence after the table ("D9 and D10
   still write `seq`") is therefore wrong for D9. Fix: copy D9 from the plan;
   say only D10 still writes `seq`. Gate criterion, trivial.
2. **m — §24.1 omits two A0 additions to the descriptor field list:**
   `codec_level` and `lineage_count` are not in DESIGN §6.1's list; add them
   to the last row so the sign-off covers them.

### B. Internal consistency (check 2)

3. **m — Envelope form 1 does not authenticate its own header.** Form 0's MAC
   covers `envelope[0..12]`; form 1's AAD is `meta_aad` only (as ADR-11 and the
   spike), so `env_ver`, `form`, reserved and `body_len` are checked, not
   authenticated. Safe today (every one is compared against the expected
   value and the AEAD fixes the length), but §9 should say so explicitly:
   "form 1: bytes 0-11 are not in the AAD; a reader rejects any value other
   than `LXM1`, 1, the kind of the name, the form of the mode, 0, and
   `len − 52`". No byte change.
4. **m — Attach matrix, rows 74 and 80.** A pin that records `encrypted` on an
   `integrity` descriptor matches the file's own definition of `Downgrade`
   ("the pin expects more protection than the descriptor has") but is
   reported `WrongKmk` by rule 6. Both refuse, so pick one and state it: either
   add "pin mode stronger than the descriptor's → `Downgrade`" as rule 2b, or
   note under rule 6 that a mode mismatch of any direction is `WrongKmk`.
5. **m — `lease.host_len` 1..=255 against §22's `<host>` 1..=64.** Harmonise
   (64 is enough; the reader pin name uses the same host).

### C. Spec fidelity (check 3)

All spec field lists are present (FULL, DELTA, EPOCH, CONFIG, TRANSFER, lease,
the three pins, `volume`, keyring, journal header and record, footer and
trailer, checkpoint, device, pins, index run). The A0 additions are each sound
and necessary: the FULL root location (the root has no parent entry), DELTA
`prev_cut_seq`/`cut_seq`/`size`/codec (gap detection and resize replay), the
keyring's `root_kind`/`dek_gen`/`next_slot_id` (a reader must know what a slot
wraps, rotation needs an authenticated current generation, slot ids must not
recur), record `gen`/`off` (what makes the torn-batch argument hold, finding
7), device `max_io_buf_bytes`/`volume_uuid` (D14), pins `pin_mode`/`family_id`
(the matrix needs the mode), header CRCs. Two problems:

6. **M — `created_at ≥ the parent's` is written as a legal range (§10.1) and
   a chain rule (§10.7).** A reader cannot enforce it without the parent, and a
   verifier that does would refuse a legitimate chain after an ownership change
   to a host whose clock is behind (nothing clamps today). Fix: make it a
   writer rule, `created_at = max(now, parent.created_at)`; mount and fsck
   never reject on it, fsck warns. Keep `epoch` and `chain_floor` as reader
   rules (they are authenticated monotone state, not clocks).
7. *(none needed)* The three crypto modes are a faithful encoding of the
   spec's ambiguity (ADR-11 "an unencrypted family has a keyring" and D11
   "unkeyed otherwise"); see the open question.

### D. Crash safety and attack resistance (check 4)

8. **M — Journal: a CRC failure in a record at or below the floor hides later
   flushed records (§5.3 note).** The torn-batch argument itself is sound: a
   file with a valid header is zero beyond its last write; a reader reads a
   header only at 4096, after an accepted record, or at the 4 KiB boundary
   after a non-aligned zero; `gen` (random, never guest-visible) ‖ `off` ‖ CRC
   makes guest data, stale records and relocated records unacceptable; and the
   global contiguity rule, not file position, decides what is kept. That last
   point is also why the lane can safely **resynchronise** without a format
   change. Proposed rule (an amendment of D12's last clause, reader side only,
   for the lead to put to the maintainer): on a header or CRC failure at `p`,
   the reader does not end the lane; it scans forward from `p + 8` at 8-byte
   steps to the end of the file for the next header that passes magic, `ver`,
   `gen` = file generation, `off` = position, CRC and `write_seq >` last
   accepted, accepts it and continues; END OF LANE only when no such header
   exists. The merge then does the work: a damaged record above the floor is a
   missing `write_seq` and stops recovery exactly where D12 stops it today; a
   damaged record at or below the floor costs nothing. Forging a header that
   passes needs the 64-bit `gen`; random damage passing `gen`, `off` and CRC
   is ~2^-160. Cost: reading the rest of a 64 MiB file after a hit. fsck
   reports the damaged range. Optional, cheap, decide now or never: put a
   header-only `hdr_crc32c` (over bytes 0..56 with the main CRC zero) in the
   reserved bytes at 56, so fsck can name the lost `write_seq` and `len` of a
   data-damaged record; the resync rule does not need it.
9. **M — Unauthenticated index runs can lose a new write through dedup.**
   §17.3's argument ("every use is verified before data is returned") holds
   for reads, not for dedup: DESIGN §5 step 4 points the map at the hit's
   location and stops, so a forged or wrong run entry `X → (seg, off)` that
   names a valid extent of another id makes the new chunk `X` never be
   stored; every later read of `X` fails verification, falls back to the same
   bad entry and fails. Checksums do not stop this on a hostile bucket (phase
   2+). Fix, a reader rule in §17.3/§17.4 and a line in M5/M6: a dedup hit
   taken from an index run (as opposed to the in-process table built from
   verified footers) is confirmed by reading the extent header at the hinted
   location and comparing all 32 bytes of `chunk_id`, and its tag or AEAD in
   keyed modes, before the map points at it; an unconfirmed hit is a miss.
   With that rule a bad run is an availability cost only, and checksum-only
   runs are the right call (a per-block MAC would fit single-block GETs but
   would need 84 entries per block to make room for 16 bytes; not worth it).
10. **M — Checkpoint lane tails after `fsck --repair --truncate-tip`.** §18
    says the hint "never changes the result". After a truncate-tip the
    checkpoint's `last_commit_seq` is above the tip, and its lane tails say
    files holding records ≤ the *old* tip's cut may be skipped: records in
    `(new cut, old cut]` are then not replayed. Fix: the reader uses
    `lane_tail[]` only if `last_commit_seq ≤` the tip's `commit_seq`;
    `--truncate-tip` also zeroes both checkpoint slots. Same repair, second
    hole: the open-time orphan sweep deletes ids above the new high-water mark
    and the allocator would then reuse them, breaking §3 "ids are never
    reused" (stale index entries then resolve to a different extent; safe
    only because ids are verified). Fix: state the allocator rule, next id =
    1 + max(checkpoint `id_high`, highest id named by the chain, highest id
    present before the sweep).
11. **m — Bounds.** Every variable-length field has a maximum. Two are nominal:
    index `nblocks ≤ 2^32 − 1` (a 128 GiB block index) and `ncovered ≤ 2^24`;
    say that a reader loads a block index incrementally or cap `nblocks` at
    2^28 (1 TiB run, 2^34 entries, far above any volume at the stated limits).
12. **m — Replay and splice.** Commit chain, lease epoch floor, keyring
    `keyring_seq` pin, extent AAD without location, and the family-scoped
    extent identity are all stated correctly. One acknowledged gap to record
    in §25 or ADR-11, not a format change: the `keyring_seq` floor lives only
    in the local pin, so an orchestrator path cannot detect a keyring
    rollback; low value to an attacker (an old keyring yields nothing a holder
    of the old credential did not already have), but if wanted, a
    `keyring_seq` u64 in the FULL body costs 8 bytes now and a migration later.

### E. Future-proofing (check 5)

13. **M — Thin clones cannot share map nodes.** ADR-25 reserved `lineage_idx`
    in the leaf dictionary (hints into a parent's segments) but the interior
    child entry `{child_hash, pack u64, off u32, len u32}` has no lineage
    field and no reserved bits, and `pack` is a volume-scoped `ObjId`. A clone
    therefore has to copy every leaf and interior node of its base (about
    300-350 MiB per TiB) at creation, or a new node encoding is needed (the
    map pack's trailer `ver` can version it, so old packs stay readable, but
    it is a second encoder and decoder forever). `len` is at most 65 644, so
    the top byte of `len` is free: redefine offset 44 as `len` u24 and offset
    47 as `lineage_idx` u8 (0 in v1, rejected if non-zero). Zero cost now;
    §23.5 and §23.6 keep their bytes (the byte is already zero). Decide
    explicitly either way; the ADR-25 "format ready now" claim rests on it.
14. **m — ADR-30 GC needs the dictionary ids per segment.** "A dictionary is
    live while any reachable extent references it" requires the `dictID` of
    every codec-3 extent, which today sits only inside the zstd frame header
    in the payload: a mark on an S3-only volume would need a ranged GET per
    codec-3 extent. Add to the footer body now: `n_dict_ids` u16 at offset 2
    (the reserved bytes) and `dict_ids[]` u32, sorted unique, after the
    entries; the §23.4 example keeps its bytes when `n_dict_ids` = 0 (the cap
    in §7.2 grows by `4 × n_dict_ids`). The footer body is versioned, so this
    could also come as body `ver` 2 later; doing it now avoids two decoders.
15. **m — FIPS.** Envelope form 2 and the HKDF/HMAC derivations are reserved
    correctly, and `fips` is refused at create in phase 1. Two more things a
    FIPS profile needs are already addressable but unnamed: the slot `wrap`
    starts with an unnamed byte `01`; call it `wrap_ver` (1 = XChaCha20-
    Poly1305) so an AES-GCM or AES-KW wrap can be `02`; and note that slot
    kind 1 (Argon2id) is not FIPS-approved, so a PBKDF2 slot kind is reserved
    (4). No byte change.
16. **m — Also checked, no action:** S3 backends and mirrors (ADR-15/24): no
    layout names a bucket, backend or local path, the AAD names are
    bucket-relative, CONFIG/TRANSFER bodies and `since_seq` are complete.
    ADR-28 `refs[].kind = journal` is reserved with an `id` and `hash`; a
    staged object would need a new path and object kind (14+ are reserved),
    which is an addition, not a migration. `dicts/<id:08x>` is a u32 matching
    the zstd `dictID`, sealed under its own kind (ADR-30). Codec 3 is in
    chunkfmt's registry and `Codec::from_id` rejects it, as §3 says. Reserved
    fields are must-be-zero and rejected on read throughout (§1); the only
    unreserved slack found is the one in finding 13. Compression changes
    (ADR-30 `apply`, recompression) cannot go in the create-only descriptor;
    the CONFIG `policy` body or `volume.json` carries them, which needs no
    format change. `Epoch` is effectively capped at 2^24 by `ObjId`; fine.
    `size ≤ 2^56` gives ≤ 6 levels; `levels ≤ 15` is ample.

### F. The open question (check 6)

See "Recommendation" below.

## Hand decodes

Everything in §23 was rebuilt from the tables, not copied, and compared:

- **§23.1 journal batch (volume B):** file header CRC `899d8081`; record 1
  (write, 16 384 × `41`) CRC `cff71822`, 16 448 bytes ending at `0x5040`;
  record 2 (hole) CRC `2965e8cd`, 64 bytes ending at `0x5080`; batch 16 512
  bytes → 5 blocks, `[0x1000, 0x6000)`. The reader rule walks it as stated
  (accept, accept, zero at non-aligned `0x5080`, zero at aligned `0x6000`,
  end). All match.
- **§23.3 encrypted extent (volume A, KAT):** the AAD built from the extent's
  fields per §6.3 equals `aad_hardened` (61 bytes) and the `info` equals
  `info_v2` (100 bytes); `keyed BLAKE3(dek, info)` = `key_hardened`;
  AES-256-GCM(key, zero nonce, AAD, `00..3f`) = `body_hardened`, so bytes
  92-155 and 156-171 of the 172-byte extent are the stated ciphertext and
  tag; the extent opens with the `nonce` field as nonce; flipping a nonce
  byte fails the AEAD. Length 172 = 108 + 64.
- Also recomputed and matching: the four derived keys of volume B and the KAT
  purposes 0x02-0x05; §23.2 `chunk_id`, keyed and unkeyed tags, and the
  75-byte LZ4 block (decodes to 16 384 × `41`; Python's encoder emits the same
  bytes); §23.4 `seg_hash`, the zstd frame (decodes to the 120-byte body,
  content size 120), footer MAC, trailer CRC; §23.5 run-table packing
  `0002104000`, leaf hash, leaf-extent tag, root hash; §23.6 genesis MAC,
  both `parent_hash` values, commit 1 MAC; §23.7 E0-E2 for both fanouts;
  §23.8 both descriptor MACs and the fingerprints.

## Recommendation on the open question (§25)

**Default to `integrity`; do not offer `plain` in v1.** Keep mode 0 in the
format exactly as drafted (decodable, tested, refused by `volume create` like
`id_len = 0`), so the decision is reversible without a byte change.

- Why not plain: on a backend, plain mode has no defence at all. Commits,
  footers and tags are recomputable checksums, so a bucket can rewrite
  history and data undetected; local reads do not recompute the chunk id
  (D11), so a swapped extent with the right header id returns wrong data. The
  mode is fixed in the create-only descriptor, so a plain volume can never be
  made safe later: offering it means either forbidding `backend add` on it or
  shipping volumes that silently lose every integrity property the moment
  they gain a bucket. ADR-11 also records the maintainer's choice of the
  integrity key over corruption detection, and lists the alternative as
  rejected; `plain` as a default would reopen that.
- Cost of integrity: a credential at every attach. For a local-only CLI volume
  that is friction, not security (the host already holds the plaintext
  journal). Remove the friction in M13, not in the format: `volume create`
  without `--encrypt` creates a key-file slot (kind 2) with a random key
  stored next to the volume (`<dir>/<uuid>/integrity.key`, mode 0600) and
  attach picks it up; `--passphrase` adds a slot for volumes that will move.
  Kubernetes already needs a Secret for encrypted volumes, so the integrity
  credential rides the same path. Per-I/O cost is nil (keyed BLAKE3 runs at
  the unkeyed speed; HMAC touches metadata only).
- If the maintainer wants `plain` anyway: restrict it to local-only volumes
  (`backend add` refuses mode 0) and say in the CLI that it detects
  corruption only. The attach matrix already has its rows.

## What the maintainer is signing

1. Every structure is little endian, explicit offsets, byte-string magics,
   canonical encodings enforced by decoders, reserved = zero and rejected.
2. Three crypto modes in one descriptor byte; `fips` only with `encrypted`
   and unwritable in phase 1; one MAC primitive (HMAC-SHA-256) for all
   metadata, BLAKE3-128 for extent tags, unkeyed BLAKE3 checksums in plain
   mode; purpose ids 0x06-0x09 and object kinds 3-13 (4 retired).
3. Index runs are checksummed, never MAC'd or sealed (with the dedup
   confirmation rule of finding 9 as the condition that makes this safe).
4. Journal: no batch header; the 4 KiB grid plus the zero-magic rule; `gen`
   and `off` in every record; recovery as D12 (or as amended by finding 8).
5. One `ObjId` counter for segments, packs and runs; `epoch << 40 | counter`;
   ids never reused (finding 10's allocator rule).
6. Map: 3-byte node header, `rb16` leaf with a lineage byte per dictionary
   entry and `OVH` 68/108 in its arithmetic, 48-byte interior entries,
   per-level empty hashes, hash inputs that exclude locations; whether
   clones may share nodes (finding 13).
7. Commits: envelope kind 1 or 3, body kinds 1-5, FULL carries the root
   location, every kind carries `created_at`, DELTA carries `prev_cut_seq`,
   `cut_seq`, `size` and a records codec, CONFIG `policy` is opaque and
   versioned; maximum counts as §10.6.
8. Keyring with `root_kind`, `dek_gen`, `next_slot_id`, three slot kinds,
   Argon2id bounds, XChaCha wraps; the descriptor MAC input and label.
9. Local files: two-slot checkpoint, device record with `max_io_buf_bytes`,
   pins with mode and family, the `volume.json` key set, `state/gc/condemned`
   envelope; text forms and paths of §22; `gc/<pass>` from a listing.
10. The attach check order, `NeedCredential` and `ModeMismatch`, and `BadMac`
    where ADR-11 F7 says "downgrade".
11. The spec field additions of §24.1 (plus `codec_level`, `lineage_count`),
    and review 2's N12 (the tag covers every byte before it).

## Order of fixes

Before sign-off: 1, 6, 8 (as a D12 amendment the maintainer approves), 9, 10,
13, and a decision on the open question and on 14. The minors (2-5, 11, 12,
15) can land in the same commit. After the fixes, only §23.4 changes if 14 is
taken with `n_dict_ids` placed elsewhere than the reserved bytes; with the
placement proposed here, no worked example changes.
