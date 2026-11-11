# P0.8: crypto design review for ADR-11 (results)

Status: **done**. Outcome: **amended** (a finding requires a design change; proposed text at
the end). ADR-11 stays *proposed* until an external reviewer answers the questions in
`docs/explanation/research/2026-11-11-adr-11-crypto-review.md`.

The executing model is not the cryptographer of record. This file is the measured part; the
reasoning, threat model, per-item verdicts and the 15 reviewer questions are in the review
note.

> **Loud summary.** No finding breaks the confidentiality or integrity of the per-extent
> construction (salted per-extent key, constant nonce, AES-256-GCM): the amended nonce
> rule held in every modelled case. Two protocol-level findings (F5 commit-chain splice,
> F7 downgrade on ephemeral clients) affect integrity/confidentiality and, with F2
> (no `dek_epoch` in the extent), F6 (key-slot mutation protocol), F9 (unspecified sealing
> of footers/heat/gc/readers) and F10 (FIPS needs a stored nonce), **block the format
> freeze**. All are cheap to fix before phase 1.

## Header

| | |
|---|---|
| date | 2026-11-11 |
| host | AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root |
| kernel / distro | 7.0.0 (Ubuntu), Ubuntu 26.04.1 LTS |
| governor | not exposed (virtualised guest has no cpufreq); turbo state unknown |
| toolchain | rustc 1.96.0, cmake 4.2.3, gcc 15.2.0, Python 3.14.4, `cryptography` 46.0.5 |
| privileges | unprivileged; no root, no credentials needed |
| spike revision | uncommitted |
| pinning | build `taskset -c 8-15` with `cargo build -j 4`; benchmarks `taskset -c 12` (one core); CPUs 8-15 were shared with other agents (load average about 6 of 16) |
| crates (exact pins, checked on crates.io 2026-11-11) | aws-lc-rs 1.19.0 (aws-lc-sys 0.47.0), blake3 1.8.7, argon2 0.6.0, chacha20poly1305 0.11.0, zeroize 1.9.1, zstd 0.13.4, lz4_flex 0.11.6, hex 0.4.3 |

## Reproduce

On the test host: `~/lunix-p0/crypto-kat/run.sh` (build, 40 tests, vectors, independent
cross-check, the XOR-leak demo, the cost bench). Raw outputs: `raw/cargo-test.txt`,
`raw/crosscheck.txt`, `raw/demo.txt`, `raw/kat-vectors.json`, `raw/bench-run{1,2,3}.csv`,
`raw/bench-summary.csv`.

Gate rule (plan 00, ADR-11): the review cannot accept the ADR on its own. Outcome is
*amended* (a finding requires a design change, with the proposed text) or *accepted pending
external review*. A finding that breaks confidentiality or integrity blocks the format
freeze. Measured outcome: **amended**.

## Tests (40, all pass: `raw/cargo-test.txt`)

| file | tests | what they prove |
|---|---|---|
| `tests/kat.rs` | 9 | AES-256-GCM zero-key/zero-nonce cases (McGrew-Viega 13, 14); BLAKE3 empty hash; cloned-context derivation equals `derive_key`; extent key + body vectors for ADR / Hardened / Fips; purpose-key vectors (BLAKE3 and HKDF); all purpose keys distinct and family-dependent; derive vs keyed mode separation; injective input encoding; Argon2id RFC 9106 vector; slot KEK and XChaCha vectors |
| `tests/roundtrip_tamper.rs` | 10 | round trip for 3 profiles × 4 codecs × 3 sizes; fresh salt/ciphertext every seal; one tamper per field (version, codec, level, six `ulen` values, chunk_id, salt, DEK epoch label, payload and tag bits, truncate, extend, FIPS nonce); relocation; wrong epoch/family/kind; verify order with trace; zstd bomb cut off; plaintext-injection; FIPS random nonce |
| `tests/tripwire.rs` | 8 | the (key, nonce) ledger: draft scheme trips on recompress and leaks XOR; ADR scheme survives 10,000 re-encodes; per-DEK/per-family isolation; stuck-RNG residual; fixes A/B under a stuck RNG; cached-salt anti-pattern; 200,000 OS salts distinct; birthday bounds |
| `tests/slots_and_meta.rs` | 13 | slot wrap/unwrap, wrong passphrase, parameter ceiling/floor, tampered params/ids, cross-family splice, key-file and KMS slots with encryption context, removal does not revoke, passphrase change keeps all keys, DEK-epoch rotation, metadata AEAD binding, commit-fork splice, lease replay, attach/downgrade policy, nonce budget |

