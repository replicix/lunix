# P0.6 trace-driven simulation: chunk size (ADR-5) and cache policy (ADR-14)

Date: 2026-11-11. Status: **partial. Simulators implemented and unit-tested; the measurement is NOT RUN
because no block trace could be obtained** (see TRACES.md). No gate was evaluated. ADR-5 and ADR-14
stay *proposed*. Nothing in this file is a measured result about 16/64/256 KiB or about S3-FIFO vs SIEVE vs W-TinyLFU.

## Host
AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root.
Kernel 7.0.0 (Ubuntu), Ubuntu 26.04.1 LTS. rustc/cargo 1.96.0. Builds and tests
ran under `taskset -c 8-15`, `cargo build --release -j 4`. Spike revision: uncommitted. No crates (std only).

## What exists
- `spikes/tracesim` (Rust, std only): readers for MSR / Alibaba / Tencent-CBS layouts (defensive, `.gz/.xz/.zst`
  streamed through the external tool), P0.6a chunk simulator, P0.6b four-policy cache simulator,
  and `gate`, which applies the plan's fixed rules to the CSVs.
- `cargo test --release`: 10 tests pass: offset math and depth table (8 TiB = 3 levels, one more chunk = 4),
  hand-computed 10-request fixture (writes 8, partial 7, partial-to-written 4, partial-to-new 3, cold fetches 2,
  two cuts with 98 304 B data and 49 152 B of 512-fanout leaves at ratio 1.0), determinism (identical CSV rows on rerun),
  LRU/SIEVE exact sequences, S3-FIFO and W-TinyLFU scan survival vs LRU, capacity respected for all four policies.
- A CLI smoke run on `tests/msr_format_smoke.csv` (40 made-up lines) ran `chunk`, `cache` and `gate` end to end; its numbers mean nothing and are not kept.

## Modelling choices (where the plan was ambiguous; fixed in code)
1. Cold RMW: the model caches (infinite, LRU 1 %, LRU 10 % of the touched-chunk working set) are filled by reads and by
   RMW fetches only; full or partial writes do not insert. Otherwise the infinite cache would never miss on a previously
   written chunk and the "every previously written chunk is fetched once" lower bound would be 0. A partial write counts as
   an RMW candidate only if that chunk was written earlier in the trace; partial writes to other chunks are counted
   separately as zero-fill (pre-existing data of the volume is unknowable).
2. Commit cuts every T of trace clock from the volume's first request; the last partial interval is also cut.
   Data uploaded per cut = distinct dirty chunks x C (x 0.5); map sealed = dirty leaves x F x 48 B (x 0.5 for leaves only) +
   path interior nodes x F x 48 B (uncompressed); depth from the highest touched chunk index. Map write amplification =
   map bytes / data bytes with the same ratio. Fanouts 256/512/1024 are computed in every run.
3. S3(C) for the gate = (data_r05 + map_r05) per logical GiB written at T = 60 s, fanout 512, per volume, then p50/p90 across volumes.
   RMW(C) uses the LRU-10 % column. MAP(C) uses MiB per TiB touched at ratio 0.5. Map-WA condition uses the p90 over cuts per
   volume, then the p90 across volumes.
4. Cache replay: reads are the measured accesses (request miss ratio per chunk access, byte miss ratio by bytes inside the chunk).
   Writes are pinned dirty until the next 60 s cut, reads of dirty chunks are overlay hits, at the cut dirty chunks enter the clean
   cache as an access. Sizes are per mille of distinct touched chunks (5, 10, 20, 50, 100, 250), minimum 16 chunks. Scan test: 2x cache
   capacity of never-reused read chunks injected at the midpoint of the 10 volumes with most requests; statistics cover original accesses only.
5. W-TinyLFU: 1 % window, SLRU 80 % protected, 4 x 4-bit count-min sketch with a doorkeeper, reset every 10 x capacity records, candidate wins only
   if its estimate is strictly greater (Caffeine's random jitter omitted for determinism). S3-FIFO per the paper's pseudocode (promotion at freq > 1, reinsertion in main at freq > 0, ghost = main size).
6. Records are processed per invocation merged by volume in memory; MSR per server file fits easily, the Alibaba 751 GB archive would need a per-device
   splitter first (not written).

## Not run (and why)
| item | reason |
|---|---|
| P0.6a on MSR (mandatory), Alibaba, Tencent CBS | no trace obtainable: SNIA IOTTA needs a JS proof-of-work challenge and licence acceptance, Alibaba needs a survey (TRACES.md) |
| ADR-5 gate (16 / 64 / 256 KiB, 1 MiB reference; T = 10/60/300 s) | depends on the above |
| ADR-14 gate (S3-FIFO vs SIEVE vs W-TinyLFU vs LRU, scan test, 16 KiB check) | depends on the above |
| libCacheSim cross-check on 3 volumes | needs real volumes; libCacheSim not built |
| segment-granular eviction stretch, ARC/2Q, plots | cut; report-only items, no data |
| Tencent CBS column layout | UNVERIFIED (recalled), parser untested on real data |

Time used: well inside the two-day box; what was cut is everything that needs data.

## Gate status
- ADR-5: *pending* (not run). Rule restated: 64 KiB confirmed iff at p50 and p90 S3(64) <= 1.25 x min S3(C), RMW(64) <= 2 x RMW(16), map WA p90 <= 25 %.
- ADR-14: *pending* (not run). Rule restated: S3-FIFO accepted iff at 1/5/10 % sizes its median miss ratio <= 1.05 x best, <= LRU, and it loses to LRU on < 25 % of volumes, and no scan failure (> 2 % abs worse than LRU).

## Proposed decision record
Do not change ADR-5 or ADR-14. Suggested gate-outcome blocks (2026-11-11):

> **ADR-5 gate outcome: not run (pending).** The P0.6 simulator is built and tested (`spikes/tracesim`), but the public traces
> (MSR Cambridge, Tencent CBS via SNIA IOTTA; Alibaba 2020) are behind a bot-challenge/licence-acceptance/survey step that an
> unattended executor must not pass. 64 KiB remains the proposed default. To close: a person downloads the traces to the test host
> and runs `spikes/tracesim/run.sh`; the fixed rules in the plan then decide.
>
> **ADR-14 gate outcome: not run (pending).** Same reason; the four policies are implemented and unit-tested; S3-FIFO remains proposed.

Closing the gate needs: MSR (about 5 GB compressed, 36 volumes) is enough for ADR-5 plus the scan test; add Alibaba/Tencent subsets of >= 50 volumes
(25 most writes plus 25 seeded random) to meet the plan. Disk need on the host is under the 10 GiB budget for MSR streamed through `xz/gzip -dc`.
