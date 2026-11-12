# M2 review: `lunix-crypto` (`p1/m2-crypto` at 5e3d751)

Independent security review, 2026-11-12. Read: `docs/plans/v1/reports/p1-m2.md`, `crates/lunix-crypto/**`,
ADR-11 with all amendments, DESIGN §5, §6, §6.1, §8.4, §15, §22.1, on-disk-formats §4, §6-§13, §17, §20,
`attach-matrix.md`, `kat-vectors.json`, the phase 0 crypto review, the M2 plan section, and the AWS-LC
sources behind `aws_lc_rs::rand::fill`. The gate was re-run on the test host (`tools/ci.sh`, `-j 4`,
CPUs 4-7, rustc 1.98): **`ci: all green`**, `check-features: ok`, `check-ownership: m2 ok`; `lunix-crypto`
90 tests pass, among them `kat::every_field_of_the_vector_file_is_reproduced` (37 fields),
`attach_matrix::the_matrix_file_has_the_rows_it_says` (119 rows, 186 combinations, 162 base once, 72 in
Table A) and `check_attach_equals_the_matrix` (no differences), `tamper_matrix_every_byte_of_every_mode`
(every byte of 16 KiB extents with two bit flips and of 64 KiB extents, raw and LZ4, three modes, plus
truncations and a trailing byte), `a_million_seals_never_repeat_a_pair`, `a_replayed_rng_fires_the_tripwire`.
The remote directory was removed afterwards.

## Verdict: **merge after fixes**

No blocker. Two majors: the random source is not the operating system (finding 1, a small code change
plus a re-measurement), and integrity-mode safety rests on three rules that live in other milestones and
must become normative text (finding 2). The construction itself (key hierarchy, AAD, `info`, per-kind keys,
slot binding, descriptor MAC, matrix) matches ADR-11 byte for byte where vectors exist and is tested well.

**Decisions asked of the reviewer.** (1) `rustix` edge: approve (already pinned and in the lock, the `mm`
feature is present; `libc` would be worse). (2) Self dev-dependency: approve; it is the standard way, and
`cargo build -p lunixd` compiles the crate without `test-rng` (see finding 7). (3) `check_attach(&desc,
Option<&[u8]>, &AttachRequest)`: approve, the plan's positional form cannot unwrap a slot. `WrongCredential`
and `is_wrong_key_class()`: approve, the matrix leaves them outside and the engine needs the class.

## Findings

1. **Major. The salt and nonce source is AWS-LC's userspace CTR-DRBG, not the OS.** `src/rng.rs` says
   "draws from the operating system on every call: there is no buffered pool and no seed", and ADR-11 F3
   says "the salt never comes from a buffered RNG pool". `aws_lc_rs::rand::fill` is `RAND_bytes`, which in
   AWS-LC (`crypto/fipsmodule/rand/rand.c`) is a per-thread CTR-DRBG reseeded from the OS only every
   `kCtrDrbgReseedInterval = 4096` generate calls (`rand/internal.h`), with OS prediction resistance mixed in
   on the first call only; fork is detected, but a VM snapshot is detected only through SysGenID
   (`crypto/ube/vm_ube_detect.c`), which mainline Linux does not have. A restored snapshot therefore replays
   up to 4096 salts per thread: exactly the F1b case. Impact is bounded by mitigation A (`chunk_id`, codec,
   level and `clen` are in `info`), so a repeated pair needs the same chunk re-sealed with the same encoding
   but different payload bytes (a non-deterministic compressor), but the rule is violated and the
   documentation is wrong. Fix: `rng::fill_os` calls `getrandom(2)` (`rustix::rand::getrandom`, feature
   `rand` of the already-pinned crate, or the `getrandom` crate already in `Cargo.lock`) for the extent salt,
   metadata nonce, slot nonce and Argon2 salt, and root generation. The kernel CRNG is reseeded on VM fork
   by vmgenid since 5.18, below the ublk floor. Re-measure the 64 KiB and 4 KiB seal (the spike's 347 ns
   "fresh salt" figure was also aws-lc); one syscall per seal stays inside the 1.5× bound. Correct the
   doc comments in `rng.rs`, `lib.rs` and the `extent.rs` module docs.
2. **Major. Integrity-mode correctness depends on three rules outside this crate.** The keyed tag covers
   the extent bytes only (D11), so kind and location are unbound and `open` with neither `chunk_id` nor
   `id_prefix` in `OpenExpect` accepts *any* valid extent of the family. The analysis below shows no format
   change is needed, provided the map, the store and the engine enforce rules R1-R4 (section "Attacks").
   They appear in plan prose (M11 step 4, M5 gate, M14 items 3-4) but are not stated as rules of the
   format. Fix: the lead puts R1-R4 into the M5, M7, M9, M11 and M14 briefs and into DESIGN §6/§7 as
   normative; M2 adds to the `OpenExpect` and `ExtentCodec::open` docs (with the M1.B1 owner) that a data
   read without an id expectation is a scan and must set `verify_id`.