`cargo clippy --all-targets -- -D warnings` and `cargo fmt` are clean (run on the host).

## Known-answer vectors (published; reproduced independently)

Inputs: `dek = 00..1f`, `kmk = 20..3f`, `family = 40..4f`, `chunk_id = 80..9f`,
`salt = c0..d7`, `dek_epoch = 7`, `payload = 00..3f` (64 bytes, raw codec, `ulen` 64), nonce
`00…00` (12 bytes). Full set: `raw/kat-vectors.json`.

| vector | value |
|---|---|
| `key_adr` = `derive_key("lunix extent v1", dek ‖ chunk_id ‖ salt)` | `38db76592bf26edd34ad1ceece8b9e4c90f58c2981058b1f415d35b6af9def1a` |
| `key_v0` (first draft, `dek ‖ chunk_id`, ctx `"lunix extent v0"`) | `d7ebd176b7909dfdb788a9235c0a02edf412815319fc15ede0e140abd2a2a673` |
| `key_hardened` = `keyed_hash(dek, info_v2)` | `11c0b591c327735ee6b91913514ca60e5ea143c2a1f2ebeab34a2443e75774c4` |
| `key_fips` = `HKDF-Expand(dek, info_v2, 32)` (SHA-256) | `f8f8b1a8c43cf9db8b1751e622fa4fff6284217c2a5652f0ab8bbee345041f16` |
| `aad_adr` = `01 00 00 40000000 ‖ chunk_id` | `01000040000000808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f` |
| `body_adr` (ct ‖ tag) | `b2ae6b93fa4237489bbf2b8687e3edca255c14fcd775a2b844f3454cc3e57188c34fcfe5ce8396286a01d8a1601a5b48b9107e8525bbd025d318fd5e4f3556b0a2f1259968acf2f193750bc0bf1d730b` |
| `purpose_addressing` = `keyed_hash(kmk, "lunix kdf v1\0" ‖ family ‖ 01)` | `51be94e46821496d256755858d4c9def5825899b520a03669cd61db4723cfd5f` |
| `purpose_dek1` = `… ‖ 02 ‖ 01000000` | `e6bf9ce3d1f733ac70341fac2fd7857123fecda67ac532f181d8772f82b0d750` |
| slot KEK: Argon2id(m = 19456 KiB, t = 2, p = 1, salt = 11×16, "correct horse battery staple") | `064401872f712d299da5dc8fb3058e4af259cf62e8c3d78419ee23295cc88a19` |
| XChaCha20-Poly1305 (key = dek, nonce = 30..47, aad "aad-1", "lunix xchacha vector") | `3e2c41cfa2fa2d7db2ff252dab6d85252591313d6778eb12fe9079aa31a7420270de6b30` |
| AES-256-GCM, zero key, zero nonce, 16 zero bytes | `cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919` |

`info_v2` layout (100 bytes here): `"lunix extent v2\0" ‖ family(16) ‖ kind(1) ‖ dek_epoch(4 LE)
‖ codec(1) ‖ level(1) ‖ clen(4 LE) ‖ id_len(1) ‖ chunk_id ‖ salt(24)`.

`raw/crosscheck.txt`: 27 independent checks pass (pure-Python BLAKE3 from the spec, validated
against the official empty-input hash; `cryptography` AES-GCM and Argon2id; HChaCha20 +
ChaCha20-Poly1305 for XChaCha; hmac/hashlib for HKDF-SHA-256). The Rust side and the Python
side share no code. Limit: BLAKE3 cross-check covers single-chunk inputs (≤ 1 KiB).

## Item 1 concretely: the draft hole and the amended rule

`raw/demo.txt`: first-draft scheme, one 64 KiB chunk sealed `Raw` then `Lz4` (what a
recompress does):

