# lunix-iface

Traits and shared types between Lunix crates. No implementation lives here and
no implementation crate is a dependency. Rustdoc is the contract: every trait
item says which thread may call it and whether it blocks (queue-thread /
worker / control).

- `block`, `journal`, `overlay`, `map`, `index`, `store`, `space`, `crypto`,
  `gc`: the seams (see the table in `lib.rs`).
- `ctl`: control protocol, `u32` big-endian length then one JSON object
  `{"v":1,"id":N,"method":..,"params":..}`, 1 MiB cap, `Secret` redacted.
- `crashpoint!("crate.step")`: nothing unless feature `crashpoints`; then
  `LUNIX_CRASH_AT=<name>[:<hit>]` sends `SIGKILL` at that hit,
  `LUNIX_CRASH_COUNT=<file>` + `dump_counts()` reports hit counts.
- `contract::{journal, overlay, map, index, store, space}!(make)`: suites each
  fake and real implementation runs; fixtures are the `*Fixture` traits.

## Space accounting walk-through (full text: `space` module docs)

1. admission: `reserve(JournalReserved, record_bytes)` (may park or refuse)
2. C2 cut: `reserve_headroom(Segments, ulen_bound)` and
   `reserve_headroom(Headroom, map+commit+index bounds)`
3. C4 segments sealed: `transfer(Segments, Segments, bound, actual)`
4. C5/C6/C8: `transfer(Headroom, Map|Commits|Index, bound, actual)`
5. C9: `release(JournalReserved, reclaimed)`, `release(Headroom, remainder)`
6. GC delete: `release(kind, bytes)` after the delete is durable
7. compaction output: like a cut's segments

Exact outside C2..C9, conservative inside. `transfer` is atomic and never parks.

Frozen after M1: additive changes only (new item with a default), through the lead.
