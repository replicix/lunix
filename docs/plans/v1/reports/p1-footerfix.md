# p1-footerfix: the compactor's footer cache was 8 bytes

## Bug

dcaebdf (G1) changed `FooterCache::new` from a count of segments to a byte cap. `compact.rs` still called
`FooterCache::new(8)`, now 8 bytes: the compactor cached about one footer. `refill` sorts the queue by chunk, so
consecutive extents come from different sources and nearly every `copy` reloaded, authenticated and decoded a footer
(about 6x slower compaction per unit at 16K; analysis in bisect-16k-compaction.md).

## Change

* `footers.rs`: new `CacheBytes(usize)` newtype; `FooterCache::new(CacheBytes)`. A bare literal no longer compiles.
  Test call sites in footers.rs updated.
* `compact.rs`: `Compactor::new` takes a `CacheBytes` for its footer cache (`FooterCache::new(8)` is gone).
* `collector.rs`: both `Compactor::new` call sites pass `CacheBytes::new(self.cfg.footer_cache_bytes)` (32 MiB default).
* `mark.rs`: converted to `CacheBytes::new(cfg.footer_cache_bytes)`.
* `tests/footercache.rs` (new): a segment store wrapper counts `footer` calls while a `Compactor` copies 6 interleaved
  sources (source k holds chunks k, k+6, ...). With the config cap: loads <= sources + 1. A second test checks that a cap
  of 8 bytes reloads more than 4 times per source.
* `docs/reference/gc.md`: one clause on the footer cache (mark and compactor each hold one of that size).
* `tools/ownership.toml`: `[footerfix]`.

## Proof the test bites

With `FooterCache::new(CacheBytes::new(8))` temporarily in `compact.rs`: `compaction_loads_about_one_footer_per_source`
FAILED ("48 footer loads for 6 sources"); restored afterwards (diff has no `new(8)`).

## Gate

* `tools/ci.sh`: all green (258 `test result: ok` lines), CPUs 12-15, -j 4.
* `gc_kill_loop_plain_16k`: pass, 24 points (23 reached; `gc.recheck.before_delete` is not reached by the regular-pass
  arm, as before), 68 runs = 68 kills, 5 045 s, at-rest fsck clean.
* `gc_kill_loop_integrity_16k`: pass, 24 (23), 68 kills, 4 838 s, at-rest fsck clean.
  Both: release, `LUNIX_KILL_HITS=all`, `crashpoints`, `lunix-fsck` release, run concurrently as two processes.
* No performance measurements (as briefed).

## Deviations, conflicts

None. No rust-skills conflicts (rule applied: `api-newtype-safety`). No format change, no dependency change.
