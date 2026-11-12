# M1.B2a report: `lunix-testkit` part 1

- Milestone M1.B2a, branch `p1/m1-b2a`, base `41fc9aa` (origin/main unchanged at report time), 2026-11-12.
- Code commit `a13f904`; this report is the commit after it.
- Gate run through `tools/remote.sh run bash tools/ci.sh` with `LUNIX_CPUS=0-3`, `LUNIX_JOBS=4`,
  host dirs `lunix-p1/m1b2a/{src,target}`. Root not used.

## What was built (`crates/lunix-testkit`, all new files)

| item | file | notes |
|---|---|---|
| `PlainCodec` | `src/plain.rs`, `src/digest.rs` | `ExtentCodec + MetaCodec + CommitCodec`. `new()` = integrity mode with the fixed test key, `with_key`, `unkeyed()` = plain mode. D11 tag over every byte before it (keyed or unkeyed), real form 0 envelope + 32-byte MAC for metadata and commits (AAD from `meta_aad`, fixed test volume id), commit bound to `commits/<seq>`. |
| `FlatOverlay`, `coalesce` | `src/overlay.rs` | one entry per 4 KiB block, newest `write_seq` wins; `coalesce` merges pieces the trait allows merging, so M4 compares `coalesce(real) == coalesce(oracle)`. |
| `FlatMap` | `src/map.rs` | flat leaves of 512 chunks + one root node, sealed as real node extents through the `ExtentCodec` you pass; old roots stay readable; `with_dirty_leaf_cap` gives `MapFull`; `snapshot()` for oracles. |
| `MemPacks` | `src/packs.rs` | in-memory `PackStore` (needed by `FlatMap` and its contract fixture; B2b's `MemStore` can embed or replace it). |
| `MemSpace`, `MemSpaceConfig` | `src/space.rs` | ledger, FIFO waiters, headroom, atomic transfer, timeouts via `tick` on an injected `Clock`, ladder steps run in order on a helper thread, `reconfigure` applies and validates. |
| strategies | `src/strategies.rs` | `io_ops` (read, write, flush, FUA, discard, write-zeroes; 4 KiB..1 MiB; chunk-aligned, straddling and arbitrary 4 KiB-aligned offsets; optional invalid requests; `IoSpec::is_valid`, `payload`, `pattern`), `overlay_ops` (distinct shuffled `write_seq`), `map_ops` (commit, abort, get, seal, reopen, grow), `data_entry`. |

## Gate (all on the host, at `a13f904`)

| gate | result |
|---|---|
| `tools/ci.sh` (tool tests, fmt, clippy `-D warnings` workspace + `lunix-iface` with crashpoints, tests, feature edges, `lunixd` crashpoints build, ownership) | `ci: all green`; ownership `m1b2a ok (19 changed paths)` |
| `lunix-testkit` tests | 73 pass, 0 fail: 2 unit (digest), contract suites 41 (crypto 9 x 2 modes = 18, overlay 7, map 7, space 8, plus `reads_past_the_end_are_invalid` = 1), own tests 28 (plain_codec 8, flat_map 6, mem_space 7, strategies 7), model proptests 2 |
| contract suites | `crypto` (keyed and unkeyed `PlainCodec`), `overlay` (`FlatOverlay`, chunk sizes 4 and 16 blocks), `map` (`FlatMap` on `MemPacks`), `space` (`MemSpace`): all pass |
| self-consistency, 1 000 sequences of 100 ops | `FlatOverlay` vs a dense per-block model (lookups tile, `coalesce` keeps content and is idempotent, cuts grouped per chunk and frozen, release, `dirty_bytes`) and `FlatMap` vs a `BTreeMap` model (commit reports, abort, get/`try_get_cached`, seal, reseal is a no-op, reopen, grow, every historical root still readable, root hash equals that of a fresh map built from the same content): pass |
| mutation check (not committed) | a wrong `release` bound and a seal that ignores growth are each caught by the model tests |

The device oracle and the 10 000-op `MemBlockEngine` check of the M1.B2 gate are B2b's.

## Deviations and open choices

1. **Digest is not BLAKE3/HMAC.** `lunix-testkit`'s dependency list (M1.A) has no `blake3`, `hmac` or `sha2`, and
   dependency changes go through the lead. `src/digest.rs` is a small keyed 256-bit digest (not cryptographic, detects
   any single-byte change by construction). Consequence: `PlainCodec` bytes are not byte-identical to `lunix-crypto`'s
   plain/integrity extents (framing is identical), and chunk ids differ from real BLAKE3 ids. Fakes only need to agree
   with themselves. If you want real ids, a `deps: add blake3 =1.8.7 for m1b2a` commit is all it takes.
2. **Raw codec only.** There is no zstd/lz4 dependency, so `PlainCodec` can seal any codec and open it as
   `OpenOutput::Stored`, but `Plaintext` (and `verify_id` on a non-raw extent) return `Unsupported`. Tests of the
   engine over `PlainCodec` must use `CodecPolicy::None` until the lead adds `zstd`/`lz4_flex` (both are already in the workspace).
3. **`PlainCodec` has the same method names in `ExtentCodec` and `MetaCodec`** (`seal`, `open`), so a direct
   `codec.seal(..)` call is ambiguous; callers use the trait-qualified form or hold it as `Arc<dyn ExtentCodec>` etc.
4. `FlatMap::get` past `chunk_count` is `Invalid` (the trait only specifies `set`); `try_get_cached` never answers
   `Miss` (everything is resident). `FlatMap::open` takes the chunk count (an empty root does not record it) and
   rejects a root whose recorded count differs. Root hash covers size and leaf hashes only, so it will not equal
   `lunix-map`'s root hash for the same content: compare content, not roots. One root node caps a `FlatMap` at about
   1 100 non-empty leaves (`seal` fails `Invalid` beyond).
5. `FlatOverlay` returns one-block pieces from `lookup` and `cut`; the contract allows it, M4 compares through `coalesce`.
6. `MemSpace` spawns one short-lived thread per parked request to run the ladder (the real gate has one background
   thread); steps run in `LadderStepKind` order and a step that errs is skipped.
7. `MemSpaceConfig` is the testkit's own constructor argument; the contract's `SpaceSetup` is mapped onto it in the test.

## rust-skills conflicts

- `doc-all-public`/`lint-missing-docs`: public items and modules are documented; enum-variant fields in `strategies`
  are documented, `missing_docs` is still not enforced workspace-wide (as in M1.A).
- `err-no-unwrap-prod`: the library uses `expect` only for invariants (fixed-width slices, a leaf has at most 512
  entries); no `unwrap`.
- `unsafe-*`: none; `#![forbid(unsafe_code)]`.
- Plan/CONVENTIONS versus the implementer rules: CONVENTIONS ground rule 1 says not to commit; the milestone rules
  require signed commits on the branch, which I followed.

## Commits

- `a13f904` M1.B2a: lunix-testkit part 1 (PlainCodec, FlatOverlay, FlatMap, MemSpace, strategies)
- the commit adding this report

## Remote disk use

`~/lunix-p1/m1b2a` is 4.6 GiB in total (target dir included), under the 10 GiB budget.
