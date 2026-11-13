# lunix-engine

The block engine of a Lunix volume (plan M11): the ublk request handlers,
flush groups, the cut and commit protocol C1-C10, and startup recovery.
It implements `lunix_iface::block::BlockEngine` over the `lunix-iface`
traits only; `lunixd` (M12) wires the real crates in through `EngineDeps`.

| module | what |
|---|---|
| `queue.rs` | one queue's `QueueHandler`: write path (space, `try_reserve` + in-flight registration, `append`, overlay insert on the `Append` completion), reads (overlay, map, cache, helpers), FLUSH/FUA, DISCARD/WRITE_ZEROES, parking |
| `flush.rs` | flush groups: `upto = last_issued()` at formation, one group in flight |
| `coord.rs` | C1-C10, the commit triggers (`T`, `B`, journal fill, a full ring, pending `moved`), the GC hook |
| `recover.rs` | `Engine::open`: chain verification (no fallback), R4, orphan sweep and allocator floor, index catch-up, map, checkpoint hint, journal recovery and overlay rebuild, floor raise, ledger |
| `read.rs` | the R1 reader (the only `read_extent` call), the stale-hint fallback, the read helper pool |
| `destage.rs` | the `Destager` seam to M9 and `SimpleDestager`, the in-crate stand-in |
| `cache.rs` | the decoded-chunk CLOCK cache |

Three seams the frozen iface does not have are engine traits:
`MapOpener` (the map's constructor), `RollbackFloorStore` (the `state/pins`
v2 floor, which `VolumeDir` cannot express yet) and `EngineHooks` (test
crash points, sequence observation, the queue-thread marker).

Tests run against the `lunix-testkit` fakes: `tests/model.rs` is the
crash-consistency model (`LUNIX_MODEL_CASES=500` for the gate size).
