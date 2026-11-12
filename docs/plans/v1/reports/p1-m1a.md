# M1.A report: workspace, tools, `lunix-format`

- Milestone M1.A, branch `p1/m1-a`, base `3bab655`, last code commit `f090ee2`
  (this report is the commit after it), 2026-11-12.
- Host: Linux 7.0.0 (aws), 16 CPUs, rustc 1.98.1 (toolchain pinned 1.98), root not used.
- The gate ran with `LUNIX_HOST_SRC=lunix-p1/m1-a`, `LUNIX_HOST_TARGET=lunix-p1/target-m1a`,
  `LUNIX_JOBS=8`, `LUNIX_CPUS=0-7` through `tools/remote.sh run bash tools/ci.sh`.

## What was built

- Root workspace: `Cargo.toml` (17 crates, exact `=x.y.z` pins resolved 2026-11-12,
  `replicix-chunkfmt` / `replicix-platform-lite` by git rev `6fb59d2`), `Cargo.lock`,
  `rust-toolchain.toml` 1.98, `.gitignore`, `[profile.release] debug = 1`,
  `[profile.test] opt-level = 1`.
- Skeletons with final dependency lists for the other 16 crates (`lunix-gc` has the `lunix-map`
  dev-dependency; features `lunix-crypto/{test-rng,tripwire}`, `lunix-iface/crashpoints`,
  `lunixd/crashpoints`; binaries `lunixd`, `lunix`, `lunix-fsck`, `lunix-ublk-testd` are empty `main`s).
- `.github/workflows/ci.yml` runs `tools/ci.sh` on a hosted runner (no root).
- `tools/`: `remote.sh` (sync, cargo, run, fmt, pull, root, leftovers, df, df-guard; host from
  `LUNIX_HOST`, never printed or stored), `ci.sh`, `check-ownership.sh` + `ownership.toml`,
  `check-features.sh` (the feature-edge check), `tests/run.sh` with fixtures (16 checks).
- `lunix-format`: all 15 freeze items with strict encode/decode, 68 golden files (`.bin` + `.hex`
  or text), the KAT file copied to `tests/vectors/`, and the shared test harness
  (`tests/common/mod.rs`: golden compare/regenerate, truncation, 10 000 mutations with canonical
  re-encode, counting allocator asserting `<= 2L + 64 KiB`).

## Gate (all on the host, at `f090ee2`)

| gate | command | result |
|---|---|---|
| fmt | `cargo fmt --all --check` (inside `tools/ci.sh`) | pass |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| tests | `cargo test --workspace` | pass: 157 tests in `lunix-format` (f01 10, f02 13, f03 10, f04 12, f05 11, f06 26, f07 22, f08 9, f09 8, f10 7, f11 4, f12 4, f13 4, f14 10, f15 7), 0 failed; other crates have no tests yet |
| all 17 crates compile | same run | pass |
| tool tests | `bash tools/tests/run.sh` | 16 ok (ownership synthetic diffs, feature-edge fixtures) |
| feature edges | `tools/check-features.sh`, `cargo build -p lunixd --features crashpoints` | pass |
| ownership | `tools/check-ownership.sh` | `m1a ok (220 changed paths)` |
| KAT | `aad_hardened`, `aad_fips_kdf_zero_nonce`, `info_v2`, `slot_aad_pass`, `purpose_*` | reproduced byte for byte |
| negative / allocation | every golden: all strict prefixes fail, 10 000 mutations never panic and only canonical inputs are accepted, allocation bound | pass |
| proptest | 10 000 cases per type (`common::CASES`) | pass |

`ci.sh` refuses to run unless `LUNIX_ON_HOST=1` (set by `remote.sh`), so it cannot compile on a workstation by accident.

## FREEZE items ticked

All 15 (1 primitives, 2 journal, 3 extent, 4 segment/map pack, 5 map node, 6 commits, 7 envelope/pins/opaque,
8 volume, 9 keyring, 10 lease, 11 purposes, 12 paths, 13 index run, 14 local files, 15 GC files).
Every `a0-*` golden equals the worked example of section 23 byte for byte, with the doc's hex as an
independent literal in the test and the MACs, hashes and CRCs recomputed. FREEZE.md text and the
sign-off line are unchanged (only the checkboxes).

## Layout versus spec

