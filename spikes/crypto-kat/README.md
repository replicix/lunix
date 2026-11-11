# crypto-kat (P0.8)

Spike for the ADR-11 crypto design review. It *models* the extent writer/reader,
the key slots and the metadata sealing on the real crate choice (`aws-lc-rs`),
so the review in `docs/explanation/research/2026-11-11-adr-11-crypto-review.md`
and `bench/crypto-kat/RESULTS.md` rests on executable evidence. It is throwaway
code, not the product's encryption.

Reproduce (on the test host, never on the dev box): `./run.sh`

What it contains:
- `tests/kat.rs`: known-answer vectors (AES-256-GCM zero-nonce cases, BLAKE3 derive,
  extent keys and bodies for the three profiles, purpose keys, Argon2id RFC 9106, XChaCha).
- `tests/roundtrip_tamper.rs`: round trips and one tamper case per header/AAD field,
  verify-before-use order, zstd bomb cap.
- `tests/tripwire.rs`: the (key, nonce) tripwire, the draft-scheme XOR leak, the
  re-encode/stuck-RNG cases and the proposed fixes.
- `tests/slots_and_meta.rs`: key slots, rotation, commit chain splice, lease replay,
  attach/downgrade policy.
- `tools/crosscheck.py`: independent check of the vectors (pure-Python BLAKE3,
  `cryptography`, hmac/hashlib). `crypto-kat bench` costs derive + AES-GCM at
  4, 16 and 64 KiB on one core.

Prerequisites: Rust >= 1.96, cmake and a C compiler (aws-lc-sys), Python 3 with
`cryptography` >= 45 for the cross-check. Dependencies are pinned exactly in
`Cargo.toml`; `Cargo.lock` is committed.