| quantity | value |
|---|---|
| overlapping ciphertext bytes | 1291 |
| `c1 ⊕ c2 == p1 ⊕ p2` over the whole overlap | true |
| LZ4 literal header | 5 bytes (token + length extension), literal run 1024 B |
| attacker knowledge | the first 5 plaintext bytes (a file magic, say) |
| plaintext bytes recovered | 1029 (the whole first literal run plus the 5 following) |

| scenario (all on one chunk id) | draft scheme | ADR-11 as amended | + derivation over `codec, level, clen` (A) | + hedged salt (B) |
|---|---|---|---|---|
| recompress Raw → LZ4 → zstd-1 → zstd-3, honest RNG | **trips** | no reuse | no reuse | no reuse |
| 10,000 mixed re-encodes incl. crash retries | n/a | 0 violations, 0 repeats | n/a | n/a |
| same encoding again (crash-retried destage) | benign repeat | n/a (fresh salt) | n/a | benign repeat (same salt, same bytes) |
| RNG replayed (VM snapshot model), codec/level change | n/a | **trips** | no reuse | no reuse |
| RNG replayed, same codec/level/length, different bytes (e.g. zstd upgrade) | n/a | **trips** | **trips** | no reuse |
| retry that caches the extent header and its salt | n/a | **trips** | n/a | n/a |
| same salt under another DEK epoch or family | n/a | no reuse | n/a | n/a |

## Cost of the per-extent key schedule (one core, `taskset -c 12`, median of 3 runs of 7 timed rounds)

Max/min across the 3 runs ≤ 1.03 in every cell (no noisy cell). Full table:
`raw/bench-summary.csv`. All "full" rows seal a raw (uncompressed) buffer copied into place,
which is what the data-path note's method did. Not a CPU-cost-per-IO figure.

| ns/op (GB/s) | 4 KiB | 16 KiB | 64 KiB |
|---|---|---|---|
| AES-256-GCM seal, pre-keyed key | 301 (13.6) | 838 (19.6) | 3442 (19.0) |
| AES key setup only (schedule + GHASH tables) | 94 | 94 | 94 |
| `derive_key`, ADR as written | 255 | 255 | 254 |
| `derive_key` with cloned context hasher | 180 | 181 | 180 |
| `keyed_hash` proposed derivation (`info_v2`, 100 B) | 175 | 175 | 174 |
| HKDF-SHA-256 Expand (aws-lc-rs) | 258 | 261 | 258 |
| OS RNG, 24-byte salt | 354 | 354 | 353 |
| **full: ADR as written** (derive + setup + seal) | 652 (6.3) | 1201 (13.6) | 3766 (17.4) |
| full: ADR with cloned context | 577 (7.1) | 1120 (14.6) | 3669 (17.9) |
| full: proposed derivation + fresh salt | 920 (4.5) | 1471 (11.1) | 3975 (16.5) |
| full: proposed derivation + hedged salt (BLAKE3 over payload) | 4981 (0.8) | 8595 (1.9) | 16173 (4.1) |
| full: FIPS profile (HKDF + `RandomizedNonceKey`) | 1349 (3.0) | 1902 (8.6) | 4434 (14.8) |

Reading: the per-extent key schedule costs about 0.35 µs (0.55-0.65 µs with a fresh salt and
the proposed derivation). That is +9% of an AES-GCM seal at 64 KiB, +43% at 16 KiB, +117% at
4 KiB. In absolute terms, at 5 GB/s of ingest: about 4% of one core at 64 KiB extents, about
19% at 16 KiB. The hedge over the full payload with BLAKE3 is 4× the AES cost and is not
recommended as a default. Argon2id at 64 MiB, t = 3, p = 1: 75-83 ms per wrap or unwrap
(single sample per run, three runs).

The 11 GB/s per-core AES-GCM figure quoted in ADR-11 is from the i9-14900K; this host (Zen 5
with VAES) measures 13.6 GB/s at 4 KiB and 19 GB/s at 64 KiB with aws-lc-rs. No
comparison with `ring` or software AES was run here.

## Findings (ranked; evidence in the review note)

