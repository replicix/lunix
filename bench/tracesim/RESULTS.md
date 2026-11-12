# P0.6 trace-driven simulation: chunk size (ADR-5) and cache policy (ADR-14)

Date: 2026-11-12. Status: **done for MSR Cambridge only** (Alibaba and Tencent CBS: not run, see TRACES.md).
Trace source: MSR Cambridge block traces (SNIA IOTTA), provided by the maintainer. All 36 files were streamed
(`gzip -dc`, nothing decompressed to disk); 27 volumes have >= 1 GiB written and were simulated, 9 were dropped by
the plan's rule (hm_1, rsrch_1, rsrch_2, src2_1, wdev_1, wdev_3, web_1, web_2, web_3). Raw summaries: `raw/`.

## Verdicts (plan's fixed rules, thresholds unchanged)
- **ADR-5: amend the default to 16 KiB** (profile for bulk volumes stays 64 KiB). At T = 60 s, ratio 0.5: S3(64) = 1.30 (p50) /
  1.95 (p90) per logical GiB against S3(16) = 0.76 / 1.05, so the "64 KiB confirmed" clause fails (needs <= 1.25 x min) and the
  16 KiB clause (S3(16) <= 0.75 x S3(64) at p50 and p90: 0.756 <= 0.975 and 1.051 <= 1.464) holds. T = 10 s and 300 s agree in sign.
  RMW(64) <= 2 x RMW(16) holds, map write amplification p90 at 64 KiB does not (68.8 % across volumes, limit 25 %).
  MAP(16)/MAP(64) = 3.9x (limit 6x, not a fail).
- **ADR-14: S3-FIFO is NOT accepted (rule failed), and no amendment margin is met.** Two independent failures of the accept rule:
  (a) at 1 % of the working set S3-FIFO loses to LRU on 33 % of volumes (limit < 25 %); (b) the scan rule: two scan tests
  (mds_1 at 0.5 % and 1 % cache) have S3-FIFO worse than LRU by 5.3 and 5.1 points absolute (limit 2). SIEVE is within 5 % of S3-FIFO at
  the three sizes (0.989x at 5 %, 0.974x at 10 %, 1.036x at 1 %), so the SIEVE amendment (<= 0.95x at two sizes) is not met; W-TinyLFU is worse than
  S3-FIFO at every size up to 10 %. Net: ADR-14 stays *proposed*; the data do not select another policy. The plan's
  literal reading of the scan rule is per test; the mean over the nine scan volumes is better than LRU at every size (informational lines below).

## Host and setup
AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root. Kernel 7.0.0 (Ubuntu 26.04.1 LTS).
rustc/cargo 1.96.0, std only. Spike revision: uncommitted. Run: `spikes/tracesim/run-msr.sh <trace dir> <out dir> 2`
(one process per volume file, 2 in parallel, `taskset -c 8-15`, `cargo build -j 4`), then `tracesim gate`. Wall time about 40 minutes
(sum of per-file times 4 239 s over 2 workers; the five largest files took 416-935 s each; peak RSS 9.7 GB for the 2.07 GB prxy_1 file).
The plan's two-day box was not an issue; nothing was cut apart from the items under "Not run".

Scan volumes (rule: files ranked by compressed size, ranks 1, 4, 7, ... 28): prxy_1, src1_0, prxy_0, proj_4, prn_0, usr_0, src1_2, mds_1,
mds_0, src2_1. src2_1 was dropped by the 1 GiB filter, so the scan test ran on 9 volumes.

## P0.6a numbers (all 27 volumes; percentiles are across volumes; raw per-volume rows in `raw/chunk_*.csv`)
S3 = (data uploaded + map sealed) per logical GiB written, ratio 0.5 on data and on leaves, fanout 512. RMW = cold-RMW fetches per 1000 writes with the 10 % LRU
model cache. MAP = MiB per TiB touched (ratio 0.5). mapWA = map bytes sealed / data bytes uploaded per cut, p90 over cuts, then percentile over volumes.

