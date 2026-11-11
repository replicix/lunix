# ADR-11 crypto design review (P0.8)

Date: 2026-11-11
Author: executing model (Sonnet-class), **not the cryptographer of record**.
Evidence: `spikes/crypto-kat/` (40 tests, all green), `bench/crypto-kat/RESULTS.md`,
`bench/crypto-kat/raw/`.

> **Read this first.** No finding breaks the confidentiality or integrity of the
> *per-extent* construction in ADR-11 as amended on 2026-11-11 (salted per-extent key,
> constant nonce, AES-256-GCM). The amended nonce rule holds in the model under
> recompression, codec change, crash retry and compaction.
>
> Two findings **do** affect integrity/confidentiality at the protocol level and block
> the format freeze until fixed (both are cheap):
>
> - **F5 (integrity of history):** the commit chain is linked by `parent_seq` only, so
>   valid commits from two histories (for example an abandoned DR-steal branch, ADR-24)
>   can be spliced into one chain that the MAC/AEAD checks accept. Fix: a `parent_hash`
>   in every commit.
> - **F7 (confidentiality of future writes):** the encryption-downgrade defence is a
>   local trust-on-first-use pin, but the product premise is ephemeral clients that mount
>   from S3 alone, which have no pin. Fix: supplying key material to attach implies
>   "must be encrypted", with an out-of-band KMK fingerprint.
>
> Three more block the freeze because they change bytes or object layout: **F2** (the
> extent has no `dek_epoch` field), **F6** (key-slot mutation protocol undefined; the
> keyring is both "create-only `volume`" and a separate object), **F9** (ADR-11 does not
> say which bucket objects are sealed; segment footers, heat maps, `gc/`, `readers/` are
> unspecified) and **F10** (a FIPS profile cannot seal with a fixed nonce through
> aws-lc-rs; the header must reserve a nonce field).
>
> **Gate outcome proposed: *amended*** (text in `bench/crypto-kat/RESULTS.md`,
> "Proposed decision record"). ADR-11 stays *proposed* until an external reviewer answers
> the questions at the end.

## Question

Is the key hierarchy and extent framing of ADR-11 (and the parts of DESIGN §5, §6, §8, §9,
§15 that depend on it) safe to freeze at the start of phase 1?

## Scope

In: ADR-11, DESIGN §5 step 6, §6 extent layout and bucket layout, §8 commits/lease, §15,
ADR-2/9/15/24 where they touch keys, the data-path note §5, Constellation `e2e.rs` and its
September 2026 audit (findings 2, 12), the fact-check note (conditional writes; used only
for the rollback-anchor discussion).

Out: implementation of the product's encryption (phase 1+), side-channel analysis of
`aws-lc-rs`, fuzzing, a formal proof. Everything here is a design review plus tests of a
model; nothing was read from a real bucket.

## Threat model

| actor | capabilities | goals we defend |
|---|---|---|
| bucket reader (provider, other principals with read access, a mirror operator) | reads every object and its metadata (key names, sizes, times, access pattern) | confidentiality of guest data and file-system structure |
| bucket writer (compromised credentials, malicious mirror, provider) | writes, deletes, replays, reorders, truncates, swaps any object, including old valid ones | integrity; no silent acceptance of forged, relocated, downgraded or spliced data; (availability is out of reach: they can delete) |
| stale or zombie owner | holds the lease key material; may write after fencing | no two writers acknowledged |
| guest | chooses all plaintext written to the block device | cannot cause key/nonce reuse, cannot collide chunk ids, cannot exhaust memory through decoders |
| host-local attacker (other uid, core dump, swap, cold disk) | reads host disk and process memory it can reach | keys not written to disk; local store plaintext is **out of scope of ADR-11** (see F11) |
| KMS/credential admin | can add or remove slots, or lose a credential | slot semantics are explicit and honest (what removal does and does not revoke) |

Assumptions: AES-256-GCM, BLAKE3, XChaCha20-Poly1305, HKDF-SHA-256, Argon2id are secure
as specified; the OS CSPRNG works (the one place this is relaxed is F1b).

## Findings