3. **Minor. Hostile keyring cost runs inside `lunixd`.** `keyring::unlock` tries every passphrase slot;
   the read bounds (1 GiB, t 10, p 16, 32 slots) are the format's, so a bucket writer can cost an attach
   minutes of CPU and 1 GiB of memory, locked if the daemon runs `mlockall`. Fix: a policy ceiling in
   `Unlock::Passphrase` or `AttachRequest` (for example `max_m_kib`, default 256 MiB, and a total budget
   over the slots tried), try the slot recorded at the last attach first (`only_slot` exists), and enable
   the `argon2` crate's `zeroize` feature so the 64 MiB of blocks are wiped.
4. **Minor. `KeyFile::read` follows symlinks and blocks on a FIFO.** Open with
   `O_NOFOLLOW | O_NOCTTY | O_NONBLOCK` and check `metadata().is_file()` before reading; log (do not
   refuse, a Kubernetes mount is not 0600) when group or other bits are set. `write_new` is right
   (`create_new`, 0600, fsync of file and directory).
5. **Minor. `dek_gen` above the keyring's is `Corrupt` before the AEAD** (`extent.rs::open_inner`,
   `k.dek(dek_gen)?`). Before authentication a forged header cannot be told from a writer bug; return `Auth`
   like the other pre-AEAD id checks, or document the class in the `ExtentCodec::open` contract.
6. **Minor. Tripwire in every `debug_assertions` build** (`tripwire.rs`): a dev build of `lunixd` leaks
   16 bytes per seal for the life of the codec. Gate it on `cfg(test)` or the `tripwire` feature only, or
   cap the ledger and count overflow.
7. **Minor. Lint coverage of the non-feature path.** `cargo clippy --workspace --all-targets` unifies
   `test-rng` and `tripwire`, so the `#[cfg(not(feature = "test-rng"))]` arms (`fips_allowed`, `Source`) are
   never linted; add `cargo clippy -p lunix-crypto --lib -- -D warnings` to `tools/ci.sh`. Also give
   `arena.rs` a `#[cfg(miri)]` heap-only path so `cargo miri test -p lunix-crypto` can run the rest.
8. **Minor. `id_len = 16` residual in integrity mode.** The read compares a 16-byte prefix; a guest who
   finds two chunks with equal prefixes (a 2^64 BLAKE3 birthday search on its own data) plus a bucket
   writer can swap them on a local read, since local reads do not recompute the id. Document, and make
   `id_len = 32` or `verify_reads` the recommendation for integrity volumes exposed to untrusted guest
   content. Encrypted volumes are not affected (keyed ids).
9. **Minor. Documentation.** `check_attach` rule 4 also rejects a non-zero `kmk_fingerprint` on a plain
   descriptor (right, but not in the matrix text); `finish()` may leave decrypted bytes in a reallocated
   `Vec`'s old allocation (local plaintext is outside ADR-11, F11, but say so next to `rollback`).

Checked and found right: salt drawn inside `seal` after `check_desc`, `out` untouched on RNG failure
(`extent.rs::seal`); no salt or nonce parameter outside `test-rng` (`api_rules.rs`); FIPS zero-nonce seal
only through `AesGcmExtents::with_rng(.., true)`, otherwise `Unsupported`; `allow_nonzero_nonce: false`
on every open; per-extent key, DEK of older generations, Argon2 KEK, unwrapped root all `Zeroizing` or
arena-backed; `rollback()` wipes partial output; tripwire records a BLAKE3 digest, never the key; AAD and
`info` fields each tested alone (`every_aad_and_info_field_is_authenticated`); kind, family and `id_len`
moves fail; node and data addressing keys differ (purpose 0x06); metadata keys per kind, name and kind in
`meta_aad`, envelope header checked against the expectation and bound through the AAD; commit `seq` in the
name and the body; `object_hash` over stored bytes; lease epoch floor; keyring MAC over `meta_aad ‖ body`,
slot AAD over family, slot id, kind and all KDF parameters; `keyring_seq` checked after the MAC; wrong
passphrase, corrupted wrap, foreign family and bad MAC are one error after the same work; all tag, MAC,
fingerprint and id comparisons through `ct::eq` (aws-lc `verify_slices_are_equal`); `arena.rs`: the three
`unsafe impl`/blocks carry SAFETY comments, slots are owned by exactly one `Secret`, zeroed before reuse,
mappings never unmapped, `mlock`/`MADV_DONTDUMP` best effort with counters, heap fallback zeroized.