| T(s) | C(KiB) | S3 p50 | S3 p90 | S3 mean | RMW/1000w p50 | RMW p90 | MAP MiB/TiB touched p50 | mapWA p90-of-cuts: p50 vols | p90 vols |
|---|---|---|---|---|---|---|---|---|---|
| 10 | 16 | 1.0781 | 1.7340 | 1.1502 | 71.245 | 482.089 | 2627 | 2.0455 | 2.5000 |
| 10 | 64 | 1.7896 | 2.9081 | 1.8419 | 57.937 | 372.736 | 671 | 0.7500 | 0.9375 |
| 10 | 256 | 4.1095 | 8.3195 | 4.6510 | 22.104 | 156.043 | 183 | 0.1250 | 0.2500 |
| 10 | 1024 | 12.8126 | 28.8049 | 14.8004 | 13.953 | 46.485 | 51 | 0.0312 | 0.0586 |
| 60 | 16 | 0.7556 | 1.0507 | 0.7856 | 71.245 | 482.089 | 2627 | 1.0714 | 1.9286 |
| 60 | 64 | 1.2999 | 1.9518 | 1.3071 | 57.937 | 372.736 | 671 | 0.2875 | 0.6875 |
| 60 | 256 | 3.0280 | 5.5678 | 3.2256 | 22.104 | 156.043 | 183 | 0.0600 | 0.1875 |
| 60 | 1024 | 7.4992 | 19.8087 | 9.6734 | 13.953 | 46.485 | 51 | 0.0165 | 0.0469 |
| 300 | 16 | 0.5352 | 0.7997 | 0.5419 | 71.245 | 482.089 | 2627 | 0.7692 | 1.3184 |
| 300 | 64 | 0.8452 | 1.4103 | 0.8756 | 57.937 | 372.736 | 671 | 0.2171 | 0.6250 |
| 300 | 256 | 2.0808 | 3.0037 | 1.9647 | 22.104 | 156.043 | 183 | 0.0511 | 0.1688 |
| 300 | 1024 | 5.6182 | 8.5691 | 5.3353 | 13.953 | 46.485 | 51 | 0.0120 | 0.0352 |

Informational only (NOT the plan's rule): S3(C) at T=60 s with the map bytes scaled by 0.46 (DESIGN's run-encoded leaves, 352/768 MiB per TiB):
  C=16 KiB: S3 p50 0.6597 p90 0.8330
  C=64 KiB: S3 p50 1.2155 p90 1.7746
  C=256 KiB: S3 p50 2.9917 p90 5.4409
  C=1024 KiB: S3 p50 7.4655 p90 19.7324

(MAP and RMW do not depend on T.) Reading: S3 falls with smaller chunks because random small writes dirty whole chunks; the price is the map. At 16 KiB the sealed
map is on median larger than the data uploaded in the same interval (mapWA > 100 % at T = 10 s and 60 s), a result the plan's rule does not penalise but that matters for the decision (see caveats).

## P0.6b numbers (64 KiB chunks, 27 volumes; 16 KiB check below; raw `raw/cache_results.csv`, `raw/cache_scan.csv`)
Median request miss ratio over volumes, cache size as per mille of each volume's distinct touched chunks:



The rule's sizes are 10 (1 %), 50 (5 %) and 100 (10 %). Result of the rule: S3-FIFO <= 1.05 x best at all three (1.00, 1.01, 1.03) and <= LRU at all three, but
loses to LRU on 33 % of volumes at 1 % (limit 25 %). With a materiality margin the picture is milder (informational): worse than LRU by more than 0.005 on 22 % of volumes at 1 %,
11 % at 5 %, 4 % at 10 %.

## Caveats the lead should weigh (not applied to the verdicts)
1. S3 charges full chunks at the 48 B-entry map of the plan. DESIGN/ADR-8 now uses run-encoded 16 B-id leaves (about 0.46 of the 48 B size). Scaling map bytes by 0.46 gives
   S3 p50/p90 of 0.66/0.83 (16 KiB), 1.22/1.77 (64 KiB), 2.99/5.44 (256 KiB): the ordering and the 16 KiB result do not change.
