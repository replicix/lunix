# M1.A code review: workspace, tools, CI and `lunix-format`

Independent review of branch `p1/m1-a` at `3e5229a` (report
`docs/plans/v1/reports/p1-m1a.md`), against `docs/reference/on-disk-formats.md`
(signed off), `attach-matrix.md`, the plan's M1.A section and gate,
CONVENTIONS.md and `bench/crypto-kat/raw/kat-vectors.json`. 2026-11-12.
Reviewer did not implement the milestone. `rust-skills` loaded; rule ids
cited where relevant.

## Verdict: **merge after fixes**

No blocker. Two majors (a bounds check that is looser than §9.1, and a
branch-name guess in `check-ownership.sh` that breaks the CI of M1.B2a/B2b)
and a handful of minors. Every byte layout I checked matches the signed-off
document; the worked examples are anchored to the document's hex, not to the
encoder's output. The gate reproduced on the test host from a fresh copy.

## Gate reproduced

Run from a fresh sync through `tools/remote.sh run bash tools/ci.sh`
(`LUNIX_HOST` from the environment only, `LUNIX_CPUS=8-15`, `LUNIX_JOBS=8`,
own base directory, deleted afterwards). Result `ci: all green`:

- tool tests: 16 ok; `cargo fmt --check`: pass; `cargo clippy --workspace
  --all-targets -- -D warnings`: pass; all 17 crates compile; `cargo build
  -p lunixd --features crashpoints`: pass; `check-features: ok`;
  `check-ownership: m1a ok (221 changed paths)` (the report's 220 plus the
  report itself).
- `cargo test --workspace`: **157 tests in `lunix-format`, 0 failed**
  (f01 10, f02 13, f03 10, f04 12, f05 11, f06 26, f07 22, f08 9, f09 8,
  f10 7, f11 4, f12 4, f13 4, f14 10, f15 7), as the report states. Other
  crates: 0 tests (skeletons).
- The gate log contains no host name, address or user.
- `df-guard` exercised directly: `ok` at 10 GiB wanted, `refusing` (exit 1)
  at 400 GiB, `LUNIX_MIN_FREE_GIB` honoured; bad `<GiB>`, unset
  `LUNIX_HOST` and a bad milestone id all exit 2 with a message.

## Fidelity to the signed-off layouts

Sampled thoroughly (offsets, widths, legal ranges, reserved-zero checks,
bounds, canonical form, what is authenticated), code against the tables:

1. **Journal (§5)**: file header, record header, `crc32c` over header-with-
   zero-crc ‖ data, `align8`, batch padding, and the normative reader of
   §5.3 (zero magic aligned → end of file, unaligned → next 4 KiB; resync at
   8-byte steps requiring magic, ver, kind, reserved, `gen`, `off`, bounds,
   CRC and rising `write_seq`). The §23.1 bytes are assembled by hand in
   `f02_journal.rs:54-96` from the doc's tables (CRCs `899d8081`,
   `cff71822`, `2965e8cd` as literals) and asserted equal to the encoder.
2. **Extent (§6)**: both layouts, `clen` rule per codec, `ulen = chunk size`
   for data and `≤ 65 536` for nodes, zero-nonce rule, AAD (1/1/1/4/1/4/16/
   1/32 = 61 bytes) and `info` (100 bytes) field order. `aad_hardened`,
   `aad_fips_kdf_zero_nonce`, `info_v2` reproduced from the KAT file (byte-
   identical copy under `tests/vectors/`). §23.2's keyed tag
   `5f2205bd…`, unkeyed `f21b8c97…` and §23.3's KAT ciphertext are literals.
3. **Container (§7)**: trailer (CRC over 0..56, magic at 60, `extent_count
   ≤ floor(footer_off/69)`, length ≤ 2^32−1), footer body (48-byte entries,
   strict `(chunk_id, off)` order, `dict_ids[]` sorted unique), zstd frame
   content size checked against `72 + 52·floor(footer_off/69)` **before**
   decompression, frame must be exactly one frame. §23.4 hex (`seg_hash`
   `0392362e…`, MAC `d945f9bf…`, trailer CRC `59ed78e9`) as literals.
4. **Map node (§8)**: `rb16` columns in the doc's order with the lineage
   column after the dictionary, run record `start ‖ seg_idx ‖ start_off ‖
   rank` LSB first, `cum[g]` definition, widths checked (`wseg =
   bits(ndict)`, `wsd ≤ 64`, `woff ≤ 32`, `wc ≤ 32`, `wcum ≤ 48`), canonical
   form enforced by re-encode, `lineage_idx` non-zero rejected (leaf and
   interior), interior entry `len` u24 + `lineage_idx` byte, hash input
   header ‖ child hashes only. §23.5 leaf (45 bytes), leaf and root hashes
   and all ten §23.7 empty hashes are literals (`05-empty-hashes.txt`).
