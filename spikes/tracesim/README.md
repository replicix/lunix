# tracesim (P0.6)

Trace-driven simulators for ADR-5 (chunk size) and ADR-14 (cache policy).
Std-only Rust, no dependencies. Build and run on the test host only.

    cargo build --release -j 4
    cargo test --release
    # needs real block traces; none could be fetched in the P0.6 session
    # (see bench/tracesim/TRACES.md):
    ./run.sh /var/tmp/lunix-traces out_dir

Commands:
- `tracesim chunk --set S --fmt msr|alibaba|tencent --out DIR FILES...`
  writes `chunk_rmw|chunk_map|chunk_commit.S.csv` (P0.6a).
- `tracesim cache ... [--chunk 65536]` writes `cache_results|cache_scan.S.csv`
  (LRU, S3-FIFO, SIEVE, W-TinyLFU; P0.6b).
- `tracesim gate DIR` applies the plan's fixed ADR-5/ADR-14 rules to the CSVs.

Inputs may be `.gz/.xz/.zst` (decompressed on the fly through the tool, no
shell). All files of one invocation are merged per volume in memory.
Modelling choices are listed in bench/tracesim/RESULTS.md. The Tencent CBS
column layout is from memory (UNVERIFIED). Unit tests cover offset math, a
hand-computed 10-request fixture, policy behaviour and determinism.