2. The data side, S3(C), is dominated by MSR's many small random writes; these are 2007 enterprise server volumes, no VM-image or database profile. The 16 KiB gain
   comes largely from write amplification of small writes; sequential/bulk volumes prefer larger chunks (p90 of S3 is worse at 64 KiB than the median suggests).
3. The cut model is the plan's (a cut uploads every dirty chunk; no overwrite coalescing across cuts, no dedup, no content). With ratio 1.0 data the 16 KiB advantage is the same in sign
   (computed from `data_r1`, `map_r1` in `raw/chunk_commit.csv`: S3 p50/p90 = 1.45/2.02 at 16 KiB, 2.58/3.86 at 64 KiB, 6.03/11.1 at 256 KiB; the 0.75x test also holds).
4. mapWA at 64 KiB exceeds 25 % in the plan's model because each dirty chunk dirties a 24 KiB leaf (fanout 512 x 48 B) at 0.5; this is the leaf-seal cost the ADR-8 run-encoded
   leaves and map-delta records are meant to reduce. Under the plan's own rule this alone prevents "64 KiB confirmed" even with S3 within margin.
5. libCacheSim cross-check not run (it would need a download, which was not allowed). The policy implementations are checked only by unit tests (exact LRU and SIEVE sequences, scan
   survival, capacity bounds). Treat small differences between policies (a few percent) accordingly.
6. 16 KiB cache check (informational, median miss ratio): at 1 % LRU 0.666, S3-FIFO 0.641, SIEVE 0.638, W-TinyLFU 0.692; at 5 % 0.569 / 0.520 / 0.544 / 0.561; at 10 % 0.525 / 0.489 / 0.478 / 0.514.
   Same ordering as at 64 KiB: S3-FIFO and SIEVE close, both ahead of LRU and W-TinyLFU.