No case where the signed-off layout could not be implemented, and none where it was changed. Points the
layout leaves open, resolved without changing bytes (the lead may want to confirm):

1. Journal record decoder accepts any `epoch` (the table says "0 in phase 1"); a later phase uses it.
2. Footer `n_dict_ids`: zero exactly when no entry has codec 3, and at most the number of codec-3 entries.
3. Leaf entries: `clen` 0 or above 1 MiB rejected; footer `ulen` above 1 MiB (segment) or 65 536 (map pack) rejected.
4. `Raw` (hand-written) zstd frames only for footer bodies under 128 KiB; real zstd is used above that.
5. The v1 encoders also refuse a non-zero `lineage_idx` (the decoders reject it as the layout says).
6. FULL: `heat_seq` non-zero only with `has_heat`; `map_root.len` 0 with a pack is rejected.
7. CONFIG: `authority` must name an authority-role backend, empty only when detached with no backends.
8. Plain pin only with a zero fingerprint; a "no pin" record must have zero mode and fingerprint.
9. Checkpoint with neither slot valid decodes as two per-slot errors (caller ignores the file); equal `slot_seq` picks slot 0.
10. `volume.json` / `volumes.json`: a missing `version` is read as 1 (one-line change if it should be required).

Plan versus layout: plan text (D11, freeze item 3) and the doc agree on the tag covering `clen`; D10 says `seq`
where the doc says `commit_seq` (the doc wins, as D10's note says).

## Decisions taken where the plan was silent

- `lunix-format` takes hashes and MACs from the caller (closures or the exact input bytes) to keep
  "no crypto operations"; the tests use `blake3`, `hmac`, `sha2` as dev-dependencies. `zstd` and `crc32c`
  are normal dependencies (footer frames, CRCs); `serde_json` for `volume.json` (feature `float_roundtrip`).
- `lz4_flex =0.13.1` and `zstd =0.13.3` follow replicix-chunkfmt's pins so one copy is built.
- Cargo license `MPL-2.0` (repository LICENSE).
- `ownership.toml`: `m1a` lists the skeleton files by name so later milestones are not blocked by it;
  `m1b2a` and `m1b2b` both list `crates/lunix-testkit/**` (they run one after the other).
- `check-ownership.sh` also enforces the `deps: add <crate> =<version> for <m>` commit rule for
  `Cargo.toml` / `Cargo.lock` outside m1a, and falls back to `.lunix-changed-files` when there is no usable git
  checkout (a worktree's `.git` file is useless on the host). `tools/remote.sh sync` writes that file. A raw
  `rsync --delete` removes it, so run the gate through `tools/remote.sh`.
- `remote.sh` accepts `LUNIX_HOST_SRC`, `LUNIX_HOST_TARGET`, `LUNIX_CPUS`, `LUNIX_OVERLAY` overrides, so the
  lead's brief paths (`lunix-p1/m1-a`, `target-m1a`) work next to the plan's `<base>/<m>/{src,target}` default.

## Requests to the lead

- A5 type checkpoint: all types are complete in `f090ee2`; B1 may start from it (no separate earlier checkpoint was announced).
- `shellcheck` is not installed on the host, so "shellcheck-clean" is not verified (all scripts pass `bash -n`).

## rust-skills conflicts

- `lint-missing-docs` / `doc-all-public`: `missing_docs` is not enforced; types, functions and modules carry
  short docs, struct fields mostly do not (would fail `-D warnings` on ~1000 fields).
- `num-cast-try-from`: narrowing `as` casts remain in encoders after the range check of the same field
  (for example `len() as u16` behind a `<= 64` check); decoders use checked conversions.
- `num-float-compare`: `VolumeJson` derives `PartialEq` over `f64` fields (`gc_live_threshold`), needed for round-trip tests.
- `api-non-exhaustive`: only `FormatError` is `#[non_exhaustive]`; format enums are closed on purpose (a new value is a format change).

## Skipped / deferred

- `unsafe-miri-ci`: no unsafe in `lunix-format` (`#![forbid(unsafe_code)]`); only the test allocator wrapper uses it.
- Fuzz harnesses are phase 8; the negative and allocation tests stand in.

## Remote state

`~/lunix-p1/target-m1a` is 3.4 GiB (may stay). Source copy `~/lunix-p1/m1-a`; scratch `~/lunix-p1/m1a`.
Fork build directories were removed.