5. **Commits (§10)**: common header (`has_parent` iff `commit_seq ≠ 0`,
   `parent_seq = commit_seq − 1`, reserved flag bits, `has_heat` FULL only),
   FULL/DELTA/EPOCH/CONFIG/TRANSFER offsets and maxima, DELTA iff envelope
   kind 3, genesis per D5, `check_child` (contiguity, `parent_hash`, epoch
   monotone; `created_at` not checked, D17). §23.6 genesis and commit 1 are
   full hex literals in `f06_commit.rs:79-104`; the MACs are recomputed with
   the doc's integrity HMAC key `aa13505a…`.
6. **Descriptor (§11)**, **keyring (§12)**, **envelope and `meta_aad`
   (§4.4, §9)**: §23.8 A and B as literals; `fips` only with `encrypted`;
   plain ⇒ zero fingerprint; slot layout and `slot_aad_pass` reproduced;
   Argon2id read bounds enforced on decode, write bounds as constants.

Spot-checked the rest (lease, pins, opaque, GC files, index run, checkpoint,
device, pins file, `volume.json`, paths, purposes) by reading six captured
goldens field by field against the tables (`14-device`, `10-lease-
transferred`, `07-pin-snapshot`, `09-keyring-passphrase`, `13-index-run-one-
block`, `06-commit-config`): all offsets and widths agree. Purpose messages
and the eleven KAT purpose outputs are in `11-purposes.txt`.

**Golden independence.** Every `a0-*` golden is first asserted equal to a
hex literal transcribed from §23 and only then compared with the file, so a
drift of the encoder fails the test before the golden could be regenerated.
Non-`a0` goldens are encoder captures (the doc gives no hex for them), which
is acceptable; the field tests and my manual reading cover their layout.

## Findings

Severity: blocker / major / minor. Paths are relative to the repository.

1. **major** — `crates/lunix-format/src/envelope.rs:138` and
   `src/commit.rs:1122-1129`. `Envelope::decode` checks `body_len` only
   against the caller's single `max_body_len`; `decode_envelope` passes
   `ObjectKind::Delta.max_body_len()` (68 MiB) for kinds 1 **and** 3, so a
   kind-1 commit with a 68 MiB body is accepted where §9.1 allows 4 MiB.
   Fix: after parsing `kind`, `let max = max_body_len.min(kind.max_body_len())`
   (footer callers are unaffected: their tighter bound is `buf.len()`).
   Add a rejection test for a kind-1 envelope with `body_len` = 4 MiB + 1.
2. **major** — `tools/check-ownership.sh:34-41` (`from_branch`). The guess
   accepts only `<letter><digit?>` after the hyphen, so `p1/m1-b2a` and
   `p1/m1-b2b` yield `m1`, which has no entry: CI on those branches fails
   with "no entry for milestone 'm1'" (verified locally with
   `LUNIX_BRANCH`). `p1/m1-a`, `m1-a0`, `m1-b1`, `m5-map`, `m12-a`,
   `m13-a`, `m17-a`, `m18-b` guess correctly. Fix: allow
   `^[a-z][0-9]?[a-z]?$`, or better, try `first$rest` and fall back to
   `first` only when `first$rest` has no entry; add the thirteen branch
   names above to `tools/tests/run.sh`.
3. **minor** — `crates/lunix-format/src/journal.rs:402-405, 444-452`.
   In the resync path every 8-byte candidate whose magic matches is fully
   parsed, CRC over up to 1 MiB of data included, before `gen`/`off`/
   `write_seq` are compared. Guest data that imitates headers after one
   damaged record makes the lane scan quadratic (M3 recovery). Fix: in
   `valid_at`, read `gen` (bytes 40..48), `off` (48..56) and `write_seq`
   (8..16) from the slice and reject before `Record::parse`; same result,
   constant cost per false candidate. `rust-skills`: `perf-profile-first`
   does not apply; this is an input-dependent bound, not a hot path.
4. **minor** — `crates/lunix-format/src/commit.rs:570-588`
   (`Delta::records_decoded`). `zstd::bulk::decompress` is given
   `records_ulen` (≤ 64 MiB, read from the object) as capacity, so a
   100-byte DELTA can cost a 64 MiB allocation before the frame fails. The
   64 MiB cap is the spec's, but for parity with the footer path check the
   frame's declared content size equals `records_ulen` first (reuse
   `segment::frame_content_size`, made `pub(crate)`), so the allocation is
   bounded by what the frame claims and a lying frame is rejected before it.
