# ADR-5 chunk size benchmark: results (M16, 2026-11-18)

Run on one dedicated host (16 vCPU AMD EPYC, 30 GiB, kernel 7.3, fio 3.43, PostgreSQL 18.3 from the distribution packages,
ext4 and xfs from the distribution), `lunixd`/`lunix`/`lunix-fsck` release builds of `p1/m16`. Volumes are 48 GiB virtual,
`--id-len 16`, `--compress auto`, integrity mode (unencrypted), `journal_max` 1 GiB, 4 queues, GC on with the default
retention, recreated for every cell; chunk sizes 64 KiB then 16 KiB back to back for every workload (the VM workload on the same
day, so both sizes saw the same package set: 248/250/249 installable names, 1918/1764/1905 packages after the churn at both
sizes). Every cell ran **once** (the plan's long-cell rule; no cell here is short enough to repeat), so every number below
carries the noise caveat; differences below about 10-20 % are not distinguishable from run-to-run variation (Part A shows
up to 1.3x min/max spread on cells of 30 s).
Raw data: `bench/phase1/chunksize/results/{fio-pg,vm}` (per cell: status before/after, GC pass output, `lunix fsck --deep`
JSON, fio JSON or pgbench output and the `pglat.json` latency percentiles, the store-over-time timeline). The pgbench
per-transaction logs (5 MB per cell, 10 % sampling) are not kept; `pglat.json` holds what is computed from them.
Reproduce the tables: `python3 -I bench/phase1/chunksize/analyze.py bench/phase1/chunksize/results/fio-pg bench/phase1/chunksize/results/vm --parta bench/phase1/perf/results/partA-verdict.json`.

## Reading the numbers

- "Stored bytes (data + map) per logical GiB written" is the fsck recount of live data plus map packs over the status counter
  `bytes_written` of the workload. For the time-based pgbench cells the denominator differs between the sizes (65.7 against
  74.2 GiB on ext4) because the 16 KiB cells ran fewer transactions; the absolute stored bytes of the same 15 GB data set are
  the fairer view: live data plus map, ext4 1.58 GiB (16 KiB) against 1.51 GiB (64 KiB), xfs 1.71 against 1.46 GiB. Either way
  16 KiB is **higher**, never 15 % lower. The on-disk store after the final GC (`store after GC GiB`) points the same way
  (pgbench ext4 2.36 against 2.13, xfs 2.52 against 2.02, vm 9.38 against 9.21 GiB).
- The map costs what the chunk count says: map GiB per TiB mapped 11-13 at 16 KiB against 1.7 at 64 KiB (about 7x), and the
  map seal write amplification is about 3x higher at 16 KiB (0.015 against 0.005 for pgbench, 0.107 against 0.040 for randmix).
- The compression gain of 16 KiB chunks is small and does not pay for the map: pgbench 12.8 against 13.3 (ext4), VM 2.87
  against 3.00. Dedup is about 1.0 for fio (the `bssplit` mix at 4-64 KiB does not repeat whole chunks), 1.05-1.07 for the VM
  images (16 KiB 1.067, 64 KiB 1.045: the only workload where the smaller chunk dedups more, by 2 %).
- Daemon CPU per GiB written (utime+stime of the process over the workload): pgbench 4.87 against 4.59 (ext4) and 4.72 against
  4.55 (xfs) at 16 / 64 KiB (1.06x, 1.04x); randmix 9.21 against 10.39; seqrand 5.84 against 6.50; **vm 10.43 against 3.95**
  (absolute CPU 397 s against 156 s for the same 38-40 GiB). The VM cell drives clause 2: without it the ratio is 1.05x.
  Why the VM cell at 16 KiB needs 2.5x the CPU is not established here (the apt churn is single-run and dominated by small
  random writes into loop-mounted images; the daemon's busiest thread per cell is in `threads-1.txt`).
- Partial-chunk read-modify-write (`cold_rmw`) is 790-1100 per 1 000 writes for the fio and pgbench workloads at both sizes
  (these workloads write 4-16 KiB into chunks that are not resident), and 85-87 per 1 000 for the VM churn. 16 KiB does not
  remove the RMW for 8 KiB PostgreSQL pages or 4 KiB fio writes: it makes it cheaper per write (a 16 KiB read instead of 64 KiB),
  which shows in the fio latency (randmix p99 10.7 ms against 33.8 ms, seqrand 9.5 against 39.1 ms) and not in the pgbench
  TPS (756/738 against 905/898, 16 KiB 16-18 % lower; PostgreSQL p99 34.8/35.5 ms against 42.0/38.3 ms).

## GC and store growth under sustained random overwrite (finding for the lead)

The store follows the lead's M15 note: under the random-overwrite workloads the collector falls behind the writer. From the
60 s `store-timeline.tsv` of each cell (store directory bytes, collector state):

| cell | live data (fsck) | store at the end of the workload | growth | store after the three final passes |
|---|---|---|---|---|
| randmix 64 KiB | 10.2 GiB | **69.1 GiB** (at 1784 s) | linear, about 2.3 GiB per minute | 15.9 GiB |
| randmix 16 KiB | 10.2 GiB | 27.8 GiB | about 0.9 GiB per minute | 14.2 GiB |
| seqrand 64 KiB (15 min overwrite) | 8.1 GiB | 52.4 GiB | about 2.5 GiB per minute | 13.1 GiB |
| seqrand 16 KiB | 8.1 GiB | 25.7 GiB | about 1.1 GiB per minute | 11.5 GiB |
| pgbench (all four) | 1.4-1.5 GiB | 7.8-10.2 GiB | bounded | 3.0-3.5 GiB (store dir after GC) |

The collector was in `compacting` for most of the second half of the 64 KiB randmix cell (`bytes_moved` is 2.15 GB per final
pass in every cell; the first final pass of the 64 KiB randmix cell freed 9.2 GB) and its pass rate did not keep up with a writer of about
17 MB/s; the peak of 69.1 GiB on a 20 GiB file with 10.2 GiB live (6.8x) stayed under the 90 GiB guard of the harness
(`STORE_CAP_GIB`, kill of fio/pgbench above it; not triggered) and the 100 GiB plan cap. The harness's three final passes
brought every store down to 11-16 GiB, still above the live data plus map (about 10.4 GiB for randmix 64 KiB). This is a GC
rate finding, not a harness failure; 16 KiB chunks make it 2.5x milder (less data rewritten per 4-16 KiB write) and that effect
is not part of the advisory rule's clause 1 (which counts live data).

## Tables (analyze.py output)


### pgbench

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 16 | 1 (once: noise caveat) | 65.7 | 1.000 | 12.773 | 1.36 | 0.220 | 12.95 | 3586933.7902 | 0.0153 | 2.36 | 0.0240 | 4.87 | 901.6 | n/a | n/a | n/a | 756 | 34.77 |
| xfs | 16 | 1 (once: noise caveat) | 65.4 | 1.000 | 11.848 | 1.51 | 0.199 | 11.36 | 3265074.0792 | 0.0148 | 2.52 | 0.0262 | 4.72 | 989.6 | n/a | n/a | n/a | 738 | 35.49 |
| ext4 | 64 | 1 (once: noise caveat) | 74.2 | 1.000 | 13.310 | 1.48 | 0.032 | 1.67 | 463421.6551 | 0.0050 | 2.13 | 0.0203 | 4.59 | 801.9 | n/a | n/a | n/a | 905 | 41.98 |
| xfs | 64 | 1 (once: noise caveat) | 72.8 | 1.000 | 12.468 | 1.43 | 0.030 | 1.73 | 446331.9620 | 0.0044 | 2.02 | 0.0201 | 4.55 | 842.4 | n/a | n/a | n/a | 898 | 38.31 |

PostgreSQL: postgres (PostgreSQL) 18.3

### randmix

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 16 | 1 (once: noise caveat) | 29.2 | 1.005 | 1.989 | 10.15 | 0.261 | 13.16 | 9581573.3717 | 0.1066 | 13.21 | 0.3564 | 9.21 | 792.9 | 1758 | 16 | 10.68 | n/a | n/a |
| ext4 | 64 | 1 (once: noise caveat) | 30.1 | 1.000 | 1.993 | 10.17 | 0.031 | 1.59 | 1123155.7435 | 0.0404 | 14.85 | 0.3389 | 10.39 | 872.3 | 1792 | 17 | 33.82 | n/a | n/a |

### seqrand

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 16 | 1 (once: noise caveat) | 30.4 | 1.006 | 1.974 | 8.06 | 0.033 | 2.14 | 1181992.0612 | 0.0610 | 10.48 | 0.2661 | 5.84 | 1101.1 | 1896 | 17 | 9.50 | n/a | n/a |
| ext4 | 64 | 1 (once: noise caveat) | 28.7 | 1.000 | 1.978 | 8.09 | 0.020 | 1.29 | 753041.4488 | 0.0154 | 12.13 | 0.2831 | 6.50 | 980.3 | 1589 | 15 | 39.06 | n/a | n/a |

### vm

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 16 | 1 (once: noise caveat) | 38.0 | 1.067 | 2.866 | 8.08 | 0.166 | 6.89 | 4693551.3168 | 0.0074 | 9.38 | 0.2169 | 10.43 | 84.7 | n/a | n/a | n/a | n/a | n/a |
| ext4 | 64 | 1 (once: noise caveat) | 39.6 | 1.045 | 2.996 | 7.98 | 0.056 | 2.30 | 1522694.2613 | 0.0055 | 9.21 | 0.2031 | 3.95 | 87.2 | n/a | n/a | n/a | n/a | n/a |

### Advisory rule (ADR-5, fixed in advance)

1. stored bytes (data plus map) per logical GiB written, VM-image and pgbench workloads together: 16 KiB 0.0682, 64 KiB 0.0590 -> 16 KiB is 15.6 % higher (need >= 15 % lower): **does not hold**
2. daemon CPU seconds per logical GiB: 16 KiB 6.062, 64 KiB 4.441 -> 1.36x (need <= 1.3x): **does not hold**
3. Part A: rows that pass at 64 KiB but not at 16 KiB: seqr: **does not hold**

**Recommendation by the rule: 64 KiB**

Per-workload stored bytes per logical GiB (data plus map), for the reader who wants to see whether the aggregate hides a split:

| workload | fs | 16 KiB | 64 KiB | 16 KiB vs 64 KiB |
|---|---|---|---|---|
| pgbench | ext4 | 0.0240 | 0.0203 | 18.1 % higher |
| pgbench | xfs | 0.0262 | 0.0201 | 30.1 % higher |
| vm | ext4 | 0.2169 | 0.2031 | 6.8 % higher |

### Proposed Gate outcome block for ADR-5 (the lead appends it after the maintainer decides)

```
**Gate outcome (phase 1, M16):** advisory rule applied to the VM-image and pgbench workloads on ext4 and xfs volumes.
Stored bytes per logical GiB written (data plus map): 16 KiB 0.0682, 64 KiB 0.0590 (15.6 % higher; clause 1 does not hold).
Daemon CPU per GiB: 1.36x (clause 2 does not hold). Part A: rows that pass at 64 KiB but not at 16 KiB: seqr (clause 3 does not hold).
Recommendation: 64 KiB as the default; both sizes stay supported per volume. Evidence: bench/phase1/chunksize/RESULTS.md.
```

## Sensitivities of the rule (not part of the rule as fixed in the plan)

- Clause 3 uses Part A as run to the plan: the 4 GiB working set against the default `read_cache_max` of 128 MiB, where the
  "cache hit" rows read from the store. There `seqr` passes at 64 KiB (8.42 GB/s at 4 queues) and fails at 16 KiB (3.98 GB/s at
  8 queues), which is the row clause 3 names. In the supplementary run with `read_cache_max` 8 GiB (the working set resident,
  `bench/phase1/perf/results/partA-readcache*`) `seqr` passes at both sizes (11.60 GB/s at 64 KiB, 9.61 GB/s at 16 KiB, 8 queues)
  and `r4k` reaches 832 K (64 KiB) and 772 K (16 KiB) IOPS, so clause 3 would hold; clauses 1 and 2 do not depend on it, so the
  recommendation by the rule is 64 KiB either way.
- Clause 2 without the VM workload holds (1.05x); with it it does not (1.36x). The VM CPU gap is a single run and unexplained.
- Neither size is excluded: both stay supported per volume (the rule is advisory; the maintainer decides).