Severity: **Critical** breaks confidentiality or integrity of the construction as written;
**High**/**Medium** a protocol-level weakness or a gap that forces a format change;
**Low** hardening; **Info** noted. "Freeze" = blocks the format freeze.

| id | item | finding | severity | freeze | evidence |
|---|---|---|---|---|---|
| F1a | 1 | Amended nonce rule is sound: fresh salt per `seal()`, key unique per (DEK, chunk_id, salt). Safe under recompress, zstd upgrade, crash retry, compaction, level change | OK | no | `adr_scheme_survives_10k_reencodes…`, `same_salt_under_different_dek…` |
| F1b | 1 | The rule depends on the RNG and on *API shape*. A replayed/duplicated RNG (VM snapshot restore, forked state) or a "cache the header, redo the payload" retry reintroduces the original hole for any pair of different encodings of one chunk | Low (hardening) | no | `stuck_rng_is_the_residual_risk…`, `cached_salt_anti_pattern_trips` |
| F2 | 2 | The extent header (DESIGN §6) has no `dek_epoch`; a reader cannot select the key after rotation. Also "epoch" names two things (lease epoch, DEK epoch) | Medium | **yes** | `dek_epoch_rotation_protects_only_new_writes`; ADR profile ignores a wrong label |
| F3 | 2 | Neither family id nor object kind is in the derivation or AAD; separation rests on every family having a distinct KMK. Data extents and map-node extents (DESIGN §7) share one key space and one keyed-hash space | Low | no (but free to fix now) | `adr_derivation_does_not_bind_family_hardened_does` |
| F4 | 3 | AAD is adequate (version, codec, level, ulen, chunk_id). Wrong `ulen` dies at the tag before any decompress. Location-independent AAD is *required* by verbatim repack/mirroring/dedup and is harmless because the chunk id binds content | OK | no | `tamper_each_header_and_payload_field_is_rejected`, `relocation_between_chunk_ids_fails` |
| F5 | 6 | Commit chain linked by `parent_seq` only; spliced valid commits (abandoned steal branch, old copies) are accepted | Medium (integrity) | **yes** | `commit_fork_splice_is_invisible_without_parent_hash` |
| F6 | 7 | Key-slot mutation protocol undefined: `volume` is create-only yet holds the slots; `families/<id>/keyring` is a second home; no rule for CAS, mirroring, rollback of a keyring (a removed slot can reappear) | Medium | **yes** | DESIGN §6.1; `removing_a_slot_revokes_nothing…` |
| F7 | 10 | Downgrade defence is a local TOFU pin; ephemeral clients have none | Medium-High (confidentiality of later writes) | **yes** (protocol) | `attach_policy_blocks_the_encryption_downgrade` |
| F8 | 7 | Constellation's keyring AAD is the constant `keyring-master`: KDF parameters, slot id, family id are not bound; Argon2 parameters read from the bucket are not bounded (a hostile keyring can request gigabytes) | Low-Medium | no | `slot_spliced_from_another_family…`, `argon2_parameters_are_bounded…` |
| F9 | 4, 6 | ADR-11 names commits and lease as sealed; map nodes (DESIGN §7) yes; **not stated**: segment footers, `heat/`, `snapshots/`, `readers/`, `holds/`, `gc/`, `sync/*/progress`. Footers expose offsets, sizes and codecs; `heat/` exposes access patterns; `readers/` exposes host names | Medium | **yes** | table in item 4 |
| F10 | 8 | aws-lc-rs marks every `LessSafeKey` *seal* method "must not be used" in FIPS mode; only `RandomizedNonceKey` seals. A fixed-nonce FIPS profile is impossible; it needs a stored random nonce | Medium | **yes** (reserve the field) | aws-lc-rs 1.19.0 `aead.rs` FIPS notes; `fips_profile_stores_a_fresh_random_nonce` |
| F11 | 9 | Local journal, dirty overlay and index hold plaintext at rest; ADR-11 protects the bucket only. Key memory rules are sound but `mlockall` (already required for ublk, CONVENTIONS rule 7) subsumes per-key `mlock` | Info (document it) | no | DESIGN §4 |
| F12 | 6 | The mutable `lease` object has constant AAD, so any older version replays through the AEAD | Low | no | `lease_replay_passes_the_aead_and_needs_an_epoch_floor` |
| F13 | 5 | Verify-before-use order is right; needs an absolute `ulen` cap taken from the volume (not the header) for plain volumes too | OK / Low | no | `verify_order_…`, `zstd_bomb_is_stopped_at_the_cap` |

## Checklist

### 1. Nonce/key uniqueness of the salted per-extent key — OK (hardening F1b)

*Construction under review.* `key = blake3::derive_key("lunix extent v1", DEK_epoch ‖
chunk_id ‖ salt)`, `salt` random 192-bit in the extent header, AES-256-GCM, constant nonce.

**Fresh salt on every encryption.** The property holds if and only if every path that
produces extent bytes goes through one function that draws the salt itself. The model's
`Writer::seal` does; the tripwire test `adr_scheme_survives_10k_reencodes_of_one_chunk…`
runs 10,000 re-encodes of one chunk (Raw, LZ4, zstd-1, zstd-3, and the same encoding again
as a crash retry) with zero reuse and zero benign repeats. The weakness is structural: an
implementation that caches the extent header from a first attempt and redoes only the
payload (a natural "optimisation" when the pipeline degrades zstd→LZ4 under backpressure,
DESIGN §5) reuses the salt, and `cached_salt_anti_pattern_trips` shows the ledger catching
it. **Rule to write into DESIGN §5/§6:** *the salt is generated inside the seal function,
never stored in or accepted from any retry state; an extent builder never exposes a
salt setter; re-encoding always re-seals.*

**192-bit salt.** Two extents of one `(DEK, chunk_id)` collide with probability at most
`k²/2^193`; with k = 2^40 re-encodes of a single chunk that is 2^-113, and summed over a
whole family of 2^46 extents it stays below 2^-100. Keys of distinct derivation inputs
collide only at the 256-bit birthday bound (`N²/2^257`, 2^-157 at N = 2^50). Negligible.

**Domain separation of the context string.** `derive_key` and `keyed_hash` are different
BLAKE3 modes (distinct flag bits), so the extent context cannot alias the keyed-hash
purpose keys (test `derive_key_mode_is_separated_from_keyed_hash_mode`). The ADR does not
name the context strings of the *other* purposes (addressing key, DEK epochs, metadata
key, volume MAC, KMK pin); this review proposes a table (item 11).

**Input encoding.** `DEK(32) ‖ chunk_id(id_len) ‖ salt(24)` has one variable-length field
in the middle and fixed-length neighbours, so the total length fixes `id_len` and the
encoding is injective even if a family ever mixes id lengths (test
`extent_key_input_encoding_is_injective_across_id_lengths`). The proposed form below makes
`id_len` explicit anyway.

**F1b, the residual risk.** Reuse needs the same `(DEK, chunk_id, salt)` with two
*different* payloads. Under an honest OS CSPRNG that is the 2^-113 case. Under a replayed
RNG (restored VM snapshot, forked process with copied user-space state, a broken
entropy-less container) it is reachable: the test models it with a replaying salt source
and the tripwire fires on the second encoding. Three escalating mitigations were built
and measured:

| mitigation | covers (stuck RNG) | cost (64 KiB, 1 core) |
|---|---|---|
| A. derive from `codec, level, clen` as well (proposed `Hardened` profile) | codec change, level change, any length change | free (keyed BLAKE3 is *faster* than `derive_key`: 174 vs 254 ns) |
| B. hedged salt = keyed-BLAKE3(rng ‖ chunk_id ‖ encoding ‖ payload bytes) | every difference in payload bytes; identical payload gives identical salt and identical ciphertext (harmless) | +12.2 µs (about 4× the AES-GCM cost) with full BLAKE3 over the payload |
| C. as B but with a cheap checksum (xxh3-64 or hardware crc32c) of the payload instead of BLAKE3 | same, with 2^-64 accidental collision | **not measured**; expected ~1-2 µs at 64 KiB (UNVERIFIED) |

Recommendation: adopt A (zero cost) and the API rule; make B/C a reviewer question (Q3)
rather than a default. Do not batch-draw salts from a buffered RNG pool: a buffered pool
is exactly the state a VM snapshot duplicates.

**Cost of the per-extent key schedule** (details in RESULTS; Zen 5, one core, median of 3
runs, max/min ≤ 1.03):

| extent | AES-GCM seal, pre-keyed | + derive (ADR as written) + key setup | overhead | + fresh salt (OS RNG, 347 ns) |
|---|---|---|---|---|
| 4 KiB | 301 ns (13.6 GB/s) | 652 ns (6.3 GB/s) | +117% | |
| 16 KiB | 838 ns (19.6 GB/s) | 1201 ns (13.6 GB/s) | +43% | 1471 ns with proposed derivation (+76%) |
| 64 KiB | 3442 ns (19.0 GB/s) | 3766 ns (17.4 GB/s) | +9% | 3975 ns with proposed derivation (+15%) |

The absolute added cost is about 0.35 µs per extent (derive 254 ns including the context
hash, or 180 ns with a pre-initialised `Hasher::new_derive_key` clone; AES key schedule and
GHASH tables 94 ns), and about 0.55-0.65 µs with the proposed derivation plus a fresh salt.
At 5 GB/s of ingest that is 76k extents/s at 64 KiB (about 4% of one core) or 305k/s at
16 KiB (about 19% of one core). It is not a bottleneck: the zstd stage costs one to two
orders of magnitude more per extent (DESIGN §5 table). At 4 KiB the overhead is large in
relative terms, which is one more reason not to use extents that small.

### 2. Same chunk id, different epochs and families — issue (F2, F3)

Epochs: `DEK_epoch` is in the derivation (as the DEK itself), so identical `(chunk_id,
salt)` under two epochs give unrelated keys (test
`same_salt_under_different_dek_epoch_or_family_does_not_collide`). But the *label* that
tells a reader which epoch to use is not in the DESIGN §6 header at all (F2), and in the ADR
profile it is not authenticated: a wrong label is harmless only because it selects a wrong
DEK and fails the tag. Add `dek_epoch: u32` to the extent header (or the segment header,
since a segment is written in one epoch; a compaction output may mix epochs, so per
extent is safer) and bind it in the AAD.

Families: the family id is in neither the derivation nor the AAD; the test shows that the
ADR profile decrypts an extent when only the `family` argument differs and the DEK is the
same. Separation therefore relies entirely on KMKs being unique per family (random, never
copied, never imported from a key file shared by two families). Clones share a keyring
(DESIGN §6.1), so a family boundary is a KMK boundary; fine, but put the family id into the
derivation and the AAD anyway (zero cost) so a mistaken key-file reuse, a restored keyring
or a bug in keyring lookup fails closed. Same for an object-kind byte: data chunks and map
nodes (DESIGN §7: "encrypted with ADR-11's scheme keyed by the node hash") live in one key
space and one keyed-hash space, so a data chunk whose bytes equal a node's canonical
encoding has the same id as that node. The guest cannot construct such bytes (node
encodings contain keyed hashes), so this is hardening, but give nodes their own address key
purpose and a `kind` byte in the derivation.

### 3. AAD contents — OK, with additions

What the current AAD stops (each case is a test, all rejected): **relocation** of an
extent under another chunk id (`relocation_between_chunk_ids_fails`: tag failure even if the
attacker also rewrites the map's expectation); **codec swap** (header says zstd, payload is
LZ4: tag failure); **truncation/extension** of the payload (tag failure); **`ulen`
tampering** for six values including `u32::MAX` (tag failure *and* `decompress_called ==
false`, so a decompression bomb cannot be unleashed by editing the header);
**version/level** edits.

Volume id, segment and offset in the AAD (the data-path note §5.2 had them): not needed and
harmful. Extents are copied verbatim by repack and mirroring (ADR-11, ADR-15) and are
shared by dedup, so their AAD must be location-independent. Moving an extent within a
family is a no-op because identical chunk ids mean identical plaintext; moving it across
families fails because the key differs. Proposed additions (all free): `kind`, `dek_epoch`,
`family_id`, `id_len` (profile `Hardened`).

Unencrypted volumes have no tag. There the only protection against a lying `ulen` is the
bounded decompressor with a cap taken from the *volume's* chunk size, never the header
(`decompress_bounded` does; a 256 MiB zstd bomb of a few KiB is cut off at the cap in
under 500 ms, test `zstd_bomb_is_stopped_at_the_cap`). An unencrypted volume has
corruption detection (unkeyed BLAKE3 ids, Merkle map) but **no authenticity** against a
bucket writer; say so in the docs (Q14).

`id_len`: with unkeyed 128-bit truncated ids, a collision costs about 2^64 hash
evaluations; only the volume's own writer can mount that attack, on its own volume.
Acceptable. Second-preimage stays 2^128.

### 4. Keyed addressing and what the provider learns — needs-reviewer

Visible to a bucket reader: (a) **equality of chunks within a family**: `chunk_id` is in the
clear in every extent header and in map leaves, so two LBAs holding the same data are
visibly the same id (this is the price of dedup; confined to the family, ADR-13); (b)
**sizes**: `clen` and the extent boundaries; (c) **compressibility** of every chunk, a
CRIME-style length side channel of compress-then-encrypt; (d) **access patterns and write
cadence** from object names (`commits/<seq>`, `segments/<seg>`), times, GET ranges; (e)
**sparsity**: holes are map runs; (f) hot ranges from `heat/`.

Not leaked: plaintext, the addressing key, any relation to a public hash of the plaintext
(so confirmation-of-file attacks fail, unlike convergent encryption).

The length channel needs an attacker who can place chosen data in the same 64 KiB chunk as
a secret and observe the resulting `clen`; for a block device with one trust domain per
family that is a narrow case. The `pad` option (bucket the `clen` to 64 B or to the chunk
size) closes it at a space cost; record it as an opt-in and let the reviewer decide whether
the default should pad (Q5). No measurement was made of what padding costs on real data.

### 5. Verify-before-use order and failure behaviour — OK

The model's reader does: header sanity (version, known codec, header `chunk_id` equals the
map entry; no crypto) → AEAD tag → `ulen` ≤ volume cap → bounded decompress → recompute the
keyed id → compare. Tests prove each step runs only after the previous one and report the
distinct error: a lying-but-validly-sealed `ulen` is caught after the tag and before
decompress; a valid extent holding the wrong bytes is caught only by the id recompute
(`verify_order_tag_then_bounded_decompress_then_chunk_id`). The final comparison uses
`blake3::Hash` equality, which is constant time; the tag comparison is inside aws-lc.

The id recompute also acts as the key-commitment that GCM lacks: a ciphertext cannot be
crafted to decrypt validly under two keys to two plaintexts that both hash to the map's
keyed id without the addressing key (relevant to invisible-salamander style attacks; Q4).

Behaviour on tag failure (rules for DESIGN §10.2): do not return data; count it
(`extent_auth_failures_total{backend}`); try another InSync backend before failing with
EIO; never cache the bad extent; never panic; never log plaintext or keys; a *tag-valid but
id-mismatch* extent is a writer bug or a key compromise and should raise an alarm, not a
retry.

### 6. Commit, DELTA and lease encryption — issue (F5, F12)

**Messages per key / nonces.** XChaCha20-Poly1305 with random 192-bit nonces: the collision
bound is `n²/2^193`; even 2^50 sealed objects under one key (a PUT per millisecond for
35,000 years) is 2^-93. OK. aws-lc-rs has no XChaCha, so metadata uses the RustCrypto
`chacha20poly1305` crate (same as Constellation), a second AEAD implementation to audit.

**Key separation per object kind.** Proposed: `Purpose::Meta { kind }` for `commit`,
`lease`, `delta`, `mappack`, `heat` (and `footer`/`gc`, F9) so a key leak or an
implementation bug in one kind cannot be used on another; tests show a commit sealed under
the commit key does not open under the lease key.

**AAD = object identity.** The model uses `(volume uuid, kind, object name)`. Not the
bucket name or backend id: mirrors hold byte-identical objects (ADR-15), so the AAD must be
backend-independent. A commit moved to another sequence number or volume fails.

**F5, the chain is not hash-linked.** DESIGN §8.2 commits carry `parent_seq`. Each commit's
AEAD binds its own name (`commits/<seq>`), but nothing binds a commit to *which* parent it
followed. Test `commit_fork_splice_is_invisible_without_parent_hash`: two chains sharing
commits 1-3, diverging afterwards; a chain mixing main's commit 4 with the branch's commit
5 passes the `seq`-only check, and fails with `parent_hash`. Where do two valid branches
come from? ADR-24's DR steal explicitly leaves an *abandoned* branch in the old bucket;
mirrors can lag; an owner can crash between a TRANSFER and its copy. A bucket writer can
then present a history the owner never produced. Fix: every commit kind (FULL, DELTA,
EPOCH, CONFIG, TRANSFER) carries `parent_hash = BLAKE3(parent object bytes)` plus `epoch`;
mount walks from the newest FULL and verifies links; epochs may not decrease. The FULL
commit's separate "MAC over map root, epoch and parent" is redundant with the AEAD for an
encrypted volume (the AEAD authenticates the whole body), and is the *only* integrity for
an unencrypted one, where it needs its own key (an HMAC key held in the key slots, or an
Ed25519 key) or it protects nothing: write which of the two it is.

**F12, lease replay.** `lease` is rewritten in place (CAS), so every version has the same
identity and any old version is a valid ciphertext. The attacker gains little (the commit
chain is the real fence: a zombie loses its next slot at 412) but can force spurious
takeovers or resurrect `released = true`. Rule: put the epoch inside the plaintext (the
model does) and reject a lease whose epoch is below the newest epoch seen in the verified
chain or in local state.

**Rollback and fork anchors.** The chain is internally consistent but a bucket writer can
still serve an *older prefix of the true chain*. Defending that needs state outside the
bucket the attacker controls. Candidates:

| anchor | defends | does not defend | notes |
|---|---|---|---|
| local state: last-seen `{seq, epoch, tip_hash}` per volume on the host | rollback and forks seen from that host after its first attach | a fresh or ephemeral host (the CSI pod-move case); loss of the local disk | cheap; `tip_hash` makes forks visible, which `seq` alone does not |
| orchestrator object (K8s CR status or CSI attachment annotation) | rollback seen by any host of the cluster, across pod moves | cross-cluster moves; an etcd-level attacker; must stay a *floor hint* (can raise the expected seq, never lower it) or it contradicts ADR-22 "the bucket is the truth" | updated by the owner after each FULL |
| KMS context | identity binding only (family id, slot id) | **freshness**: KMS has no monotonic counter; EncryptionContext is not a version; CloudTrail is audit, not a gate | a counter in Secrets Manager/SSM/DynamoDB would work but is the external lock service ADR-2 rejected, here as a read-only anchor |
| maximum over mirrors (ADR-15 cursors) | one backend rolled back while another is intact | an attacker with write access to every backend, or mirrors sharing one credential set | free with multi-backend; wants different accounts |
| write-only notary bucket (separate account; the owner appends signed or MAC'd `{seq, tip_hash, epoch}` receipts; hosts cannot delete) | an attacker holding only the data bucket's credentials | an attacker holding both | one extra PUT per FULL; like a mirror with a smaller role |
| transparency log / witnesses (signed tree heads cosigned by independent parties) | rollback and forks even with the bucket and owner credentials stolen | witness collusion, availability | new infrastructure; v2 at earliest |
| TPM/NV counter | local rollback by restoring the host's disk | new hosts | marginal |

No candidate is free of a trade-off. A realistic v1: local state with `tip_hash` always,
plus "maximum over mirrors" when the volume has several backends, with the notary bucket as
the documented upgrade (Q8).

### 7. Key slots — issue (F6, F8)

**Argon2id.** Constellation's defaults (64 MiB, t = 3, p = 1) cost 75-83 ms per
derivation on this host (single run each, three samples). Parameters are stored per slot
and unwrap uses the stored parameters, never today's defaults (correct, kept). New: bound
them when *reading* (ceiling 1 GiB, t ≤ 10, p ≤ 16): without a ceiling a hostile keyring
makes a client allocate whatever it asks (`argon2_parameters_are_bounded…`: 4 GiB is
refused in under 50 ms, before any allocation), and enforce a floor (19 MiB, t ≥ 2, OWASP
minimum) when *creating* a slot.

**Binding.** Constellation wraps with AAD `keyring-master`. Lowering the stored `t` or
swapping the slot id then does not change the AAD. The model's AAD is `"lunix slot v1\0" ‖
family_id ‖ slot_id ‖ kind ‖ every KDF parameter ‖ KDF salt`; tests show a slot spliced from
another family, an edited `t`, and a renumbered slot all fail to unwrap even with the right
passphrase.

**KMS slots.** The blob is bound by the KMS *encryption context* `{family_id, slot_id,
key_ref}`: moved to another family or renumbered, it fails; a different KMS key fails.
Equivalents: AWS KMS `EncryptionContext`, GCP KMS `additionalAuthenticatedData`, Azure Key
Vault wrap has **no** AAD parameter (needs a MAC over the slot instead), Vault transit
`context` needs a derived key. A per-provider check is open work (Q9).

**What removing a slot means (to be stated in the docs).** Removing a slot deletes one
wrapped copy of the KMK *from the current keyring object*. It revokes nothing already
exposed: the holder of that slot's credential who kept, or can still fetch, any older
keyring version (mirrors, bucket versioning, backups, a bucket writer replaying an old
object) still unwraps the **same KMK** (test
`removing_a_slot_revokes_nothing_already_exposed`). The KMK derives every DEK epoch, the
addressing key and the metadata keys, so it opens all past *and future* data until the
volume is migrated to a new KMK (re-addressing everything: the "migrate volume"
operation of the data-path note §5.4). A passphrase change is a re-wrap of the same KMK and
has the same property (test `passphrase_change_is_a_rewrap…`).

**Rotation.** DEK-epoch rotation protects **new writes only**: with the epoch-0 DEK an
attacker opens old extents and not epoch-1 extents (`dek_epoch_rotation_protects_only_new_writes`).
Because of dedup, a new write of data that already exists in an old extent is a dedup hit
and keeps pointing at the old, old-key extent, so rotation does not re-encrypt hot data
until compaction rewrites it. KMK compromise opens everything.

**F6, the protocol around the slots.** DESIGN §6.1 lists wrapped slots inside the
create-only `volume` object *and* a separate `families/<id>/keyring` "shared by clones".
Slots must be added and removed after creation, which a create-only object cannot do. The
keyring needs: a single home (the family keyring), CAS update on the authority only,
byte-identical copies to mirrors, a monotonic `keyring_seq` inside the MAC'd body so an old
keyring cannot silently come back, and a defined effect on a mounted volume. The ADR-24
rule "no other mutable membership state" does not cover it.

### 8. FIPS profile — issue / needs-reviewer (F10)

*Structure.* The proposed derivation is a PRF with the DEK as key: `keyed_hash(DEK, info)`
for the default profile, `HKDF-Expand(PRK = DEK, info, 32)` = `HMAC-SHA-256(DEK, info ‖ 0x01)`
for the FIPS profile, with the *same* `info` bytes (`info_v2` in the spike; vectors for
both are published and cross-checked). The ADR's `derive_key("…", DEK ‖ chunk_id ‖ salt)`
has no PRF-key slot (the secret is part of the "key material") and has no direct HKDF
analogue; this is the reason to adopt the keyed form.

*AES-GCM with an externally fixed nonce.* In aws-lc-rs 1.19.0's `aead.rs` every
`LessSafeKey::seal_*` method is annotated `// # FIPS: This method must not be used`;
`open_*` is allowed; `RandomizedNonceKey` (AES-GCM, nonce generated inside the module and
returned) is the supported way to encrypt. So a FIPS build **cannot** perform the ADR's
fixed-nonce seal. The compatible profile keeps the unique per-extent key and stores the
module's random 96-bit nonce (12 extra bytes, 0.02% at 64 KiB). The spike's `Fips` profile
does this and passes the full round-trip and tamper tests (in a non-FIPS aws-lc build;
the FIPS module itself was **not** built or run). Whether FIPS 140-3 implementation
guidance treats this construction (unique derived key per message plus a module-generated
IV) as approved is a question for the reviewer, as is the HKDF usage.