5. **minor** — `crates/lunix-format/src/segment.rs:434, 550`. The tiling
   rule of §7.2 lives in `check_tiling(ovh)` and is not part of
   `decode_footer_frame`, which cannot know `OVH`. Readers (M7) can forget
   it. Fix: give `decode_footer_frame` a `CryptoMode` (or `ovh`) parameter
   and call `check_tiling` inside, or at least document on
   `decode_footer_frame` that `check_trailer` and `check_tiling` are
   mandatory follow-ups; the test harness (`f04_segment.rs:107-118`) already
   does both.
6. **minor** — `tools/check-ownership.sh:136-150`. The `deps: add <crate>
   =<version> for <m>` commit-title rule runs only in git mode, so the gate
   on the host (`--changed-list` / `.lunix-changed-files`) never enforces
   it; only the GitHub runner does. The report says so. Keep, but say it in
   the script header and in the plan's git-workflow section when it is next
   edited, and make the GitHub job required for merges.
7. **minor** — `tools/check-ownership.sh:73`: `shellcheck` SC2034 (`n`
   unused) is a warning; the plan asks for shellcheck-clean tools. Drop
   `n`. The remaining four infos (SC1003, SC1091, SC2016, SC2329) are
   fine; SC1091 wants a `# shellcheck source=/dev/null` on the `.env.host`
   line.
8. **minor** — `tools/remote.sh:163-175`. `df-guard` quotes the path with
   `%q`, so `~` is not expanded on the host (`df: '~': No such file`). Say
   "absolute path" in the usage line, or resolve with the existing `abs()`.
9. **minor** — `crates/lunix-format/src/local.rs:526, 695-707`. `VolumeJson`
   is `#[serde(default)]` (missing `version` reads as 1, choice 10) while
   `VolumesJson` requires `version`. Either is defensible; make them agree
   (I would require it in both: §21 says both "carry" it, and a hand-edited
   file missing it is more likely a mistake than an old file).
10. **minor** — `crates/lunix-format/src/commit.rs:757`. CONFIG enforces
    `policy_ver == 0` **iff** `policy_len == 0`; §10.5 only says `policy_ver`
    is 0 when `policy_len` is 0 (a non-empty policy with `policy_ver` 0 is
    not forbidden by the text). Sensible tightening, but it is an eleventh
    silent choice: list it in the report so the lead can confirm.
11. **minor** — `crates/lunix-format/src/mode.rs:179-187`.
    `ObjectKind::max_body_len` returns 64 MiB + 8 for `Footer` with a doc
    comment explaining why; fine, but once finding 1 is fixed this value is
    what bounds a footer envelope decoded outside `segment.rs`. Consider
    returning `u32::MAX` with the comment, or keep and note that
    `decode_footer_envelope` always tightens it.

Not findings, recorded: `.gitignore` covers `/target`, `/target-*`,
`.env.host`; all ten commits are signed (`%G?` = `U`: good signature,
unknown validity on this machine) and have no trailers; FREEZE.md text and
sign-off line unchanged, only the boxes ticked; `tools/remote.sh root`
signals the script with `TERM` through `timeout -s TERM -k 180` (≥ 120 s
grace, W8), never the daemon, writes the leftovers file on exit 137 and
refuses to start while it is non-empty (W9); `spikes/common/ublk-
leftovers.sh` exists; the `cargo tree -e normal,build` parser in
`check-features.sh` handles the `(*)` and `(path)` markers and empty
feature lists; `ownership.toml` matches every "Owns" line I compared (m1a0,
m1a with the skeleton files by name, m12a/m12b, m13a/m13b, m14/m17b
`fsck/src/gc.rs`, m17a/m17b, m15, m16, m18b; m1b2a/m1b2b both own all of
`lunix-testkit`, as the plan allows for sequential items).

## The ten silent layout choices

1. Journal `epoch` any value: **accept** (the table's "0 in phase 1" is a
   writer statement, like D7; readers in M3 may still warn).
2. Footer `n_dict_ids` zero iff no codec-3 entry, and ≤ their number:
   **accept** (implied by "distinct ids used by codec-3 extents, each once").
3. Leaf `clen` 0 or > 1 MiB rejected; footer `ulen` > 1 MiB (segment) or
   > 65 536 (map pack) rejected: **accept** (§6.5 and §3 bounds applied where
   the values are stored).
