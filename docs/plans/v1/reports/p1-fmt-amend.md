# Format amendment of 2026-11-12: keyed integrity chunk ids and the rollback floor

- Branch `p1/fmt-amend`, merged with `origin/main` at 490ebde (the `lunix-store` test fix). Lead-approved
  cross-crate change: `crates/lunix-format/**` and `crates/lunix-crypto/**` only. `tools/check-ownership.sh`
  has no entry for this branch and was skipped (`LUNIX_SKIP_OWNERSHIP=1`); every other `ci.sh` step ran.
- Spec: on-disk-formats.md items 45 and 46, §4.2 (purpose 0x0a), §4.5, §20 (`state/pins` v2), §23.2, §23.4-§23.6,
  §23.9; attach-matrix.md Table E.

## What was built

- `lunix-format`: `PurposeId::IntegrityAddressing` (0x0a, allowed in `integrity` only) and
  `Purpose::IntegrityAddressing`; `ALL` has 10 entries and 0x0b.. are reserved. `state/pins` version 2
  (136 bytes): `RollbackFloor { max_commit_seq, tip_hash }` and `LocalPinsFile { pins, floor }` with strict
  encode and decode; version 1 (96 bytes) is still read as "no floor" and never written.
- `lunix-crypto`: `Material::Integrity` holds the 0x0a key; `PlainExtents::chunk_id` and the `verify_id`
  recompute give keyed BLAKE3 for data in `integrity` mode, unkeyed for `plain`, unkeyed for node hashes in both.
  Module docs updated (the `id_len` 16 residual is gone for `integrity`).
- Tests: new types keep the negative suite (all prefixes, 10 000 mutations, canonical re-encode, allocation
  bound) and proptest; new crypto tests for R1/R2 (node as data, data as node), the old public id failing
  `verify_id`, family-dependent ids, and §23.2 / §23.5 byte for byte.

## Goldens

Written from the document's §23 tables by a script that reads the hex out of the doc (never from encoder
output); the tests compare the encoder to them and, where the doc gives a hash or MAC, recompute it.

- Regenerated: `a0-extent-plain-keyed` (§23.2), `a0-segment-one-extent` (§23.4), `a0-node-leaf-one-entry`
  and `05-node-interior-root` (§23.5), `a0-commit-full-1` (§23.6).
- Changed because they are the same §23.5 content: `03-extent-node` (the leaf's 113-byte extent),
  `05-node-interior-lineage-nonzero` (the root with one byte set), `05-empty-hashes.txt` (the two §23.5 hash
  lines). Not named in FREEZE.md.
- `11-purposes.txt` gains the 0x0a row (default/FIPS values are derived by the test; the volume B value is
  checked against the doc literal separately).
- Added: `14-pins-v2.bin` (§23.9, crc `0x2b7403c6` matches the doc), `14-pins-v1-read.bin`.
- Unchanged: `04-segment-footer-body` (synthetic multi-entry body, not the §23.4 body), `14-pins.bin` and
  `14-pins-none.bin` (version 1 files, now read-only inputs), index-run goldens (synthetic).

## Deviations and open choices

1. `LocalPins` keeps its shape (adding a field breaks the literal in `lunix-store/src/tests/misc.rs`, outside
   this change's ownership). `LocalPins::encode` writes version 2 with no floor and `LocalPins::decode` drops a
   floor. The anchor lives in `LocalPinsFile`. `lunix-store`'s `local_pins` / `store_local_pins` must move to
   `LocalPinsFile` before anything raises the floor, or a rewrite would lose it.
2. `attach_matrix.rs` (lunix-crypto test) stops loading at "## Table E": Rollback and Fork are results of the
   attach chain step, not of `check_attach`. A new test pins Table E's shape (rows 120-126). The floor check
   itself is not implemented here (M2 attach / store).
3. FREEZE.md: only the golden lists of items 11 and 14 were edited. Checkboxes were already ticked, the
   amendment note and the re-sign line are untouched.

## rust-skills conflicts

None new. Same as the M1.A report (`missing_docs` not enforced; narrowing `as` after a range check in encoders).

## Gate

Full `tools/ci.sh` on the test host (`-j 4`, CPUs 12-15) with `LUNIX_SKIP_OWNERSHIP=1`: `ci: all green`, 689 tests
passed, 0 failed workspace-wide, including the `lunix-store` suites (no flake or hang after the main fix, so they
did not need separate runs). `lunix-format` 166 tests (f01 10, f02 13, f03 11, f04 12, f05 11, f06 26, f07 23,
f08 9, f09 8, f10 7, f11 4, f12 4, f13 4, f14 13, f15 7). `lunix-crypto` integration: api_rules 4, attach_matrix 7,
contract 27, extent 22, golden 7, keyring 17, kat 2, meta_chain 9, types 3, zeroize 1.