*Format consequence.* Reserve a `nonce` field (flagged, or always present as 12 bytes
that are zero in the default profile) in the header **now**; adding it later changes the
extent layout.

### 9. Memory handling — needs-reviewer, with notes (F11)

- The per-extent key is 32 bytes of stack data derived and used within one call: keep it in
  `Zeroizing`; the spike does. Whether aws-lc-rs wipes the expanded AES schedule on drop was
  not verified (UNVERIFIED; the C library's context cleanup is expected to).
- `mlock`: ublk daemons must `mlockall` and set `PR_SET_IO_FLUSHER` anyway (CONVENTIONS rule
  7), which pins the heap and stacks, derived keys included. Per-key `mlock` as in
  Constellation (`region::lock`, best effort, warns on failure) is then redundant but
  harmless; the point that remains is `Zeroizing` on drop and `MADV_DONTDUMP` /
  `PR_SET_DUMPABLE 0` / `RLIMIT_CORE 0`, because core dumps and hibernation images are
  what `mlock` does not stop.
- Queue threads never block (ADR-18): the derive cost (about 0.25-0.35 µs) is small enough
  to run on the worker pool; queue threads should not need DEKs at all (reads decrypt on
  the pool or on a dedicated thread). Keep the DEK in one immutable, `mlock`ed,
  `Arc`-shared struct per epoch rather than copies per thread.
- **Plaintext at rest locally.** The journal, dirty overlay and index hold plaintext
  (DESIGN §4). ADR-11 protects the bucket and the extents cached from it; a stolen host disk
  reveals recent writes. State this plainly and recommend dm-crypt/LUKS for the local
  directory; it is a product decision, not an ADR-11 flaw (F11).

### 10. Downgrade — issue (F7)

Constellation's audit finding 12 (plain `meta.json` decided whether a mount was E2E) was
fixed with a *local* pin (`registry.e2e.toml`). Lunix DESIGN §15 copies that ("The KMK
fingerprint is pinned locally on first attach") and adds a MAC on `volume`. But the volume
model (DESIGN §2, ADR-24) is that every host can mount from S3 alone and the client is
ephemeral; a fresh host has no pin, and the MAC key comes from the KMK that only an
*encrypted* volume provides, so an attacker who deletes `volume` and recreates it as a
plaintext volume produces a descriptor with nothing to verify.

Rule (tested in `attach_policy_blocks_the_encryption_downgrade`): **the host's attach
request, not the bucket, decides whether the volume must be encrypted.** If the caller
supplies any key material (a slot credential, a KMS reference, a key file) the volume must
be encrypted and its descriptor MAC must verify under the unwrapped KMK; otherwise attach
fails with `Downgrade`. The orchestrator (StorageClass / `LunixBackendSet` / the Secret)
additionally carries the KMK fingerprint, so a different encrypted volume (attacker's own
KMK) is refused as `WrongKmk`. Local TOFU remains for the interactive CLI. Without any of
these inputs an attacker-made plaintext volume is indistinguishable from a legitimate one,
by construction.

### 11. Domain separation strings, versioning, test vectors — OK

Proposed registry (every string is `lunix <purpose> v<N>` plus a NUL where it is a prefix
of a longer message; a new version replaces the whole string; no label is shared between
two primitives or two purposes):

| use | primitive | label / context | where it lives |
|---|---|---|---|
| extent key (as written) | `blake3::derive_key` | `"lunix extent v1"` | ADR-11 |
| extent key (proposed) | `blake3::keyed_hash(DEK, info)` / HKDF-Expand | `"lunix extent v2\0"` prefix of `info` | spike `info_v2` |
| purpose keys under the KMK (addressing, DEK epoch, metadata kind, volume MAC, pin) | `keyed_hash(KMK, msg)` / HKDF-Expand | `"lunix kdf v1\0" ‖ family_id ‖ tag ‖ args` | spike `purpose_msg` |
| key-slot wrap AAD | XChaCha20-Poly1305 AAD | `"lunix slot v1\0" ‖ family ‖ slot_id ‖ kind ‖ params` | spike `slot_aad` |
| metadata AAD | XChaCha20-Poly1305 AAD | `"lunix meta v1\0" ‖ volume ‖ kind ‖ name` | spike `meta.rs` |
| volume descriptor MAC | keyed BLAKE3 | `"lunix volume v1\0" ‖ fields` | spike `meta.rs` |
| salt hedge (optional) | keyed BLAKE3 | `derive_key("lunix salt hedge v1", DEK)` as key | spike `HedgedSalt` |

Constellation uses `keyed_hash(master, "constellation/e2e/v1/" ‖ …)`; a Lunix KMK must never
be shared with Constellation, and because the labels differ, an accidental sharing still
yields unrelated keys.

*Test vectors.* Published in `bench/crypto-kat/RESULTS.md` and
`bench/crypto-kat/raw/kat-vectors.json`, asserted in `tests/kat.rs`, and **independently
reproduced** by `tools/crosscheck.py` using a pure-Python BLAKE3 written from the
specification (checked against the official empty-input hash), `cryptography` for
AES-256-GCM and Argon2id (including the RFC 9106 vector), a hand-written HChaCha20 feeding
`cryptography`'s ChaCha20-Poly1305 for XChaCha, and `hmac`/`hashlib` for HKDF-SHA-256. The
AES-256-GCM zero-key, zero-nonce cases (McGrew-Viega 13 and 14) are asserted too. The
cross-check covers single-chunk BLAKE3 inputs (≤ 1 KiB) only.

## Items outside the checklist

- **`ulen` and chunk size.** For both encrypted and plain volumes cap decompressed output at
  the volume's chunk size, not at the header's `ulen` (F13).
- **zstd determinism.** Salting means compressor determinism is irrelevant to security; it
  only matters for dedup of extents, which operates on `chunk_id` not on extent bytes.
- **XChaCha implementation.** aws-lc-rs 1.19.0 does not expose XChaCha20-Poly1305 (no match
  in its source); metadata uses RustCrypto `chacha20poly1305 0.11.0`. The FIPS profile for
  metadata therefore needs an AES-GCM alternative with a unique key per object (derive a
  key from the metadata key and a random salt, as for extents). Not built.
- **Key-committing AEAD.** GCM and Poly1305 are not key-committing. The keyed id recompute
  closes this for extents; slot blobs are only decrypted by someone holding the credential,
  and an attacker with a *chosen-ciphertext* oracle on the passphrase (a partitioning
  oracle) gains nothing over the offline guessing the readable keyring already allows.
  Raise in Q4.

## Open questions / UNVERIFIED

- Whether the FIPS module is satisfied by the unique-key-plus-module-IV construction: not
  checked; FIPS aws-lc was not built.
- Whether aws-lc-rs wipes the AES key schedule on drop: not verified.
- Cost of mitigation C (cheap payload checksum in the salt hedge): not measured.
- Whether GCS/Azure/Vault KMS APIs offer an AAD/context equivalent to AWS KMS
  `EncryptionContext` for the slot binding: read from memory, not verified.
- Cost of `pad` on real data; compression ratios; anything about real workloads.
- Per-extent cost on CPUs without VAES (AES-NI only), and on AArch64: not measured. The
  4-5× gap between hardware AES-GCM and software in the data-path note §5.1 was taken from
  there, not re-measured.
- Behaviour with a real KMS; the KMS was a modelled AEAD (`FakeKms`).
- Numbers are one host (AMD EPYC Zen 5, VAES), shared with other workloads; three runs
  agreed within 3%.

## Questions for an external reviewer

Each is self-contained; excerpts are quoted from ADR-11, DESIGN and the spike.

1. **Fixed nonce, one message per key.** ADR-11: "The cipher is AES-256-GCM with a fixed
   nonce, because every key encrypts exactly one payload." Key = `derive_key("lunix extent v1",
   DEK_epoch ‖ chunk_id ‖ salt)`, salt random 192-bit stored in the extent header, AAD =
   `ver ‖ codec ‖ level ‖ ulen ‖ chunk_id`. Is AES-256-GCM with a constant all-zero 96-bit
   nonce and a fresh key per message sound in every respect you would audit (tag
   forgery bounds, related-key concerns with BLAKE3-derived keys, anything about GHASH key
   `H` being the same for all messages under a given AES key schedule, which here is also
   unique)? Is there any reason to prefer storing a random nonce instead (see Q10)?
2. **KDF shape.** The ADR passes the secret DEK as part of `derive_key`'s *key material* with
   public data appended. The proposal replaces it with `keyed_hash(DEK, info)` (PRF with
   the secret as key; HKDF-Expand in the FIPS profile). Is the keyed form the better
   choice, and is anything lost relative to `derive_key`'s domain separation?
3. **Salt strategy.** Reuse requires the same `(DEK, chunk_id, salt)` with two different
   encodings. Mitigations: (A) put `codec, level, clen` into the derivation; (B/C) derive
   the salt as `keyed_hash(rng ‖ chunk_id ‖ encoding ‖ payload or checksum-of-payload)` so a
   replayed RNG only ever repeats a salt for identical bytes. Is A enough? Is a 64-bit
   payload checksum in the hedge acceptable? Should the product forbid buffering RNG output?
4. **Key commitment.** AES-GCM and Poly1305 are not key-committing. For extents the
   recomputed keyed `chunk_id` (the addressing key is unknown to a bucket writer) is the
   only commitment. Is that enough against invisible-salamander and partitioning-oracle
   style attacks given that the salt and `chunk_id` are attacker-controllable on read?
5. **Metadata leakage.** The provider sees equal `chunk_id`s within a family, `clen`
   distribution, object names and timing, holes, and (if unsealed) footers and heat maps.
   Is "equality within a family plus compressed sizes" an acceptable default, and should
   padding to a size class (`pad`) be on by default?
6. **Relocation.** The AAD is deliberately location-independent (no volume, segment or
   offset) so repack, mirroring and dedup can copy extents verbatim. Is there an attack
   within one family or across families that this enables beyond what the keyed
   `chunk_id` check already stops?
7. **Commit chain.** DESIGN §8.2 FULL commit: `{kind, seq, epoch, parent_seq, cut_seq, size,
   map_root, …, mac}`, sealed with XChaCha20-Poly1305 under a metadata key, AAD = object
   identity. Proposed: every commit carries `parent_hash = BLAKE3(parent object bytes)`.
   Is that sufficient to detect splices of valid branches (including the ADR-24 abandoned
   steal branch), and should the hash cover the plaintext or ciphertext?
8. **Rollback anchor.** DESIGN §22.1: "A malicious bucket can serve an old but valid
   chain." Which of local state, orchestrator object, max-over-mirrors, write-only notary
   bucket, or a witnessed log would you require for a v1 claim of "detects rollback", and
   what must be said in the docs if none is present?
9. **Key slots.** The slot blob is `XChaCha20-Poly1305(KEK, aad = "lunix slot v1\0" ‖
   family ‖ slot_id ‖ kind ‖ KDF params ‖ salt)`; KMS blobs are bound by encryption context
   `{family, slot, key_ref}`; Argon2id is 64 MiB, t = 3, p = 1 with a ceiling on read and
   a floor on create. Are these parameters and bindings adequate, how should Azure Key
   Vault (no AAD on wrap) be handled, and is the keyring mutation protocol (single CAS
   object, `keyring_seq` inside the MAC'd body, mirrored) enough to stop a removed slot
   from coming back?
10. **FIPS.** aws-lc-rs forbids caller-supplied nonces for sealing under FIPS. The proposed
    FIPS profile stores the module-generated 96-bit nonce and uses HKDF-Expand with the
    DEK as PRK. Is that approvable in a FIPS 140-3 deployment? Should the nonce field exist
    in all profiles to keep one format?
11. **Downgrade.** Rule: "if the caller supplies key material, the volume must be encrypted
    and its descriptor MAC must verify; the orchestrator supplies a KMK fingerprint".
    Does this close the gap that a local TOFU pin leaves for ephemeral clients, and is the
    descriptor MAC (keyed by the KMK) meaningful for an attacker who recreates `volume`?
12. **Domain separation of data and map nodes.** Map nodes are sealed with the extent scheme
    and addressed by the same keyed BLAKE3 as data chunks. Proposed: a `kind` byte in the
    derivation and a separate address key for nodes. Anything else needed to prevent
    cross-kind confusion in dedup or GC?
13. **Memory.** Is `mlockall` + `Zeroizing` + `MADV_DONTDUMP`/`RLIMIT_CORE 0`/`PR_SET_DUMPABLE 0`
    sufficient for DEK/KMK handling in a long-running daemon that is also an I/O flusher
    (no swap, no allocation on the reclaim path)?
14. **Integrity-only mode.** An unencrypted volume uses unkeyed BLAKE3 ids and a Merkle map;
    its commits have no secret. Is a v1 "integrity without confidentiality" mode (HMAC or
    Ed25519 over the commit) needed to make any integrity claim, or should the docs say
    unencrypted volumes detect corruption only?
15. **Blast radius.** One KMK per family, shared by clones, derives every DEK epoch, the
    addressing key and all metadata keys; recovering from KMK compromise is a full
    migration. Is a two-level scheme (a per-volume key wrapped by the family KMK) worth the
    complexity in v1?

## Sources

- ADR-2, ADR-9, ADR-11, ADR-15, ADR-24: `docs/explanation/DECISIONS.md`.
- DESIGN §5, §6, §6.1, §7 (node identity), §8, §9, §15, §22: `docs/explanation/DESIGN.md`.
- `docs/explanation/research/2026-11-11-data-path-hash-compress-encrypt.md` §5.
- `docs/explanation/research/2026-11-11-cache-consistency-s3-protocol.md` §3.2, §3.3, §6.3.
- `docs/explanation/research/2026-11-11-fact-check.md` §A.
- Constellation `crates/store-s3/src/e2e.rs`; `docs/security-audit-2026-09.md` findings 2, 11, 12.
- aws-lc-rs 1.19.0 `src/aead.rs` and `src/aead/rand_nonce.rs` (FIPS annotations), read in the
  crate source on the test host.
- Standards used for vectors: McGrew and Viega, "The Galois/Counter Mode of Operation",
  test cases 13 and 14 (AES-256, zero key and IV); RFC 9106 §5.3 (Argon2id); the BLAKE3
  specification (compression function, modes); RFC 5869 (HKDF).