4. Hand-written raw zstd frame below 128 KiB, real zstd above: **accept**;
   §7.2 says "any level", the a0 bytes match the doc's raw frame, and the
   decoder accepts any frame. Note the footer *frame* is therefore not
   canonical (`encode(decode(frame))` may differ); the body is. Fine, since
   the MAC/AEAD covers the frame as stored.
5. v1 encoders refuse non-zero `lineage_idx`: **accept**; the hidden
   `*_allow_lineage` leaf codec keeps ADR-25 testable.
6. FULL `heat_seq` non-zero only with `has_heat`; `map_root.len` 0 with a
   pack rejected: **accept** (both follow from the table; a stored extent is
   never 0 bytes).
7. CONFIG `authority` must name an authority-role backend, empty only when
   detached with no backends: **accept**; this is §10.5 read literally.
8. Plain pin only with a zero fingerprint; "no pin" ⇒ zero mode and
   fingerprint: **accept** (§20 "zero unless a keyed mode is pinned").
9. Checkpoint with neither slot valid ⇒ two per-slot errors, caller ignores
   the file; equal `slot_seq` ⇒ slot 0: **accept** (§18; equal `slot_seq`
   cannot occur under the "rises per write" rule, so any tie-break is fine).
10. `volume.json`/`volumes.json` missing `version` read as 1: **accept with
    finding 9** (make the two files agree).

## Decoder robustness

`#![forbid(unsafe_code)]`; no `unwrap`/`expect`/`panic!` in `src/` (the
`expect` hits are `CommonHeader::expect`, a method). Every slice index I
found is behind a length check made on the same path (`Reader::take`,
`buf.get`, `checked_sub`, explicit `len != N` guards); `as` narrowing casts
are after range checks (`num-cast-try-from` conflict accepted below).
Counts go through `Reader::check_count` (maximum **and** remaining input)
before allocation; the only allocations not bounded by the input length
are the two compression paths (findings 4 and 5's `extents_len`, which is
sound when it comes from `split_container`). The leaf decoder proves
canonical form by re-encoding; `LeafView` is documented as not doing so.
The negative suite (every prefix, 10 000 mutations, allocation counter at
`2L + 64 KiB`, proptest 10 000 cases) runs on every golden; `f04`'s
`frame_cap_and_bombs` covers the zstd cap. `err-result-over-panic`,
`api-parse-dont-validate`, `type-newtype-ids`, `lint-cfg-check`,
`lint-workspace-lints`, `proj-msrv-declare`, `test-proptest-properties`:
followed.

## Workspace

Exact `=x.y.z` pins for every crate the plan lists (plus `hex`);
`replicix-chunkfmt` and `replicix-platform-lite` at `rev = "6fb59d2"`,
resolved in `Cargo.lock` to `6fb59d2a8b2c…`; toolchain and `rust-version`
1.98; edition 2021; `[profile.release] debug = 1`, `[profile.test]
opt-level = 1`; `lz4_flex`/`zstd` follow chunkfmt. Features declared only
where the plan says (`lunix-crypto/{test-rng,tripwire}`,
`lunix-iface/crashpoints`, `lunixd/crashpoints = ["lunix-iface/crashpoints"]`);
`lunix-testkit` appears only under `[dev-dependencies]`; `lunix-gc` has the
`lunix-map` dev-dependency. `lunix-format` depends on `zstd` (C through
`zstd-sys`) and `serde_json` with `float_roundtrip`; "pure Rust" holds for
the crate's own code. CI workflow: pinned toolchain via `rust-toolchain.toml`,
cache keyed on the lock file, ownership skipped off `p1/**`, `-j 4`; sane.

## rust-skills conflicts the implementer listed

- `lint-missing-docs` / `doc-all-public`: **accept**; the public items and
  most fields I read are documented. Suggest `#![warn(missing_docs)]` on
  `lunix-format` alone later, since it is the frozen public API.
- `num-cast-try-from` in encoders after the same field's range check:
  **accept**.
- `num-float-compare` (`PartialEq` on `VolumeJson` for tests;
  `validate()` uses ordered comparisons): **accept**.
- `api-non-exhaustive` only on `FormatError`: **accept**; a closed format
  enum is correct because a new value is a format change.

## Requests to the lead

- Land findings 1 and 2 before merging; 3-11 may follow in the same branch
  or as the lead's first `lunix-format` change (none alters a byte layout or
  a golden).
- Confirm choice 10's resolution (finding 9) and record choice 11
  (finding 10) in the report.