| rank | id | finding | severity | blocks freeze | proposed fix |
|---|---|---|---|---|---|
| 1 | F7 | Downgrade defence is a local pin; ephemeral clients have none | Medium-High | yes | key material supplied at attach ⇒ volume must be encrypted + descriptor MAC verify; orchestrator supplies KMK fingerprint |
| 2 | F5 | Commit chain linked by `parent_seq` only; valid branches can be spliced | Medium | yes | `parent_hash` + `epoch` in every commit kind; mount verifies the chain from the newest FULL |
| 3 | F2 | No `dek_epoch` in the extent header; "epoch" is overloaded | Medium | yes | `dek_epoch: u32` per extent, in the AAD; rename to `dek_gen` |
| 4 | F6 | Key-slot protocol undefined (create-only `volume` vs `keyring`) | Medium | yes | one mutable keyring, CAS on the authority, `keyring_seq` in the MAC'd body, mirrored |
| 5 | F9 | Which objects are sealed is unspecified (footers, heat, gc, readers, snapshot labels) | Medium | yes | object-kind table (in the proposed record) |
| 6 | F10 | FIPS: fixed-nonce seal unavailable in aws-lc-rs | Medium | yes (reserve field) | stored 96-bit nonce field; HKDF-Expand profile |
| 7 | F8 | Keyring AAD constant; Argon2 parameters unbounded on read | Low-Medium | no | AAD binds family, slot id, kind, params; ceiling on read, floor on create |
| 8 | F1b/F3/F12 | RNG dependence; family/kind not in derivation; lease replay | Low | no | derive over `family, kind, dek_epoch, codec, level, clen`; salt generated only inside `seal`; epoch floor for leases |
| 9 | F11 | Local journal/overlay plaintext at rest | Info | no | document; recommend dm-crypt |

## DESIGN.md lines contradicted or incomplete (not applied)

- §6 `extent = CCH1 header {magic, ver, codec, level, ulen} + chunk_id(32) + clen + [salt(24) if encrypted] + payload(+16 B GCM tag)`: missing `dek_epoch`, missing the reserved `nonce` (F2, F10), and "if encrypted" must come from the authenticated volume descriptor, not from the extent. Suggested: `… + clen + [dek_epoch(4) + salt(24) + nonce(12, FIPS profile; else zero)] + payload + tag`.
- §6.1 `volume … wrapped key slots (MAC'd)` and `families/<family-id>/keyring`: pick one home for slots (F6).
- §8.2 `parent_seq` and `mac`: add `parent_hash`; say what the MAC key is for plain volumes (F5).
- §15 "The KMK fingerprint is pinned locally on first attach": insufficient for ephemeral clients (F7).
- §5 step 6 "Encrypt (optional): … derived from `chunk_id` plus a random per-extent salt": add the rule that the salt is drawn inside the seal call only (F1b).

## Deviations from the plan

- The plan asks for the note at `docs/explanation/research/<today>-adr-11-crypto-review.md`
  and `bench/crypto-kat/RESULTS.md`; both written. The note is the long form.
- Plan step 1 lists Constellation `e2e.rs` and the audit: both read. The audit file name is
  `docs/security-audit-2026-09.md` in the Constellation tree, as the plan says.
- The plan costs "BLAKE3 derive plus AES key expansion at 64 KiB and 16 KiB"; 4 KiB added.
- FIPS: the FIPS-validated aws-lc build was **not** built. Evidence for F10 is the crate's
  own FIPS annotations in `aead.rs`, read in source; the `Fips` profile runs on the
  non-FIPS build.
- Mitigation C (cheap payload checksum in the salt hedge) is described but not measured.
- KMS and the Azure/GCP/Vault AAD equivalents are modelled / read from memory; no real KMS.

## Not run

- Real KMS unwrap; FIPS module build; fuzzing of the extent parser; side-channel testing;
  AArch64 and non-VAES x86 numbers; pad-option cost on real data.

## Proposed decision record (for the lead to merge; do not apply here)

Append under ADR-11 in `docs/explanation/DECISIONS.md`:

> **Gate outcome (2026-11-11, P0.8): amended, pending external review.** The salted
> per-extent key construction is confirmed by the model (`spikes/crypto-kat`,
> `bench/crypto-kat/RESULTS.md`; review note
> `docs/explanation/research/2026-11-11-adr-11-crypto-review.md`). Status stays *proposed*
> until the reviewer's answers are recorded. The following amendment text is proposed.
>
> **ADR-11 amendment (proposed):**
> 1. *Extent key.* `key = keyed_hash(DEK_epoch, info)` with `info = "lunix extent v2\0" ‖
>    family_id ‖ kind ‖ dek_epoch ‖ codec ‖ level ‖ clen ‖ id_len ‖ chunk_id ‖ salt` (FIPS
>    profile: HKDF-SHA-256 Expand over the same `info`). `kind` separates data extents from
>    map-node extents. The salt is a fresh random 192-bit value drawn **inside the seal
>    function on every call**; no retry, re-encode or compaction path may carry a salt.
>    Debug and fuzz builds run a (key, nonce) tripwire.
> 2. *Extent header* gains `dek_epoch: u32` (authenticated) and a reserved `nonce[12]` field
>    (zero in the default profile; the module-generated nonce in the FIPS profile, where
>    fixed-nonce sealing is unavailable). The AAD becomes `ver ‖ codec ‖ level ‖ ulen ‖ kind ‖
>    dek_epoch ‖ family_id ‖ id_len ‖ chunk_id`. "Encrypted" is read from the authenticated
>    volume descriptor, never from an extent.
> 3. *Rename* the DEK epoch `dek_gen` to avoid the lease/commit `epoch`.
> 4. *Commits.* Every commit kind carries `parent_hash` (BLAKE3 of the parent object bytes)
>    and `epoch`; mount verifies the chain from the newest FULL and rejects decreasing
>    epochs. FULL commits of unencrypted volumes carry an HMAC under an integrity key held
>    in the key slots, or are documented as corruption-detection only.
> 5. *Lease* carries its epoch in the plaintext; a lease with an epoch below the newest
>    verified-chain epoch is rejected.
> 6. *Key slots.* One mutable `families/<id>/keyring` (CAS on the authority, copied to
>    mirrors, `keyring_seq` inside a MAC'd body). Slot wrap AAD = `"lunix slot v1\0" ‖
>    family ‖ slot_id ‖ kind ‖ KDF params ‖ salt`; KMS slots use encryption context
>    `{family, slot, key_ref}`. Argon2id parameters are bounded when read (≤ 1 GiB, t ≤ 10,
>    p ≤ 16) and floored when written (≥ 19 MiB, t ≥ 2). Removing a slot or changing a
>    passphrase does **not** revoke a KMK the holder has already seen; revocation is migration
>    to a new KMK. DEK-epoch rotation protects only new writes.
> 7. *Attach.* If the caller supplies key material, the volume must be encrypted and the
>    descriptor MAC (keyed by a KMK-derived key) must verify, else refuse as a downgrade.
>    The orchestrator supplies the KMK fingerprint; local TOFU remains for the CLI.
> 8. *Sealed objects.* Table: `volume` (plaintext fields + MAC), `keyring` (wrapped, MAC'd),
>    `lease`, `commits/*` and `DELTA` bodies, `map/*` nodes, segment footers, `heat/*`,
>    `gc/*`, `readers/*`, `holds/*`, `snapshots/*` bodies: sealed with XChaCha20-Poly1305
>    under a per-kind metadata key (extents and map nodes with the extent scheme); object
>    names stay plaintext.
> 9. *Registry of labels* as in the review note, item 11. Test vectors in
>    `bench/crypto-kat/raw/kat-vectors.json` are normative for phase 1 tests.
> 10. *Docs.* The local journal and overlay are plaintext at rest (recommend dm-crypt);
>     unencrypted volumes detect corruption but are not authenticated; compressibility and
>     chunk equality within a family are visible to the provider (opt-in `pad`).
>
> DESIGN.md sections to change: §5 step 6, §6 extent layout, §6.1 volume and keyring, §8.2,
> §8.5, §15, §22.1 (rollback anchor candidates). No row of the ADR-11 "Rejected" list is
> reopened.