## Full gate output
```
volumes: 27

| T(s) | C(KiB) | S3 p50 | S3 p90 | S3 mean | RMW/1000w p50 | RMW p90 | MAP MiB/TiB touched p50 | mapWA p90-of-cuts: p50 vols | p90 vols |
|---|---|---|---|---|---|---|---|---|---|
| 10 | 16 | 1.0781 | 1.7340 | 1.1502 | 71.245 | 482.089 | 2627 | 2.0455 | 2.5000 |
| 10 | 64 | 1.7896 | 2.9081 | 1.8419 | 57.937 | 372.736 | 671 | 0.7500 | 0.9375 |
| 10 | 256 | 4.1095 | 8.3195 | 4.6510 | 22.104 | 156.043 | 183 | 0.1250 | 0.2500 |
| 10 | 1024 | 12.8126 | 28.8049 | 14.8004 | 13.953 | 46.485 | 51 | 0.0312 | 0.0586 |
| 60 | 16 | 0.7556 | 1.0507 | 0.7856 | 71.245 | 482.089 | 2627 | 1.0714 | 1.9286 |
| 60 | 64 | 1.2999 | 1.9518 | 1.3071 | 57.937 | 372.736 | 671 | 0.2875 | 0.6875 |
| 60 | 256 | 3.0280 | 5.5678 | 3.2256 | 22.104 | 156.043 | 183 | 0.0600 | 0.1875 |
| 60 | 1024 | 7.4992 | 19.8087 | 9.6734 | 13.953 | 46.485 | 51 | 0.0165 | 0.0469 |
| 300 | 16 | 0.5352 | 0.7997 | 0.5419 | 71.245 | 482.089 | 2627 | 0.7692 | 1.3184 |
| 300 | 64 | 0.8452 | 1.4103 | 0.8756 | 57.937 | 372.736 | 671 | 0.2171 | 0.6250 |
| 300 | 256 | 2.0808 | 3.0037 | 1.9647 | 22.104 | 156.043 | 183 | 0.0511 | 0.1688 |
| 300 | 1024 | 5.6182 | 8.5691 | 5.3353 | 13.953 | 46.485 | 51 | 0.0120 | 0.0352 |

Informational only (NOT the plan's rule): S3(C) at T=60 s with the map bytes scaled by 0.46 (DESIGN's run-encoded leaves, 352/768 MiB per TiB):
  C=16 KiB: S3 p50 0.6597 p90 0.8330
  C=64 KiB: S3 p50 1.2155 p90 1.7746
  C=256 KiB: S3 p50 2.9917 p90 5.4409
  C=1024 KiB: S3 p50 7.4655 p90 19.7324

ADR-5 verdict at T=60 s (primary): amend to 16 KiB [S3<=1.25*min: false, RMW<=2*RMW16: true, mapWA p90<=25%: false]
sign check T=10 s: amend to 16 KiB [S3<=1.25*min: false, RMW<=2*RMW16: true, mapWA p90<=25%: false]
sign check T=300 s: amend to 16 KiB [S3<=1.25*min: false, RMW<=2*RMW16: true, mapWA p90<=25%: false]

| size (per mille of WS) | LRU med | S3-FIFO med | SIEVE med | W-TinyLFU med | S3-FIFO loses to LRU on |
|---|---|---|---|---|---|
| 5 | 0.4038 | 0.4001 | 0.4036 | 0.4658 | 59% of 27 vols |
| 10 | 0.3952 | 0.3823 | 0.3962 | 0.3970 | 33% of 27 vols |
| 20 | 0.3598 | 0.3458 | 0.3346 | 0.3757 | 26% of 27 vols |
| 50 | 0.3147 | 0.2969 | 0.2937 | 0.3092 | 22% of 27 vols |
| 100 | 0.2714 | 0.2613 | 0.2546 | 0.2686 | 7% of 27 vols |
| 250 | 0.2070 | 0.2060 | 0.2010 | 0.2023 | 4% of 27 vols |

Informational: 16 KiB chunk cache check (median request miss ratio over volumes):
  10 per mille: lru 0.6664, s3fifo 0.6407, sieve 0.6381, wtinylfu 0.6919
  50 per mille: lru 0.5689, s3fifo 0.5202, sieve 0.5437, wtinylfu 0.5605
  100 per mille: lru 0.5247, s3fifo 0.4888, sieve 0.4777, wtinylfu 0.5138

Scan test (mean over scan volumes of S3-FIFO minus LRU miss ratio, absolute):
  5 per mille: -0.0057 over 9 volumes (worst +0.0528)
  10 per mille: -0.0181 over 9 volumes (worst +0.0506)
  20 per mille: -0.0242 over 9 volumes (worst -0.0020)
  50 per mille: -0.0145 over 9 volumes (worst -0.0000)
  100 per mille: -0.0176 over 9 volumes (worst -0.0001)
  250 per mille: -0.0246 over 9 volumes (worst -0.0001)
Per-test reading: 2 scan tests with S3-FIFO > 2% abs worse than LRU: mds_1 @5pm (+0.0528); mds_1 @10pm (+0.0506)
Informational: share of volumes where S3-FIFO is worse than LRU by more than a margin (miss ratio, abs):
  10 per mille: >0: 33%  >0.001: 30%  >0.005: 22%  >0.01: 15%
  50 per mille: >0: 22%  >0.001: 11%  >0.005: 11%  >0.01: 4%
  100 per mille: >0: 7%  >0.001: 4%  >0.005: 4%  >0.01: 4%

ADR-14 verdict: S3-FIFO FAILS the scan rule (any single scan test > 2% abs worse than LRU) [accept clauses: false, sieve<=0.95 at >=2 sizes: false, wtinylfu<=0.90 at all: false, scan_fail: true]
```

