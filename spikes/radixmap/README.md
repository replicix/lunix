# radixmap: P0.7 radix Merkle block-map prototype (ADR-8)

Measures seal cost, bytes written, diff, cold open and resident memory of the
fanout-512 Merkle block map at 1 and 10 TiB (64 KiB chunks), against a flat-table
baseline. Throwaway measurement code; not a product format.

- `src/leaf.rs` leaf codecs (run-encoded bit-packed `rb16`, plain rows), derived from `../leafenc`.
- `src/gen.rs` verbatim copy of leafenc's generators; `src/model.rs` deterministic random-access volume.
- `src/map.rs` overlay, seal, lazy open, diff; `src/store.rs` packs, LRU, simulated-latency GETs.
- `src/flat.rs` flat-table baseline (full seal, block-delta seal, diff).
- `src/tests.rs` unit tests (history independence, old root readable, diff vs brute force, sharing).

Reproduce (on the test host, never the authoring machine; uses CPUs 8-15, ~1 GiB disk,
up to ~3 GiB RSS):

    cargo test --release && bash run.sh      # CSVs in ./out
    python3 summarize.py out                 # medians tables used in RESULTS.md

Prerequisites: Rust 1.96, network for crates.io on first build. Dependencies are pinned
exactly in `Cargo.toml` (blake3 =1.8.7, lz4_flex =0.11.6, zstd =0.13.4). No
`rust-toolchain.toml`: the host has 1.96 and Constellation's 1.98 channel pin would force a download.
Results: `../../bench/radixmap/RESULTS.md`, raw CSVs in `../../bench/radixmap/raw/`.