## Attacks by a bucket writer, per mode

Secrets a bucket writer lacks: `encrypted`: KMK and everything derived; `integrity`: IK, the extent tag
key, the integrity HMAC key, descriptor and keyring MAC keys. Unkeyed in both: index runs (CRC), container
trailers (CRC), `head`, object names and sizes. Rules referenced (for the briefs of M5, M7, M9, M11, M14):

- **R1 (M11, M7).** Every data read passes `OpenExpect { id_prefix: Some(leaf prefix) }` (or the full id
  when it comes from the index) and `chunk_size` = the volume's; the codec compares the header `chunk_id`
  with it (`extent.rs::check_expected`, after the tag on unencrypted volumes, before the AEAD on
  encrypted). A read with neither is a scan and sets `verify_id`.
- **R2 (M5).** After opening a node extent (`kind = Node`, `codec` must be `Raw`), recompute the node hash
  (leaf: the whole encoding; interior: header ‖ child hashes, §8.5) and require it to equal the parent's
  `child_hash` (the FULL's `map_root.hash` for the root) **and** the extent header `chunk_id`. This holds
  for nodes from the local pack store too, not only fetched ones.
- **R3 (M7, M6, M17, M14).** A footer is used only after `MetaCodec::open(Footer, "<container name>",
  bytes)` succeeds; whenever the extent region is read whole (fsck, compaction, `catch_up`), `seg_hash`
  is recomputed and compared. A container whose trailer or footer fails is absent, never partly used.
- **R4 (M11).** At attach, after `verify_chain`, refuse a tip below the local floor (next section), and
  open the lease only through `open_lease_fresh(bytes, max(chain epoch, pins.max_epoch))`.

| # | attack | encrypted | integrity | detected by |
|---|---|---|---|---|
| 1 | swap two valid extents | covered | covered (R1; residual finding 8 at `id_len` 16) | `check_expected` against the leaf prefix, which the Merkle chain and the commit MAC/AEAD authenticate; the AEAD also binds id to ciphertext. Equal ids mean equal plaintext, so a "swap" of equal ids is a no-op |
| 2 | move or relocate an extent (in or across segments) | covered | covered (R1, R3) | location is unbound by design; a moved extent either carries the expected id (same content) or fails R1; a rearranged region fails `seg_hash` under R3; a stale hint falls back to the index and is confirmed against the full id |
| 3 | replay an older extent for the same LBA | covered | covered (R1) | the current leaf's prefix is the current content's; an older content has another id. Replaying the leaf too is attack 7/8 |
| 4 | node presented as data, data as node | covered | covered (R1, R2), no format change | encrypted: `kind` in AAD and `info`, separate addressing key (tested). Integrity: a node as data passes only if the leaf expects `BLAKE3(node bytes)`, that is the guest wrote those bytes as data, so the returned data is right; an interior node's hash is not the hash of its stored bytes, so no leaf can expect it without a preimage. Data as node fails R2 unless the payload is the node (then identical bytes) |
| 5 | substitute a footer, index run or map pack | covered | covered (R2, R3) | footer: envelope kind 6, container name in `meta_aad`, HMAC/AEAD (`meta.rs::open_env`); index runs are unauthenticated by design, a wrong hint fails R1 and a wrong dedup hit is confirmed (M9: 32-byte id plus tag), availability only; map pack: R2 plus its footer |
| 6 | truncate or delete objects | detected | detected | commits: `verify_chain` `NotFound`; extents: tag over every byte, decoder rejects truncation and trailing bytes (tested); footers: body_len and MAC; index: CRC. Availability is outside the model; "no automatic fallback" (§10.7) keeps a damaged tip an error |
| 7 | replay or splice commits | covered | covered | `parent_hash`, contiguous `commit_seq`, non-decreasing `epoch` (`lunix_format::commit::check_child`, run by `verify_chain`; tested in `meta_chain.rs`); a commit under another `seq` fails the name in the AAD/MAC; lease replay fails the epoch floor (R4). A second valid commit at the same `seq` exists only from an abandoned branch (ADR-24), which is attack 8 |
| 8 | roll the volume back to an older fully valid chain | **not covered** | **not covered** | nothing in the bucket can; it needs the anchor below. `plain` mode covers none of 1-8 |

### Attack 8: the rollback anchor (DESIGN §22.1), recommendation for v1

What exists: `state/pins` (§20) holds `max_keyring_seq` and `max_epoch`; every attach writes an EPOCH
commit, so a host that has attached before already detects any rollback to before its own last attach
through `max_epoch`. What it does not detect: hiding commits written *during* the last session (same
epoch), and anything on a host that never attached. The checkpoint's `last_commit_seq` is a hint
discarded when above the tip (D17) and must not be used as an anchor.

| anchor | detects | does not | cost |
|---|---|---|---|
| A. local floor in `state/pins`: `max_commit_seq` and `tip_hash` (ver 2 of the 96-byte file; local, not a bucket object), written after every verified attach and after every FULL C6; attach refuses `tip < max_commit_seq` (Rollback) and `tip == max_commit_seq` with another hash (Fork) | rollback and forks seen from that host | a fresh host, loss of the local disk | one local write per FULL |
| B. maximum over InSync mirrors (ADR-15): refuse an authority tip below a mirror's tip | one backend rolled back | an attacker with every backend's credentials | free with several backends |
| C. orchestrator raise-only floor `{commit_seq, tip_hash}` in the CR status (phase 5) | pod moves inside a cluster | cross-cluster, an etcd attacker | one status update per FULL |
| D. write-only notary bucket in another account, a signed or MAC'd `{seq, tip_hash, epoch}` per FULL | an attacker holding only the data bucket | both credential sets | one PUT per FULL, new infrastructure |

Recommend for v1: **A mandatory, B when there are several backends, C as the phase 5 item, D the
documented upgrade.** The docs must say that a host with no floor (first attach, ephemeral client without
C) cannot detect rollback; that is ADR-11 F7's "by construction" case restated for history. Local-only
volumes: whoever writes the local store also holds `integrity.key` and `state/pins`, so no anchor is
possible on that disk; integrity mode defends the bucket copy, and for the stolen or modified local disk
the honest claim is corruption detection plus crash safety (encrypted mode with a passphrase slot resists a
*reader* of the disk, not a writer who also has the key file).

### Answer on integrity-mode binding

Safe without a format change: the leaf prefix, the Merkle chain and the HMAC'd commit bind every data
extent to its LBA and content; node hashes bind nodes; the per-family tag key stops cross-family use; and
the unkeyed id function is the same for data and leaves, which makes kind confusion content-preserving
rather than exploitable. The conditions are R1-R4, which exist today only as plan prose (finding 2), plus
the `id_len` 16 residual (finding 8). Binding `kind` into the tag (a per-kind tag key, a purpose-table
change) would add defence in depth at the price of re-signing freeze item 11; I do not recommend it for v1.

## Phase 0 reviewer questions settled from the code

1. Fixed nonce, one message per key: implemented as specified (`LessSafeKey`, `ZERO_NONCE`, key unique
   per seal through the salt in `info`); GHASH `H` is unique per key. The audit-level bound is the
   external reviewer's; the implementation adds nothing beyond the construction.
2. KDF shape: the keyed form is implemented (`kdf::prf`, keyed BLAKE3 or HKDF-Expand over `info`), with
   `"lunix extent v2\0"` as the `info` label and vectors for both profiles. Nothing lost: keyed-hash mode is
   domain-separated from `derive_key` and the label versions the message.
3. Salt strategy: A is implemented. The product does not forbid buffered RNG output (finding 1).
4. Key commitment: the keyed id recompute exists only when `verify_id` is set; local reads rely on the
   tag (D11). Slot blobs: a crafted multi-key slot gives an attacker who controls the keyring a yes/no on
   "is the passphrase in my set", but building the blob costs one Argon2 per candidate, the same as the
   offline guessing the readable keyring already allows: no advantage.
6. Relocation: covered above; within a family under the same id it is a no-op, across families the key
   differs, kind is bound on encrypted volumes and content-preserving on integrity volumes.
7. Commit chain: `parent_hash` is BLAKE3 of the stored (sealed) bytes (`object_hash`); fine, because the
   AEAD or MAC binds those bytes and mirrors copy them verbatim.
9. Slots: AAD and bounds implemented and tested; KMS and Azure are not in v1 (`Unsupported`); the
   `keyring_seq` floor stops a removed slot's return only on hosts with a pin store (§12 says so).
10. FIPS: the `nonce` field exists in every profile; FIPS derivation is vector-tested; FIPS sealing is
    `Unsupported` in both extent and metadata codecs.
11. Downgrade: implemented exactly as the matrix (186 combinations); row 63 remains open by construction.
12. Data versus nodes: `kind` in `info` and AAD, purpose 0x06 for nodes; integrity mode as analysed above.
13. Memory: arena `mlock` + `MADV_DONTDUMP` + zeroize on drop implemented; `mlockall`, `RLIMIT_CORE` and
    `PR_SET_DUMPABLE` are the daemon's. Argon2 blocks are not wiped (finding 3). Whether aws-lc-rs wipes the
    AES schedule remains unverified here.
14. Integrity-only mode: built as the maintainer decided (HMAC on every commit and the lease, keyed tags);
    `plain` is documented as corruption detection only.
Not settled by code: 5 (padding), 8 (anchor, recommendation above), 15 (two-level keys).