## Modelling choices (plan ambiguities, fixed in code)
1. Cold RMW: model caches (infinite, LRU 1 %, LRU 10 % of the touched-chunk working set) are filled by reads and RMW fetches only, not by writes; otherwise the infinite
   cache would never miss on a previously written chunk. A partial write is an RMW candidate only if that chunk was written earlier in the trace; others are zero-fill.
2. Cuts every T of trace clock from the volume's first request, last partial interval included. Data per cut = distinct dirty chunks x C (x 0.5); map sealed = dirty leaves x F x 48 B
   (x 0.5 for leaves only) + path interior nodes x F x 48 B; depth from the highest touched chunk. mapWA uses matching ratios.
3. Gate inputs: S3 at T = 60 s, fanout 512, per volume then p50/p90 across volumes; RMW uses LRU-10 %; mapWA uses p90 over cuts then p90 across volumes. "Loses to LRU" is strict
   inequality on median-free per-volume miss ratio.
4. Cache replay: reads are the measured accesses (miss ratio per chunk access), writes are pinned dirty until the next 60 s cut, reads of dirty chunks are overlay hits, at the cut dirty
   chunks enter the clean cache as an access. Sizes are per mille of distinct touched chunks, minimum 16 chunks. Scan test: 2x capacity of never-reused read chunks at the midpoint; stats over original accesses only.
5. W-TinyLFU: 1 % window, 80 % protected SLRU, 4 x 4-bit count-min with doorkeeper, reset every 10 x capacity, strict-greater admission (no jitter). S3-FIFO per the SOSP'23 pseudocode.
6. Scan-rule reading: per test (any volume x size), which is the plan's wording; the mean-over-volumes variant is shown for information.

## Not run
| item | reason |
|---|---|
| Alibaba 2020, Tencent CBS | not obtainable without a survey / licence acceptance and bot challenge (TRACES.md) |
| libCacheSim cross-check on 3 volumes | no download allowed; libCacheSim not on the host |
| segment-granular eviction stretch, ARC/2Q, plots | cut (report-only items) |
| Tencent CBS parser | untested on real data (layout from memory) |
| Single-process full-trace aggregate | volumes simulated independently; aggregate = pooled CSV rows |

## Proposed decision record
> **ADR-5 gate outcome (2026-11-12): amended on MSR Cambridge evidence, pending Alibaba/Tencent.** Rule from the plan applied unchanged on 27 MSR volumes: 64 KiB not confirmed
> (S3(64) = 1.30/1.95 vs S3(16) = 0.76/1.05 per logical GiB, p50/p90; map write amplification at 64 KiB p90 68.8 % vs 25 % limit), and S3(16) <= 0.75 x S3(64) at p50 and p90.
> Proposed default 16 KiB with a 64 KiB bulk profile; map is 3.9x larger at 16 KiB (about 2.6 GiB per TiB touched at 48 B entries, less with ADR-8 run-encoded leaves). The maintainer should
> decide whether to adopt, noting MSR is small-random-write heavy and that mapWA at 16 KiB exceeds 100 % in the plan's model, which argues for map-delta records in DELTA commits if 16 KiB is adopted.
> Evidence: `bench/tracesim/RESULTS.md`.
>
> **ADR-14 gate outcome (2026-11-12): S3-FIFO not accepted; no amendment margin met; status stays proposed.** At 1/5/10 % of the working set S3-FIFO is within 5 % of the best median miss ratio
> and below LRU's (0.382/0.297/0.261 vs LRU 0.395/0.315/0.271), but it loses to LRU on 33 % of volumes at 1 % (limit 25 %) and two scan tests (mds_1 at 0.5 % and 1 %) are 5 points worse than LRU
> (limit 2). SIEVE is not 5 % better at two sizes; W-TinyLFU is not 10 % better at any. Suggested follow-up: keep S3-FIFO as the working choice, treat SIEVE as the equally good simpler alternative, and rerun with Alibaba/Tencent
> traces and a libCacheSim cross-check before the cache format freezes.
