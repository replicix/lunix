# Validation study: SUMMARY

Status 2026-11-14: measurement, timing and analysis complete, re-analysed under **Amendment A1** (`PREREGISTERED.md`, committed 30e4329
before any follow-up data) with the follow-up measurements A1 required (every queue 0 FAILED). Verdicts follow §1.2 and A1 mechanically,
except D2 for database images and D7-Lunix, which are recorded under Ruling R-D2 (post-data).
Where neither the protocol nor A1 settles a rule, the summary states the ambiguity, applies the rule that was fixed before the data in
question existed, and shows the other reading next to it. Hypothesis families: `raw/families.csv` (first analysis, 323 tests, written
2026-11-13T17:13:19Z, SHA-256 `4536a207…`) and `raw/families-a1.csv` (A1, 383 tests, written on the test host 2026-11-14T07:06:44Z,
SHA-256 `1ce6c2b7…`, before the A1 analysis ran, but after the follow-up measurements existed and after their per-image timing ratios and
the D5 aggregates had been seen (deviation A1)). D2 for database images is recorded under the maintainer's **Ruling R-D2**
(`PREREGISTERED.md`, 2026-11-14), which was made after the A1 results were seen and is labelled as such: it is not a pre-registered
verdict. Costs are given only as CPU-seconds, cores and throughput ratios.

## 0. Changes from the first analysis

| item | first analysis (2026-11-13) | this analysis (A1) |
|---|---|---|
| D2, database family | NO-GO: 2 of 15 at 16 KiB, against a hybrid comparator (best of counterfactual and shipped sets) with the cap from one PostgreSQL DB-page cell applied to 12 images | A1 applied mechanically gives "GO at 16 KiB only" (9 of 15 at 16 KiB, 8 needed; 0 of 15 at 64 KiB; judged on `pv-cover`; everything against each image's own counterfactual and own volume-wide timing cell). Ruling R-D2 (made after these data) records it as **marginal, not established** |
| D2c | not evaluated | computed because the mechanical D2 vote fired: 0 of 9 (moot under R-D2) |
| D7-Lunix | fires | does not fire mechanically (A1: D2 database "GO at 16 KiB only"); its consequences (codec 3 reserved, `estimate`/`train`/`apply` out of v1) are applied by Ruling R-D2, made after the data |
| D5 | NO-GO from the hard cap alone, saving not measured | NO-GO from the decision table itself (both floors missed on all 4 file corpora) **and** the hard cap; per-object pass on all four corpora |
| D1, D3, D4, D6, D2 raw | NO-GO | unchanged (NO-GO) |
| intervals | 1 MiB blocks only | 16 MiB blocks (16 MiB file extents for Constellation) printed next to every 1 MiB interval; they are 3.2x wider (median); no pass flag changes |
| comparator construction | "stricter reading decides" for D2 (deviation 22) | removed: A1.1 decides (counterfactual for S, LB, N, cores and cap) |
| other corrections (review) | | D1 "decisive" qualified; D5 cause named (selector); decide.py confirmatory rule replaced by A1.4; D5 vote computed, not hard-coded; one-unit rows now blank; deviation 1 reworded; §4.4 unit counts for Constellation views disclosed with an extent-level re-bootstrap |

**D2 for database images: the evidence behind "marginal, not established" (R-D2).** 8 of 15 images are needed at 16 KiB.

| reading (16 KiB) | images passing | what it depends on |
| --- | --- | --- |
| A1 as applied (pv-cover, a volume passes at either level) | **9 of 15** | the either-level rule, which A1 did not fix; two images (superuser-sq, serverfault-sq) pass only at level 1 |
| one fixed level per size (level 3) | 7 of 15 | the level-3 arm of those two images fails the decompress cap (0.796, 0.7997) |
| judged on `pv` instead of `pv-cover` | 5 of 15 | the COVER trigger: median pv-cover minus pv 1.0081 pp at level 3 (1.0944 at level 1) against 1 pp |
| cap margins of the passing level-3 images | 0.8020, 0.8044, 0.8088 | 0.002-0.009 above the 0.8x cap; the timing cells' max/min spread reaches 1.019 |
| images failing the cap by as little | compress 0.794-0.796 on askubuntu-pg, askubuntu-my, serverfault-my and geonames-pg (all with N 3.2-7.9 pp) | with the cap not applied 13 of 15 |
| counterfactual choice | zstd-3 passes the cap against zstd-1 at 0.803-0.823 on the nine SE images (0.803 on serverfault-pg) | a failure would change the comparator of that image |
| 64 KiB (confirmatory) | 0 of 15 | level 3 fails the cap (0.47-0.65x), level 1 loses bytes |

Within the same noise the mechanical vote could lie anywhere between 5 and 13 of 15.

## 1. Abstract

Three answers. **(1) Raw devices:** no shipped dictionary is worth codec 3. On OS images at 16 KiB the best per-chunk choice among the
33 zdicts dictionaries and the block set (the oracle) saves 0.9-1.5 % against zstd-3 behind the probe (LB 0.84-1.40 pp with 1 MiB
blocks, 0.76-1.20 with 16 MiB blocks), and the selector that would realise it costs 28-30 cores at 5 GB/s. At 64 KiB the best realisable
dictionary arm adds 0.3-0.8 pp over what zstd-3 alone gives and every one fails the speed cap. **(2) Per-volume training:** loses on OS
images (pv@3 is 0.2-0.9 % worse than zstd-3). On database images with 16 KiB chunks a COVER-trained per-volume dictionary saves
5.1-13.4 % against zstd-3 on the Stack Exchange and TLC images and passes N ≥ 2 pp and the 0.8x cap on 9 of 15 images (8 needed),
measured per image on a volume-wide timing sample, but only through margins inside measurement noise (7 of 15 with one fixed level, 5 of
15 with fastCover, caps 0.002-0.009 above 0.8): **marginal, not established** (Ruling R-D2, made after the data). At 64 KiB the counterfactual becomes zstd-2 or zstd-3 and every dictionary arm
either fails the cap (0.47-0.65x at level 3) or loses bytes (level 1): 0 of 15. **(3) Database pages:** a schema-disjoint DB set is no
better than plain zstd-3 at any size (medians -2.1 to +0.1 %); only the volume's own data helps.

| decision | verdict | key number |
|---|---|---|
| D1 Lunix codec 3 with built-ins | **NO-GO** (16 KiB: decisive among the measured candidate sets) | 0 of 4 eligible corpora at either size; best N -0.20 pp (16 KiB, bs-dist@3), 2.54 pp (64 KiB, bs-dist@3, cap 0.45x); oracle LB 0.84-1.40 pp at 16 KiB |
| D2 Lunix `train`/`apply`, raw devices | **NO-GO** | 0 of 4 at both sizes; pv@3 N -1.22 to -0.58 pp at 16 KiB |
| D2, database images | **marginal, not established** (R-D2, after the data) | A1 mechanically: pv-cover 9 of 15 at 16 KiB (N 3.0-13.1 pp on the passing images), 0 of 15 at 64 KiB; one fixed level 7 of 15; `pv` 5 of 15; caps 0.802-0.809 |
| D2c `--classes` | **NO-GO** (moot) | 0 of the 9 volumes gain ≥ 1 pp from per-class dictionaries |
| D3 zdicts block/fs set | **NO-GO** | D1 does not fire |
| D4 zdicts DB set | **NO-GO** | D4-L median N -1.06 / -2.91 / -2.50 pp (pg/my/sq); D4-C -0.49 / -0.59 / -0.41; no consumer fires |
| D5 Constellation file-type set | **NO-GO** | both floors missed on 4 of 4 corpora (under 64 KiB S 1.29-2.69 %, floor 5 pp; all objects 0.27-0.74 %, floor 1 pp) and the hard cap fails (0.14x / 0.39x), caused by the selector's classifier |
| D6 Constellation DB dictionaries | **NO-GO** | median N at 1 MiB -0.49 / -0.59 / -0.41 pp; page-size LB < 15 pp, no open question |
| D7 | **fires for Constellation and zdicts**; Lunix: does not fire mechanically (A1: D2 database "GO at 16 KiB only"); its consequences (codec 3 reserved, `estimate`/`train`/`apply` out of v1) are applied by Ruling R-D2, made after the data | no dictionary work in any of the three products for this release |

Per product. **Lunix:** codec 3 stays reserved; ADR-30's `estimate`, `train` and `apply` stay out of v1 (D1 NO-GO; D2 NO-GO on OS
volumes and marginal, not established, on database volumes). The database result is the candidate for a later release (§3, "Deciding
experiment"). **Constellation:** stays at zstd:3, no dictionaries. **zdicts:** the file-type set stays as is; no block, file-system or
database set.

## 2. Question 1: raw devices (D1, D3)

Population: every unique non-hole chunk, behind the ADR-12 probe; unit = 1 MiB origin block (16 MiB blocks in the second interval);
B = 10,000; LB = BCa lower end. Counterfactual (CF, §1.1, from `raw/timing.csv`): zstd-3 behind the probe at 16 KiB (0.83x zstd-1),
zstd-1 at 64 KiB (zstd-3 0.76x; zstd-2 would pass at 0.89x but the protocol names zstd-1/zstd-3 for Lunix raw devices; using it only
lowers N, deviation 21). vm-debian and vm-fedora are below the 1,024-block floor and do not vote; a GO needs all four eligible corpora.

| corpus | chunk | unique non-hole | 1 MiB blocks | holes (of chunks) | duplicates (of non-hole) | probe-raw (bytes) | vote |
| --- | --- | --- | --- | --- | --- | --- | --- |
| host-ext4 | 16 / 64 KiB | 11.44 / 11.66 GiB | 12125 / 12148 | 61.9 / 61.8 % | 6.1 / 4.6 % | 16.0 / 15.8 % | yes |
| host-xfs | 16 / 64 KiB | 11.59 / 11.83 GiB | 12194 / 12297 | 62.6 / 62.5 % | 3.2 / 1.4 % | 16.0 / 15.9 % | yes |
| host-btrfs | 16 / 64 KiB | 11.19 / 11.53 GiB | 11739 / 11914 | 63.6 / 63.4 % | 4.0 / 1.6 % | 16.5 / 16.3 % | yes |
| vm-ubuntu | 16 / 64 KiB | 2.05 / 2.07 GiB | 2165 / 2165 | 40.4 / 40.3 % | 1.5 / 1.1 % | 13.5 / 13.4 % | yes |
| vm-debian | 16 / 64 KiB | 0.78 / 0.82 GiB | 936 / 936 | 69.9 / 68.8 % | 13.5 / 12.2 % | 10.0 / 10.0 % | no (exploratory) |
| vm-fedora | 16 / 64 KiB | 0.67 / 0.68 GiB | 701 / 702 | 85.0 / 85.0 % | 10.7 / 9.6 % | 66.8 / 66.8 % | no (exploratory) |

**16 KiB: volume-wide S against zstd-3 behind the probe, % [BCa 95 % with 1 MiB blocks / with 16 MiB blocks]**

| corpus | z1 | ft@1 | ft@3 | bs-dist@1 | bs-dist@3 | bs-xfs@3 | oracle-zd@3 | oracle@3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| host-ext4 | -1.70 [-1.73, -1.66 / -1.82, -1.58] | -1.58 [-1.62, -1.54 / -1.70, -1.46] | 0.46 [0.43, 0.48 / 0.39, 0.54] | -3.18 [-3.25, -3.12 / -3.42, -2.95] | -0.66 [-0.70, -0.62 / -0.77, -0.55] | -0.65 [-0.68, -0.61 / -0.75, -0.53] | 0.76 [0.73, 0.79 / 0.67, 0.86] | 0.93 [0.89, 0.96 / 0.83, 1.04] |
| host-xfs | -1.69 [-1.72, -1.65 / -1.80, -1.57] | -1.58 [-1.62, -1.54 / -1.69, -1.46] | 0.44 [0.42, 0.47 / 0.38, 0.52] | -3.22 [-3.28, -3.15 / -3.44, -2.99] | -0.70 [-0.74, -0.66 / -0.80, -0.59] | -0.69 [-0.73, -0.66 / -0.79, -0.59] | 0.74 [0.71, 0.77 / 0.67, 0.84] | 0.90 [0.87, 0.94 / 0.81, 1.01] |
| host-btrfs | -1.70 [-1.74, -1.67 / -1.82, -1.58] | -1.62 [-1.66, -1.58 / -1.74, -1.50] | 0.40 [0.38, 0.43 / 0.33, 0.49] | -3.22 [-3.29, -3.15 / -3.45, -2.98] | -0.68 [-0.72, -0.65 / -0.80, -0.56] | -0.67 [-0.71, -0.64 / -0.79, -0.55] | 0.71 [0.68, 0.74 / 0.62, 0.81] | 0.87 [0.84, 0.91 / 0.76, 1.00] |
| vm-ubuntu | -1.67 [-1.76, -1.59 / -1.93, -1.40] | -1.46 [-1.54, -1.38 / -1.70, -1.20] | 0.73 [0.66, 0.80 / 0.54, 0.97] | -2.64 [-2.78, -2.49 / -3.11, -2.17] | 0.26 [0.15, 0.37 / -0.03, 0.58] | - | 1.13 [1.05, 1.22 / 0.88, 1.44] | 1.51 [1.40, 1.62 / 1.20, 1.87] |
| vm-debian (expl.) | -1.66 [-1.80, -1.55 / -1.95, -1.31] | -1.53 [-1.67, -1.40 / -1.84, -1.18] | 0.89 [0.76, 1.04 / 0.59, 1.34] | -2.31 [-2.53, -2.11 / -2.85, -1.76] | 1.17 [0.98, 1.35 / 0.77, 1.62] | - | 1.62 [1.47, 1.79 / 1.21, 2.14] | 2.16 [1.98, 2.36 / 1.66, 2.69] |
| vm-fedora (expl.) | -0.27 [-0.31, -0.23 / -0.47, -0.16] | -0.27 [-0.31, -0.23 / -0.47, -0.16] | 0.00 [0.00, 0.00 / 0.00, 0.00] | -0.55 [-0.65, -0.47 / -0.97, -0.31] | -0.15 [-0.18, -0.13 / -0.26, -0.08] | -0.16 [-0.19, -0.14 / -0.28, -0.09] | 0.01 [0.01, 0.01 / 0.01, 0.02] | 0.03 [0.02, 0.04 / 0.02, 0.05] |

**64 KiB: S against zstd-1 behind the probe**

| corpus | z3 | ft@1 | ft@3 | bs-dist@1 | bs-dist@3 | bs-xfs@3 | oracle-zd@3 | oracle@3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| host-ext4 | 2.53 [2.47, 2.58 / 2.35, 2.70] | 0.17 [0.16, 0.17 / 0.14, 0.19] | 2.81 [2.75, 2.87 / 2.61, 3.00] | 0.26 [0.25, 0.27 / 0.23, 0.29] | 2.96 [2.89, 3.02 / 2.74, 3.17] | 2.97 [2.91, 3.04 / 2.76, 3.19] | 3.06 [3.00, 3.12 / 2.85, 3.27] | 3.25 [3.18, 3.32 / 3.02, 3.48] |
| host-xfs | 2.52 [2.47, 2.57 / 2.35, 2.69] | 0.16 [0.16, 0.17 / 0.14, 0.19] | 2.80 [2.74, 2.86 / 2.62, 2.98] | 0.26 [0.25, 0.27 / 0.23, 0.28] | 2.95 [2.89, 3.01 / 2.75, 3.14] | 2.95 [2.89, 3.01 / 2.75, 3.15] | 3.06 [3.00, 3.12 / 2.86, 3.25] | 3.25 [3.18, 3.31 / 3.03, 3.46] |
| host-btrfs | 2.54 [2.48, 2.59 / 2.36, 2.72] | 0.15 [0.14, 0.16 / 0.13, 0.18] | 2.79 [2.74, 2.85 / 2.59, 2.99] | 0.26 [0.25, 0.26 / 0.23, 0.29] | 2.96 [2.90, 3.02 / 2.75, 3.17] | 2.97 [2.91, 3.03 / 2.76, 3.18] | 3.05 [2.99, 3.11 / 2.84, 3.26] | 3.24 [3.18, 3.30 / 3.01, 3.47] |
| vm-ubuntu | 2.67 [2.54, 2.80 / 2.22, 3.09] | 0.25 [0.23, 0.28 / 0.19, 0.34] | 3.12 [2.97, 3.27 / 2.61, 3.60] | 0.46 [0.43, 0.49 / 0.36, 0.56] | 3.48 [3.31, 3.65 / 2.90, 4.04] | - | 3.43 [3.27, 3.59 / 2.87, 3.96] | 3.71 [3.53, 3.88 / 3.10, 4.28] |
| vm-debian (expl.) | 2.85 [2.66, 3.03 / 2.24, 3.31] | 0.28 [0.25, 0.32 / 0.19, 0.39] | 3.38 [3.16, 3.60 / 2.66, 3.93] | 0.67 [0.61, 0.73 / 0.51, 0.81] | 4.14 [3.87, 4.39 / 3.26, 4.76] | - | 3.94 [3.70, 4.18 / 3.10, 4.55] | 4.44 [4.16, 4.71 / 3.51, 5.10] |
| vm-fedora (expl.) | 0.40 [0.33, 0.47 / 0.22, 0.70] | 0.00 [0.00, 0.00 / 0.00, 0.00] | 0.40 [0.33, 0.47 / 0.22, 0.70] | 0.02 [0.01, 0.02 / 0.01, 0.03] | 0.44 [0.37, 0.52 / 0.25, 0.78] | 0.44 [0.37, 0.52 / 0.25, 0.78] | 0.42 [0.36, 0.50 / 0.24, 0.74] | 0.46 [0.39, 0.54 / 0.26, 0.80] |

**D1 tests: N = LB - 0.1 pp x delta-cores at 5 GB/s, with 1 MiB blocks (16 MiB blocks); cap against the CF on the vm-ubuntu cell (T2)**

| size | arm | cap compress / decompress | delta-cores | N host-ext4 / host-xfs / host-btrfs / vm-ubuntu | max Holm p | passing |
| --- | --- | --- | --- | --- | --- | --- |
| 16 KiB | ft@1 | 0.28 / 1.00 | 28.0 | -4.42 (-4.50) / -4.41 (-4.49) / -4.45 (-4.54) / -4.34 (-4.50) | 7e-248 | 0 |
| 16 KiB | ft@3 | 0.27 / 0.94 | 29.9 | -2.56 (-2.61) / -2.57 (-2.61) / -2.62 (-2.67) / -2.33 (-2.45) | 2e-125 | 0 |
| 16 KiB | bs-dist@1 | 1.08 / 1.07 | 0.0 | -3.25 (-3.42) / -3.28 (-3.44) / -3.29 (-3.45) / -2.78 (-3.11) | 1e-266 | 0 |
| 16 KiB | bs-dist@3 | 0.75 / 0.94 | 3.5 | -1.05 (-1.12) / -1.09 (-1.16) / -1.08 (-1.16) / -0.20 (-0.38) | 0.39 | 0 |
| 64 KiB | ft@1 | 0.45 / 0.95 | 7.7 | -0.61 (-0.62) / -0.61 (-0.62) / -0.62 (-0.64) / -0.53 (-0.58) | 5e-152 | 0 |
| 64 KiB | ft@3 | 0.37 / 0.90 | 10.7 | 1.68 (1.54) / 1.67 (1.54) / 1.66 (1.52) / 1.90 (1.54) | 2e-295 | 0 |
| 64 KiB | bs-dist@1 | 0.85 / 0.95 | 1.1 | 0.13 (0.12) / 0.13 (0.12) / 0.13 (0.11) / 0.31 (0.25) | 2e-216 | 0 |
| 64 KiB | bs-dist@3 | 0.45 / 0.88 | 7.7 | 2.12 (1.97) / 2.12 (1.97) / 2.13 (1.97) / 2.54 (2.13) | 1e-287 | 0 |

The Wilcoxon p-values come from 1 MiB blocks, which are measurably dependent (16 MiB intervals 3.2x wider); read them as ordering, not
as calibrated probabilities. No D1 conclusion depends on them.

**Per class, 16 KiB** (S against zstd-3 behind the probe, %, [BCa LB 1 MiB / 16 MiB] where the class has ≥ 200 blocks):

| corpus | class (share of bytes, blocks) | ft@3 | bs-dist@3 | oracle@3 |
| --- | --- | --- | --- | --- |
| host-ext4 | data (96.5 %, 11692) | 0.46 [0.43 / 0.39] | -0.65 [-0.69 / -0.76] | 0.93 [0.89 / 0.82] |
| host-ext4 | journal (2.1 %, 251) / dir (0.9 %, 445) / meta (0.5 %, 86) | -0.42 / 0.66 / 0.00 | -2.95 / 2.53 / -5.14 | 0.81 / 4.00 / 0.02 |
| host-xfs | data (98.3 %) / meta (1.0 %, 1634) | 0.45 [0.42 / 0.38] / 0.00 | -0.69 [-0.72 / -0.79] / -1.57 [-1.65 / -1.67] | 0.90 [0.87 / 0.81] / 0.37 [0.34 / 0.33] |
| host-btrfs | data (98.2 %) / meta (1.8 %, 257) | 0.41 [0.38 / 0.33] / 0.03 | -0.68 [-0.72 / -0.81] / -0.82 | 0.85 [0.82 / 0.75] / 2.47 |
| vm-ubuntu | data (87.5 %, 1885) / other partitions (6.1 %) / journal (3.0 %) / dir (2.0 %) / meta (1.2 %) | 0.83 [0.76] / -0.01 / -0.21 / 0.51 / 0.00 | 0.34 [0.22] / -0.22 / -1.79 / 2.60 / -2.33 | 1.70 [1.58] / 0.07 / 0.67 / 3.49 / 0.54 |

(Classes with fewer than 200 16 MiB blocks get no 16 MiB interval.) Data chunks by zdicts type (type = `select_by_content` on the chunk
head), host-ext4 16 KiB: 8.92 of 11.0 GiB select no dictionary (oracle-zd 0.19 %); text types gain 4-8 % with `ft@3` (C headers 4.35 %,
markup 6.77 %, Go 6.16 %, Rust 8.23 %), Java class files lose 2.97 %. Selector loss (oracle-zd minus ft) 0.3-0.4 pp, inside R5's 1.5 pp.

**Answer.** 16 KiB: NO-GO, decisive among the measured candidate sets: the oracle (best of the 33 zdicts dictionaries and bs-dist per
chunk; no DB set, deviation 15) has LB 0.84-1.40 pp (16 MiB blocks: 0.76-1.20) on all four eligible corpora, under the 3 pp floor; it is
not a bound on sets that were never trained. 64 KiB: NO-GO by the vote (0 of 4): the level-3 arms reach N 1.7-2.5 pp only because the CF
is zstd-1, most of their gain is the level (zstd-3 alone 2.5-2.7 %), and they fail the compress cap (0.37-0.45x). The oracle LB at 64 KiB
is 3.18-3.53 pp (3.01-3.10 with 16 MiB blocks), so the "decisive" clause does not hold there, by a narrow margin. **D3**: NO-GO (D1 does
not fire); bs-dist against `ft` at the same level is -1.6 to -0.5 pp at 16 KiB and +0.1 to +0.4 pp at 64 KiB; bs-xfs keeps all of
bs-dist's saving. vm-fedora: 67 % of its unique bytes are probe-raw, consistent with transparently compressed files (not verified).

## 3. Question 2: per-volume training (D2, D2c)

pv = one 110 KiB fastCover dictionary per volume (1 % sample, 8-64 MiB, seed 1 decides); pv-cover = `zstd --train-cover=steps=16` on the
same sample, same size (F4). Both tested on the chunks outside the sample. **A1.1:** S, LB, N, delta-cores and the 0.8x cap (compress and
decompress) are against the volume's counterfactual. **A1.2:** for every database image the counterfactual and the cap come from the
image's own volume-wide timing cell (`raw/timing-db.csv`: block-stratified 1 GiB of all unique non-hole chunks, all classes, LBA-aligned,
one pinned core, max/min ≤ 1.019). Database counterfactuals: **zstd-3 at 16 KiB** on all 15 (zstd-3 vs zstd-1 0.80-0.92x; the tightest,
serverfault-pg, 0.803); at 64 KiB zstd-3 fails the cap on the SE and geonames images (0.71-0.75x), so the CF is **zstd-2** there
(0.81-0.89x; 2.4-6.1 % fewer bytes than zstd-1 on the samples) and **zstd-3** on the TLC images (0.82-0.86x). zstd-2 has no per-chunk
measurement: those 64 KiB tests run against zstd-1 and S and LB are rescaled by the cell's zstd-2/zstd-1 byte ratio; cap and cores are
against zstd-2 exactly (deviation A6). Raw devices: the vm-ubuntu cells (T2).

**COVER rule (A1.3).** pv-cover minus pv against the CF at 16 KiB, median over the 15 images: **level 3: 1.0081 pp** (range -0.14 to
1.33; the median image is se-superuser-pg); **level 1: 1.0944 pp** (range -1.48 to 1.78). Both exceed 1 pp, so the database family is
**judged on pv-cover**. The level-3 margin is 0.008 pp; no interval is defined for the rule. (Measured relative to pv's own bytes
instead of the CF's, the level-3 median is 1.090 %.) Per image, level 3: askubuntu pg 1.28, my 0.84, sq 1.24; superuser pg 1.01, my 0.94,
sq 1.05; serverfault pg 1.25, my 0.73, sq 1.31; geonames pg 1.33, my 0.33, sq 1.17; tlc pg 0.76, my -0.14, sq 0.62.

**Raw devices** (S against the CF [LB 1 MiB / 16 MiB], %; N pp; cap of pv@3 compress / decompress against the CF):

| corpus | 16 KiB pv@1 | 16 KiB pv@3 | N pv@3 | 64 KiB pv@1 | 64 KiB pv@3 | N pv@3 | cap pv@3 16 / 64 KiB |
| --- | --- | --- | --- | --- | --- | --- | --- |
| host-ext4 | -3.45 [-3.52 / -3.69] | -0.87 [-0.90 / -0.96] | -1.18 | 0.13 [0.13 / 0.12] | 2.77 [2.71 / 2.57] | 1.95 | 0.80 / 0.45 |
| host-xfs | -3.37 [-3.44 / -3.58] | -0.91 [-0.95 / -1.00] | -1.22 | 0.16 [0.16 / 0.15] | 2.81 [2.75 / 2.61] | 1.99 | 0.80 / 0.45 |
| host-btrfs | -3.39 [-3.46 / -3.62] | -0.82 [-0.85 / -0.91] | -1.12 | 0.16 [0.15 / 0.14] | 2.81 [2.75 / 2.61] | 1.99 | 0.80 / 0.45 |
| vm-ubuntu | -3.13 [-3.29 / -3.60] | -0.23 [-0.31 / -0.44] | -0.58 | 0.26 [0.24 / 0.21] | 3.21 [3.06 / 2.69] | 2.30 | 0.80 / 0.45 |

(vm-debian, vm-fedora exploratory: 16 KiB pv@3 +1.30 / -0.06 %.) Raw-device vote: **0 of 4** at both sizes (at 64 KiB vm-ubuntu's
pv@3 reaches N 2.30 pp but fails the cap, 0.45x). **D2 raw: NO-GO.**

**Database images, 16 KiB** (S against zstd-3 behind the probe [LB 1 MiB / 16 MiB], %; cap compress / decompress against zstd-3 from the
image's own cell; delta-cores at 5 GB/s; N pp; pass = N ≥ 2, Holm p < 0.05, cap passed, either level):

| image | pv@1 | pv@3 | pv-cover@1 | pv-cover@3 | cap pv@3 | cap cover@3 | cores pv@3 / cover@3 | N pv@1 / pv@3 | N cover@1 / cover@3 | pass pv / cover |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| se-askubuntu-pg | 1.44 [1.26 / 0.64] | 7.17 [7.04 / 6.80] | 1.99 [1.81 / 1.17] | 8.46 [8.30 / 8.02] | 0.786 / 0.90 | 0.796 / 0.90 | 4.0 / 3.7 | 1.26 / 6.64 | 1.81 / 7.93 | 0 / 0 |
| se-askubuntu-my | -1.11 [-1.28 / -1.70] | 5.22 [5.03 / 4.47] | -0.26 [-0.43 / -0.87] | 6.07 [5.85 / 5.21] | 0.791 / 0.87 | 0.794 / 0.86 | 3.7 / 3.6 | -1.28 / 4.66 | -0.43 / 5.49 | 0 / 0 |
| se-askubuntu-sq | 2.08 [1.95 / 1.58] | 10.71 [10.54 / 10.04] | 3.18 [3.05 / 2.68] | 11.95 [11.77 / 11.24] | 0.856 / 0.82 | 0.870 / 0.85 | 2.8 / 2.5 | 1.95 / 10.26 | 3.05 / 11.52 | 1 / 1 |
| se-superuser-pg | 0.71 [0.54 / 0.01] | 7.55 [7.42 / 7.10] | 1.78 [1.61 / 1.06] | 8.56 [8.42 / 8.06] | 0.797 / 0.88 | **0.809** / 0.89 | 3.9 / 3.6 | 0.54 / 7.04 | 1.61 / 8.06 | 0 / 1 |
| se-superuser-my | -1.13 [-1.30 / -1.76] | 5.64 [5.47 / 4.94] | 0.10 [-0.06 / -0.49] | 6.58 [6.39 / 5.80] | 0.790 / 0.85 | **0.802** / 0.86 | 3.9 / 3.6 | -1.30 / 5.08 | -0.06 / 6.03 | 0 / 1 |
| se-superuser-sq | 1.99 [1.85 / 1.43] | 10.28 [10.11 / 9.58] | 3.26 [3.13 / 2.75] | 11.33 [11.16 / 10.62] | 0.856 / 0.798 | 0.859 / 0.796 | 2.8 / 2.8 | 1.85 / 9.82 | **3.13** / 10.88 | 0 / 1 (level 1) |
| se-serverfault-pg | 0.79 [0.66 / 0.28] | 6.97 [6.80 / 6.45] | 1.63 [1.49 / 1.10] | 8.22 [8.02 / 7.61] | 0.794 / 0.86 | **0.804** / 0.86 | 4.0 / 3.8 | 0.66 / 6.40 | 1.49 / 7.65 | 0 / 1 |
| se-serverfault-my | -1.48 [-1.65 / -2.01] | 4.85 [4.64 / 4.01] | -0.16 [-0.32 / -0.70] | 5.58 [5.34 / 4.65] | 0.792 / 0.84 | 0.796 / 0.85 | 3.8 / 3.7 | -1.65 / 4.26 | -0.32 / 4.97 | 0 / 0 |
| se-serverfault-sq | 1.77 [1.65 / 1.30] | 10.81 [10.64 / 10.11] | 3.11 [2.97 / 2.60] | 12.13 [11.95 / 11.43] | 0.860 / 0.806 | 0.865 / 0.800- | 2.8 / 2.6 | 1.65 / 10.37 | **2.97** / 11.69 | 1 / 1 (level 1) |
| geonames-pg | -6.78 [-7.28 / -9.07] | 2.26 [2.18 / 2.02] | -4.99 [-5.50 / -7.33] | 3.58 [3.52 / 3.39] | 0.79 / 1.02 | 0.794 / 0.99 | 3.2 / 3.1 | -7.28 / 1.86 | -5.50 / 3.21 | 0 / 0 |
| geonames-my | -9.24 [-9.57 / -10.59] | -0.93 [-1.05 / -1.42] | -7.80 [-8.09 / -9.02] | -0.60 [-0.72 / -1.03] | 0.75 / 1.01 | 0.75 / 0.99 | 3.9 / 3.9 | -9.57 / -1.44 | -8.09 / -1.11 | 0 / 0 |
| geonames-sq | -3.73 [-3.85 / -4.16] | 0.48 [0.39 / 0.15] | -3.34 [-3.44 / -3.70] | 1.65 [1.57 / 1.35] | 0.74 / 1.00 | 0.74 / 0.98 | 4.8 / 4.7 | -3.85 / -0.09 | -3.44 / 1.10 | 0 / 0 |
| tlc-2024q1-pg | 0.22 [-0.19 / -1.58] | 12.67 [12.40 / 11.59] | -1.27 [-1.63 / -2.89] | 13.42 [13.15 / 12.33] | 0.92 / 1.00 | 0.94 / 1.02 | 1.0 / 0.8 | -0.21 / 12.29 | -1.65 / 13.07 | 1 / 1 |
| tlc-2024q1-my | 0.11 [-0.53 / -2.69] | 5.23 [5.08 / 4.66] | -0.87 [-1.50 / -3.56] | 5.09 [4.85 / 4.15] | 0.86 / 0.91 | 0.87 / 0.91 | 1.9 / 1.9 | -0.57 / 4.89 | -1.54 / 4.66 | 1 / 1 |
| tlc-2024q1-sq | -1.07 [-1.24 / -1.69] | 5.45 [5.29 / 4.87] | 0.65 [0.42 / -0.16] | 6.07 [5.88 / 5.33] | 0.87 / 0.87 | 0.87 / 0.88 | 2.0 / 1.9 | -1.26 / 5.09 | 0.41 / 5.68 | 1 / 1 |

(0.800- = 0.7997.) Level 1 runs 1.15-1.19x faster than zstd-3 on the SE images, so its cap never binds and its cores are 0.

**Database images, 64 KiB** (against each image's CF: zstd-2 on SE and geonames, rescaled; zstd-3 on TLC): every level-3 arm fails the
compress cap (pv@3 0.47-0.65x, pv-cover@3 0.47-0.65x, 5.1-10.6 cores) although its N reaches up to 8.6 pp; every level-1 arm loses bytes
(pv-cover@1 -9.3 to -0.3 %). Vote **0 of 15** for both arms. Full rows: `raw/analysis-a1-tests.csv`.

**Votes (database family).**

| arm | 16 KiB, either level | 16 KiB, one level (level 3) | 16 KiB, cap not applied | 16 KiB, 16 MiB blocks | 64 KiB |
| --- | --- | --- | --- | --- | --- |
| **pv-cover (decides, COVER rule fired)** | **9 of 15**: askubuntu-sq, superuser pg/my/sq, serverfault pg/sq, tlc pg/my/sq | 7 of 15 | 13 of 15 | 9 of 15 | **0 of 15** |
| pv (sensitivity) | 5 of 15: askubuntu-sq, serverfault-sq, tlc pg/my/sq | 5 of 15 | 12 of 15 | 5 of 15 | 0 of 15 |

**D2, database family: marginal, not established (Ruling R-D2).** A1 applied mechanically gives "GO at 16 KiB only" (A1.4: 16 KiB
reaches 9 of 8 needed; 64 KiB confirmatory reaches 0). The maintainer ruled, after seeing these data and saying so, that this is not
established, because three of its deciding margins lie inside measurement noise and four images miss by margins of the same size
(table in §0): the either-level rule (two images, superuser-sq and serverfault-sq, pass only with pv-cover at
level 1, where the dictionary at zstd-1 is faster than zstd-3 and still 3.0-3.1 pp smaller; their level-3 arm fails the decompress cap at
0.796 and 0.7997); the COVER median (1.0081 pp); and three level-3 caps between 0.802 and 0.809. Wilcoxon and the 16 MiB intervals do not
bind (every passing image has LB ≥ 2.6 pp with 16 MiB blocks). pv-cover seeds 2 and 3 were not trained (pv's seed spread is 0.1-0.5 pp).

**What it means for Lunix.** Codec 3 stays reserved and `estimate`, `train` and `apply` stay out of v1 (D7-Lunix in effect). What
the data do show: on database volumes with 16 KiB chunks a per-volume COVER dictionary stores about 5-13 % fewer bytes than zstd-3 on
text-heavy and taxi-trip databases (none on geonames) at 1-4 extra cores per 5 GB/s of ingest, with level-3 compression at the edge of
the cap; at 64 KiB it fails against zstd-2/zstd-3; on OS volumes it loses.

**Deciding experiment for a later release** (as R-D2 describes): (1) a product-level rule fixed in advance: one dictionary level for
all volumes, or a stated per-volume level choice that `estimate` would apply; (2) repeated timing on several hosts or runs, on all three
engines (PostgreSQL, MySQL, SQLite), so that cap margins of 0.002-0.009 are judged against measured run-to-run and host-to-host spread,
with more than one sample draw per image and the tuned libzstd dictionary path measured next to the default one (dictionary arms were
timed only through libzstd's default prepared-dictionary path: dictionary state created by level only, default attach or copy mode);
(3) a pre-stated choice between COVER and fastCover training, so that the arm is not selected by a median that clears its trigger by
0.008 pp. All three are written down before the data are taken; the 8-of-15 vote at 16 KiB, the 64 KiB confirmatory rule and the cap
stay as pre-registered.

**D2c** (computed because the mechanical D2 vote fired; moot under R-D2): per-class pv against single pv on the 9 volumes: **0 of 9**
gain ≥ 1 pp (within ±0.6 pp): NO-GO. (pv-class was trained with fastCover; a COVER pv-class was not measured.)

**Sweep** (pv seed 1, level 3, S against zstd-3 behind the probe, %; level 1 in brackets):

| sample | 16 KiB dict | 32 KiB | 64 KiB | 110 KiB |
| --- | --- | --- | --- | --- |
| host-ext4 0.1 % | -1.40 (-3.56) | -1.31 (-3.50) | -1.19 (-3.49) | -1.10 (-3.53) |
| host-ext4 1 % | -1.40 (-3.54) | -1.23 (-3.41) | -1.03 (-3.40) | -0.87 (-3.45) |
| host-ext4 10 % | -1.37 (-3.49) | -1.22 (-3.39) | -1.02 (-3.32) | -0.89 (-3.34) |
| se-askubuntu-pg 0.1 % | 3.19 (-0.05) | 5.04 (1.01) | 6.25 (1.30) | 6.93 (1.30) |
| se-askubuntu-pg 1 % | 3.50 (0.13) | 4.99 (0.85) | 6.32 (1.34) | 7.17 (1.44) |
| se-askubuntu-pg 10 % | 3.54 (0.06) | 5.02 (0.90) | 6.41 (1.32) | 7.18 (1.35) |

The sample fraction hardly matters above 0.1 %; dictionary size does (110 KiB is 2 pp better than 32 KiB on database pages). A 1 % sample
of a 1 TiB volume is capped at 64 MiB of ranged GETs; memory per volume is the 110 KiB dictionary plus its digested state (not measured).

## 4. Question 3: database pages (D4, D6)

15 images (5 datasets x PostgreSQL 18.6 / MySQL 8.4.13 / SQLite 3.46.1) + 2 controls. Lunix view at 16 KiB: 2.1-4.7 GiB unique per
image, 2,180-5,351 blocks, holes 71-87 %, duplicates ≤ 0.6 %, probe-raw ≤ 3.4 %; DB pages 56-82 % of the unique bytes. `dbd` = DB set
trained on the other schema families (decides); `dbs` = same schema (reported only).

**(a) Constellation view, database-file bytes: S against zstd:3, % [BCa: unit file / unit 16 MiB file extent].** Unit counts at 1 MiB
(files / 16 MiB extents / objects): PostgreSQL 875-912 / 984-1,106 / 3,626-5,515; MySQL 168-174 / 304-465 / 2,907-5,475; SQLite 1 /
96-169 / 1,531-2,703. Every view is under the 1,024-unit reading of §4.4 except some PostgreSQL extent counts (deviation A9); the
extent-level re-bootstrap gives negative upper ends on every image, so the conclusion does not depend on the unit.

| image | dbd@3 4 MiB | dbd@3 1 MiB | N 1 MiB (extents) | cap 1 MiB | dbs@3 1 MiB | pv@3 1 MiB | dbd@3 page | dbs@3 page |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| se-askubuntu pg | -0.19 [-0.23, -0.13 / -0.20, -0.17] | -0.31 [-0.40, -0.20 / -0.34, -0.28] | -0.50 (-0.45) | 0.72 / 1.01 | 0.09 | 0.06 | -1.76 [-2.95, 0.30 / -2.28, -1.16] | 8.49 [6.62, 9.83 / 7.89, 9.05] |
| se-askubuntu my | -0.21 [-0.27, -0.14 / -0.22, -0.19] | -0.38 [-0.49, -0.27 / -0.40, -0.35] | -0.59 (-0.50) | 0.72 / 1.01 | 0.12 | -0.01 | -2.80 | 5.38 [0.06, 9.01 / 4.60, 6.15] |
| se-askubuntu sq | -0.25 (one file) | -0.41 (one file) | - (-0.55) | 0.72 / 1.01 | 0.21 | 0.34 | -1.92 | 13.22 |
| se-superuser pg / my / sq | -0.18 / -0.20 / -0.24 | -0.31 / -0.37 / -0.39 | -0.49 (-0.44) / -0.57 (-0.49) / - (-0.52) | 0.72 / 1.01 | 0.15 / 0.12 / 0.35 | 0.08 / -0.01 / 0.23 | -1.91 / -2.51 / -1.90 | 9.52 / 6.01 / 13.91 |
| se-serverfault pg / my / sq | -0.17 / -0.21 / -0.25 | -0.29 / -0.39 / -0.41 | -0.47 (-0.43) / -0.58 (-0.51) / - (-0.55) | 0.72 / 1.01 | 0.11 / 0.04 / 0.22 | 0.08 / -0.06 / 0.26 | -1.95 / -2.30 / -2.06 | 8.18 / 5.26 / 13.15 |
| geonames pg / my / sq | -0.16 / -0.15 / -0.19 | -0.27 / -0.27 / -0.34 | -0.46 (-0.39) / -0.59 (-0.40) / - (-0.48) | 0.72 / 1.00 | (= dbd) | -0.10 / -0.17 / -0.12 | -2.18 / -4.50 / -3.66 | (= dbd) |
| tlc-2024q1 pg / my / sq | -0.29 / -0.37 / -0.45 | -0.42 / -0.62 / -0.76 | -0.57 (-0.51) / -0.74 (-0.73) / - (-0.87) | 0.78 / 1.00 | (= dbd) | 0.02 / -0.59 / -0.51 | -1.82 / 3.39 / -3.24 | (= dbd) |
| controls pgbench / sysbench | -0.56 / -0.02 | -1.64 / -0.04 | -4.42 (-2.24) / -0.16 (-0.15) | 0.72 / 1.00 | (= dbd) | -0.88 / -0.35 | -0.09 / 2.08 | (= dbd) |

Intervals for every image and arm are in `raw/analysis-a1-desc-db.csv`; SQLite rows have no file-level interval (one file). Page-size LB
of `dbd`: negative except tlc-my; the largest page-size value of any arm is `dbs` on SQLite SE (13.2-13.9 %), under 15 pp and same-schema.

**(b) Lunix view, 16 KiB, DB-page class: S against zstd-3 behind the probe, % [LB 1 MiB / 16 MiB where ≥ 200 blocks]**

| image | ft@3 | dbd@1 | dbd@3 | N dbd@3 | dbs@3 | pv@3 | pv-cover@3 | oracle@3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| se-askubuntu pg / my / sq | 2.42 / 4.24 / 5.39 | -5.20 / -6.98 / -6.10 | -0.52 / -2.78 / -2.13 | -1.06 / -3.21 / -2.50 | 6.95 / 7.12 / 8.34 | 7.33 / 7.06 / 9.47 | 8.70 / 8.12 / 10.72 | 5.34 / 5.30 / 5.93 |
| se-superuser pg / my / sq | 3.29 / 4.89 / 6.00 | -5.45 / -7.15 / -6.20 | -0.47 / -2.46 / -1.83 | -0.99 / -2.91 / -2.19 | 8.12 / 7.53 / 9.18 | 7.70 / 7.24 / 9.38 | 8.75 / 8.35 / 10.47 | 5.97 / 5.83 / 6.50 |
| se-serverfault pg / my / sq | 3.23 / 5.23 / 6.25 | -5.30 / -7.17 / -6.05 | -0.65 / -2.23 / -1.86 | -1.20 / -2.68 / -2.22 | 7.13 / 7.09 / 8.82 | 7.39 / 6.89 / 9.52 | 8.68 / 7.78 / 10.84 | 5.84 / 5.96 / 6.77 |
| geonames pg / my / sq | 0.00 / 0.00 / 0.00 | -10.74 / -11.67 / -6.83 | -2.89 / -4.70 / -3.72 | -3.31 / -5.21 / -4.32 | (= dbd) | 2.37 / -0.93 / 0.80 | 3.55 / -0.72 / 2.01 | 0.60 / 0.10 / 0.59 |
| tlc-2024q1 pg / my / sq | 0.00 / 0.00 / 0.00 | -7.70 / -2.40 / -8.08 | 1.31 / 4.36 / -2.77 | 1.14 / 4.06 / -3.33 | (= dbd) | 11.27 / 5.16 / 5.31 | 12.01 / 5.24 / 5.88 | 1.80 / 4.53 / 1.59 |
| controls pgbench / sysbench | 0.00 / 0.00 | -14.43 / 3.92 | -9.33 / 2.11 | -12.46 / 1.54 | (= dbd) | 27.48 / 1.01 | - | 13.25 / 2.17 |

(se-askubuntu-pg dbd@3: -0.52 [-0.66]; LBs for every cell in `raw/analysis-a1-desc-db.csv`.) **Per engine** (median N over the 5 datasets,
pp; datasets with Holm p < 0.05, S > 0 and cap passed): D4-L dbd@1 -5.67 / -7.29 / -6.33 (0 engines); dbd@3 -1.06 / -2.91 / -2.50 (0; cap
fails); D4-C -0.49 / -0.59 / -0.41 (0); D6 at 1 MiB the same against 3 pp (0); with 16 MiB extents -0.44 / -0.50 / -0.55 (0); 4 MiB
-0.26 / -0.31 / -0.25. **D4 NO-GO, D6 NO-GO**, no chunk-size open question. Per engine the answer is the same: a schema-disjoint
dictionary does not beat plain zstd-3 on pages and costs 0.3-0.8 % at Constellation sizes; the schema (same-schema 7-9 % on pages) or the
volume's own data (pv-cover 5-12 % on pages) is what helps.

## 5. Question 4: Constellation file-type set (D5, A1.5)

Per-object pass on the four file corpora (F3): objects = whole files under 4 MiB, else 4 MiB chunks; identical files once; arms zstd:3,
`select(name, head, len)` + dictionary at zstd:3 (`ft@3`), and the per-object oracle over the 33 dictionaries; "under 64 KiB" by object
length. Unit of resampling = file (16 MiB file extents in the second interval). Cap and cores from the vm-ubuntu file cells (the only
file timing; applied to all four corpora).

| corpus | population | objects / files | ft@3 S [file / extent] | oracle@3 S | selector loss (pp) | cap c / d | delta-cores at 1 GB/s | N (extents) | floor |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| vm-ubuntu | under 64 KiB | 66,829 / 66,829 | 2.31 [2.27, 2.36 / 2.27, 2.36] | 3.03 | 0.72 | 0.14 / 0.92 | 14.1 | 0.86 (0.86) | 5 |
| vm-ubuntu | all | 69,475 / 69,356 | 0.74 [0.59, 0.83 / 0.66, 0.80] | 1.07 | 0.34 | 0.39 / 0.97 | 3.4 | 0.25 (0.32) | 1 |
| vm-debian | under 64 KiB | 18,263 / 18,263 | 1.56 [1.50, 1.63 / 1.50, 1.63] | 2.79 | 1.23 | 0.14 / 0.92 | 14.1 | 0.09 (0.09) | 5 |
| vm-debian | all | 19,404 / 19,389 | 0.43 [0.32, 0.49 / 0.33, 0.49] | 0.91 | 0.48 | 0.39 / 0.97 | 3.4 | -0.02 (-0.01) | 1 |
| vm-fedora | under 64 KiB | 19,618 / 19,618 | 1.29 [1.24, 1.34 / 1.24, 1.34] | 2.45 | 1.16 | 0.14 / 0.92 | 14.1 | -0.17 (-0.17) | 5 |
| vm-fedora | all | 21,427 / 21,354 | 0.27 [0.22, 0.31 / 0.23, 0.30] | 0.65 | 0.39 | 0.39 / 0.97 | 3.4 | -0.12 (-0.12) | 1 |
| host-files | under 64 KiB | 95,300 / 95,300 | 2.69 [2.65, 2.74 / 2.66, 2.74] | 3.33 | 0.64 | 0.14 / 0.92 | 14.1 | 1.24 (1.24) | 5 |
| host-files | all | 101,176 / 100,587 | 0.50 [0.44, 0.56 / 0.47, 0.54] | 0.71 | 0.21 | 0.39 / 0.97 | 3.4 | 0.10 (0.12) | 1 |

**D5: NO-GO for this release.** The decision table's own NO-GO clause holds: both floors are missed on all four corpora, and they are
missed before any CPU is charged (S under 64 KiB 1.29-2.69 % against 5 pp; all objects 0.27-0.74 % against 1 pp). R5 passes (selector loss
0.6-1.2 pp under 64 KiB), so the selector is not why the saving is small: even the per-object oracle reaches only 2.5-3.3 % under 64 KiB.
The hard cap fails as well (0.14x and 0.39x zstd:3 compress); that failure is caused by the selector's content classifier (about 90
microseconds per call); dictionary compression alone runs at 0.89x (under 64 KiB) and 0.96x (all objects) and would pass. A faster or
name-first selector would remove the cap failure but not the saving shortfall measured here.

## 6. Decision table applied

| id | measured | N (pp) | cap | votes | verdict |
| --- | --- | --- | --- | --- | --- |
| D1 16 KiB | best arm bs-dist@3 vs zstd-3/probe; 3.5 cores | -1.09..-0.20 (16 MiB: -1.16..-0.38) | 0.75x | 0 of 4 (any arm 0; 3-of-4 sensitivity 0) | **NO-GO**, decisive among the measured candidate sets (oracle LB 0.84-1.40) |
| D1 64 KiB | best arm bs-dist@3 vs zstd-1/probe; 7.7 cores | 2.12-2.54 (1.97-2.13) | 0.45x | 0 of 4 | **NO-GO** (oracle LB 3.18-3.53; 16 MiB 3.01-3.10: not decisive) |
| D2 raw | pv vs CF | 16 KiB -1.22..-0.58; 64 KiB 1.95-2.30 | 0.80x / 0.45x | 0 of 4 | **NO-GO** |
| D2 database | pv-cover vs each image's CF (own cell); COVER median 1.0081 pp | passing: 3.0-13.1 | passing: 0.802-0.94 compress | 16 KiB 9 of 15; 64 KiB 0 of 15 | **marginal, not established** (R-D2; A1 mechanically "GO at 16 KiB only"; one level 7 of 15; pv 5 of 15) |
| D2c | pv-class vs pv on the 9 | within ±0.6 | - | 0 of 9 | **NO-GO** (moot) |
| D3 | D1 does not fire | -1.65..-0.92 / -0.64..0.07 | 0.75x / 0.45x | 0 | **NO-GO** |
| D4-L | dbd@3, DB pages 16 KiB, median per engine | -1.06 / -2.91 / -2.50 | fails | 0 of 3 | **NO-GO** (consumer D1 does not fire) |
| D4-C | dbd@3, 1 MiB | -0.49 / -0.59 / -0.41 | 0.72-0.78x | 0 of 3 | **NO-GO** (consumer D6 does not fire) |
| D5 | ft@3 vs zstd:3, per-object pass, 4 corpora | under 64 KiB -0.17..1.24; all -0.12..0.25 | 0.14x / 0.39x | 0 of 4 meet the floors | **NO-GO** (table and cap) |
| D6 | dbd@3, 1 MiB | -0.49 / -0.59 / -0.41 | 0.72-0.78x | 0 of 3 | **NO-GO**; no open question |
| D7 | derived | | | | Constellation and zdicts: fires; Lunix: does not fire mechanically (A1: D2 database "GO at 16 KiB only"); its consequences (codec 3 reserved, `estimate`/`train`/`apply` out of v1) are applied by Ruling R-D2, made after the data |

Holm within each family over all its rows (`families-a1.csv`: D1 48, D2 144, D2c 84, D3 24, D4 45, D5 8, D6 30), separately for 1 MiB
and 16 MiB units (every test re-run with both). No pass flag changes with 16 MiB units (399 tests). 16 MiB half-widths are 3.2x the 1 MiB
ones (median; range 0.1-4.7); with 16 MiB blocks eight cells exceed the 1.5 pp half-width (all level-1 or 64 KiB rows of geonames-pg and
the TLC images, none of them deciding). §1.3 expectations: "dictionaries lose at 256 KiB and more": confirmed. "D4-L and D1 at 16 KiB
are open": resolved NO-GO. "D2 fails against a block set on OS images": confirmed. "D2 open on database images": still open (marginal, not established).

## 7. Proposals (not edits)

1. **ADR-30:** codec 3 stays reserved; `estimate`, `train`, `apply` and `--classes` leave v1 (R-D2). Record the database-volume result
   and the deciding experiment of §3 as the condition for reopening `train`/`apply` in a later release; if reopened, the scope the data
   point to is 16 KiB database volumes with an `estimate` that judges bytes and throughput against the plain counterfactual, never
   64 KiB or OS volumes, and no per-class dictionaries (D2c).
2. **ADR-12 / ADR-5:** at 16 KiB zstd-3 behind the probe passes the cap on OS and database volumes (0.80-0.92x) and saves 0.8-4.9 % over
   zstd-1; at 64 KiB zstd-2 passes everywhere measured (0.81-0.96x) and saves 1.6-6.1 % on the SE, geonames and VM samples (-0.9 to
   +1.9 % on TLC). A
   per-chunk-size default level is free and should be measured volume-wide.
3. **zdicts:** add no block, file-system or database set; the file-type set stays. The classifier's cost is the first obstacle for every
   consumer measured here; a faster selector is the precondition for any re-test, but the D5 saving would still miss its floors.
4. **Constellation:** keep zstd:3, no dictionaries. Optional, code-free: zstd:4 passes the cap on database objects (0.88-0.94x) and saves
   0.2-1.8 % there.

## 8. Deviations, threats, reproducibility

### Deviations (1-17 execution, B14-B17 database part, T1-T8 timing, 18-30 first analysis, F1-F4 follow-up measurement, A1-A12 this analysis)
1. The distribution's default login is excluded from the identity scrub; literal application would delete the dpkg database and apt sources. Hostname, addresses, MACs and other uid >= 1000 logins were scrubbed.
- 2-5. (2) Python `sqlite3` module instead of the CLI `.import` for SQLite loads (same library; needed for exact NULLs). (3) Sizes come from a C measurement tool (libzstd 1.5.7, dictID flag off, content size on) instead of the Rust crate; LZ4 probe via liblz4. (4) mysqld is run from a copy of the binary to avoid the distribution AppArmor profile; buffer-pool, redo and flush settings are raised for load speed (do not change page content). (5) The bs-* training sample unit is 16 KiB for both chunk sizes.
- 6-8. (6) Arms are evaluated behind the probe; dictionary arms are not compressed on probe-raw chunks (raw by definition). (7) Class-masked dictionaries (bs-*, pv-class) run only on chunks of their cheap class, other chunks plain. (8) The 10 % sweep sample is not capped at 64 MiB (the 0.1 % and 1 % cells are).
- 9-13. (9) The OS tree is re-copied for each file system, so the three host images differ by whatever the live host changed in between. (10) The first Rocky pool run produced incomplete xfs and btrfs copies and was rerun (second run complete, rsync sizes checked). (11) The Fedora root is btrfs with subvolumes, labelled from the top-level mount (subvolid 5). (12) xfs `dir` blocks come out as 0 (FIEMAP reports no extents for them); xfs metadata comes from `xfs_db blockget -v`. (13) Queue A uses `zmeasure2`/`fiemap2` and its own tracking file; bs-dist-deb reached 24 MiB in all three classes.
14. vm-fedora (701 blocks) and vm-debian (936) are below the 1,024-block floor: exploratory, never vote; lead ruling: a raw-device GO needs all 4 eligible corpora; 3-of-4 is a sensitivity only.
15. The raw-device `oracle` omits the DB set (a new measurement would be needed); D1's decisive clause is stated "among the measured candidate sets".
16. The 16 MiB-block sensitivity is a resampling choice (block id >> 4), not a new measurement; A1.6 now prints it everywhere.
17. `collect.sh` takes the host from the environment (`DICTVAL_HOST`).
- B14-B17 (database part): B14 only `dbzd.py`, `dbsens.py`, `dbagg.py` added to the db scripts; B15 database `ft`/`oracle` are Lunix-view only, oracle = best of the 33, `dbd` and plain; B16 the 10 % pv sweep is capped at 256 MiB in the database variant; B17 controls use the same server settings as the real loads.
- T1-T8: see "CPU timing" below.
18. First-analysis families: D2 enumerated with both levels and "a volume passes if either level passes", fixed before the first tests; A1 keeps both levels (`families-a1.csv`). A1 does not rule on levels; this rule now decides D2 (one level: 7 of 15).
19. Superseded by A1.5 (the D5 saving pass now exists).
20. Superseded for D2 by A1.2 (own cells). Still applies to D4/D6: Constellation views use the PostgreSQL SE and TLC object cells (worse of both for geonames), and D4-L's `dbd` arm uses the image's own cell with `pv` standing in for the untimed `db` arm.
21. Raw-device counterfactual at 64 KiB stays zstd-1 (protocol names zstd-1/zstd-3 for Lunix); zstd-2 passes the cap there and would only lower N.
22. Superseded by A1.1 (the hybrid "best of CF and shipped sets" comparator and the "stricter reading decides" tie-break were written after the first timing data and aggregate point estimates were known; they are no longer used).
23. Statistics: Wilcoxon signed-rank two-sided over units, zero differences dropped; BCa acceleration from the leave-one-unit-out jackknife; resamples paired across arms of a population; seeds from 20261112 by SHA-256.
24. Superseded: every test is now re-run with 16 MiB units and Holm is computed over the full family for each unit.
25. SQLite databases are one file: no file-level interval (rows now blank, not LB = S); the 16 MiB-extent interval exists (96-169 extents).
- 26-28. (26) Per-type data rows use the `ft` selection of the chunk head as the type. (27) Controls are tested with unadjusted p and never enter a family or a vote. (28) `python3-matplotlib` (3.10.7) installed on the test host with apt for the figures.
29. Per-chunk results never leave the test host; `decide.py` and `figures.py` regenerate every table and figure byte for byte from `raw/` alone (checked again for A1).
30. Seeds 2 and 3 give the spread of pv only; pv-cover has seed 1 only.
- F1-F4 (follow-up measurement agent): F1 one block-stratified sample per database image serves both chunk sizes; F2 the timing sampling was paused and resumed around another project's soak (quiet-host rule kept for every timed pass); F3 D5 mount options and unreadable files: vm-fedora mounted with `subvol=root`; files skipped as unreadable: vm-ubuntu 17, vm-debian 8, vm-fedora 31, host-files 27 (19.7 KB, 6.8 KB, 7.4 MB, 154 MB); host-files = the files of the host-ext4 image (5.24 GB unique); "under 64 KiB" by object length; F4 pv-cover = `zstd --train-cover=steps=16`, same 1 % seed-1 sample, 110 KiB.
- A1. `families-a1.csv` (383 tests) was written before the A1 analysis ran but after the follow-up inputs existed; before writing it I had seen the per-image timing ratios, the D5 aggregate CSV and the header rows of the pv-cover CSV. Its changes from `families.csv` are the ones A1 prescribes (D2 comparator = CF, 60 pv-cover rows, D5 named by corpus), none chosen on data.
- A2. COVER rule unit: "pp" is the difference of S against the counterfactual (pp of counterfactual bytes), median over the 15 images at 16 KiB; A1 names no level, so both are reported and both exceed 1 pp; relative to pv's own bytes the level-3 median is 1.090 %.
- A3. The counterfactual of a database image is the best plain level among zstd-1/-2/-3 that passes the cap against zstd-1 in its own cell, ranked by the cell's stored bytes; zstd-4 and above were not timed volume-wide (failed on every earlier Lunix cell).
- A4. A volume passes D2 if either level passes (deviation 18); the one-level reading is shown.
- A5. 64 KiB is confirmatory per A1.4; `decide.py` no longer requires it for D2 to fire (review item 4) and records "GO at 16 KiB only" mechanically; Ruling R-D2 (made after the data) overrides that record with "marginal, not established" and keeps D7-Lunix in effect. `raw/analysis-a1-votes.csv` holds the mechanical votes.
- A6. zstd-2 counterfactuals (64 KiB, 12 images) have no per-chunk measurement: tests run against zstd-1 with S and LB rescaled by the cell's zstd-2/zstd-1 byte ratio (1 GiB sample); cap and cores are exact; Wilcoxon p is against zstd-1.
- A7. D2c compares fastCover pv-class with fastCover pv (no COVER pv-class exists).
- A8. D5 cap and cores come from the vm-ubuntu file cells for all four corpora; the D5 vote is computed from the tests (review item 5); D7-Constellation derives from D5 and D6.
- A9. §4.4's 1,024-unit reading is not met by the Constellation database views (1-912 files; 96-1,106 extents); disclosed with unit counts and the extent-level re-bootstrap, which does not change any conclusion (review item 6).
- A10. Rows with a single resampling unit have blank intervals and p = 1 (review item 8).
- A11. `raw/analysis-a1-desc-*.csv` splits the descriptive table in two files to stay under 1 MiB.
- A12. `decide.py` after its first run (first analysis): labels "65 KiB" corrected to 64 KiB; the 16 MiB D2 vote fell back to 1 MiB rows for database images that were not re-run at 16 MiB (before: counted one image); added the 16 MiB flip list, the half-width summary, the pv-cover and counterfactual-sensitivity lines, the "cap not applied" vote, and reading `analysis-*` files from `raw/`. No primary vote changed. For A1 it was rewritten (`decide.py`; the first version is `decide_v1.py`), as were `analyze.py` and `figures.py` (`*_v1.py` kept).

### Threats to validity (updated)
| threat | observed |
|---|---|
| Offline chunking vs a stored Lunix volume | unchanged, unverified (M18b validation cell) |
| Images are fresh or one tree | the three host corpora are one tree in three file systems and behave identically (±0.05 pp): the raw-device votes rest on two contents |
| Timing on another host | A1.2 removed the stand-ins for D2: each database image has its own volume-wide cell; the mechanical D2 vote still rests on caps of 0.802-0.809 with a cell spread up to 1.9 %, and on one sample draw per image (F1); this is why R-D2 records it as marginal, not established |
| Trainer randomness | pv seed spread 0.1-0.5 pp; pv-cover seed 1 only; the COVER median clears its trigger by 0.008 pp |
| Dictionary timing path | Dictionary arms are timed through libzstd's default prepared-dictionary path (dictionary state created by level only, default attach or copy mode); a tuned path was not measured. One sample draw per image. |
| Block dependence | 16 MiB intervals 3.2x wider than 1 MiB (median); no pass flag changes; Wilcoxon p-values from 1 MiB blocks are not calibrated |
| Constellation unit counts | under 1,024 files on every database view; extent-level re-bootstrap agrees |
| Same-schema leakage in the DB set | the schema-disjoint set loses; same-schema wins, as the threat predicted |
| D5 file populations | unreadable files skipped (F3: up to 154 MB on host-files); vm-fedora's root subvolume only |
| vm-fedora transparent compression | 67 % probe-raw; exploratory |

### Reproducibility appendix
- **Hosts (categories):** test host 16 vCPU, about 30 GiB, local NVMe (analysis pinned to 2 CPUs, `nice -n 19 ionice -c3`, one process); quiet sister host of the same class for timing. No host detail appears in this directory.
- **Versions:** libzstd 1.5.7, liblz4 1.10.0, zdicts v0.0.1 (commit 3b8645ef), PostgreSQL 18.6, MySQL 8.4.13, SQLite 3.46.1; Python 3.14.4, numpy 2.3.5, scipy 1.16.3, matplotlib 3.10.7.
- **Seeds:** master 20261112; bootstrap seed per population and unit = SHA-256 of "20261112|boot|<population>|<unit>".
- **Commands** (test host, `~/dictval`, each as `nice -n 19 ionice -c3 taskset -c 14-15 python3 -I`): `ana/families.py ana/a1/families.csv a1`; `ana/analyze.py ana/a1 10000` (about 4.5 min); `ana/decide.py ana/a1 ana/raw`; `ana/figures.py ana/a1 ana/raw ana/a1fig`. From `raw/`: `decide.py raw raw`, `figures.py raw raw <dir>` (byte-identical tables and SVGs, checked). First analysis: `*_v1.py`, outputs `raw/analysis-*`.
- **Hashes (SHA-256, first 8):** families-a1.csv 1ce6c2b7, tests df1f71b4, desc (unsplit) 324495d4, meta 855acff1, votes 5af43b32; analyze.py 02e49cb6, decide.py e498a63e, figures.py 82dd43c1, families.py b45d9424; timing-db.csv bac85ddf.
- **`raw/` inventory:** `sources.lock.tsv`; raw devices `arms-*.csv`, `zdarms-*.csv`, `ftsel-*.json`, `labels-*.json`, `shares.csv`; databases `db_lunix_arms.csv`, `db_cv_arms.csv`, `db_shares.csv`, `db_pvsamples.csv`, `db_sweep.csv`, `db_sens_fs_{xfs,btrfs}.csv`, `db_pvcover_arms.csv`; files `d5_objects.csv`; timing `timing.csv`, `timing-ratios.csv`, `timing-db.csv`; first analysis `families.csv`, `analysis-{tests,desc,votes}.csv`, `analysis-meta.json`; A1 `families-a1.csv`, `analysis-a1-tests.csv`, `analysis-a1-desc-lunix-raw.csv`, `analysis-a1-desc-db.csv`, `analysis-a1-votes.csv`, `analysis-a1-meta.json`. Every file < 1 MiB.
- **Execution record.** 14 downloads verified; host-tree copy 11.9 GiB, 1 identity file deleted; pool = Alpine, Rocky, openSUSE, Arch x native/ext4/xfs/btrfs; queues A-A4, ZD, B1-B3, timing and the A1 follow-up queues finished with 0 FAILED (one failed Rocky step in queue A2 was rerun, deviation 10).

#### CPU timing (quiet sister host)
Files: `raw/timing.csv` (first campaign, 286 rows) and `raw/timing-db.csv` (A1.2: 15 database images x 16/64 KiB, arms zstd-1/-2/-3, pv@1/3, pv-cover@1/3, compress and decompress, volume-wide block-stratified 1 GiB samples of all unique non-hole chunks; max/min ≤ 1.019), plus `raw/timing-ratios.csv`. Method: one pinned core, nothing else running, 1 warm-up and 7 timed passes over an in-memory sample, median and min/max; Lunix arms behind the ADR-12 probe; decompress = what the read path does.
**First-campaign cells:** vm-ubuntu 16 and 64 KiB; vm-ubuntu file objects (all, and under 64 KiB); PostgreSQL SE and TLC: Constellation 4 MiB and 1 MiB, Lunix DB-page 16 KiB. **Results (throughput ratio = comparator cost / arm cost; * below 0.8):** z3 vs z1: 0.83 (vm 16K), 0.76* (vm 64K); database volumes 0.80-0.92 (16K), 0.71*-0.86 (64K); z2 vs z1 database volumes 0.81-0.96. bs-dist/bs-xfs L1 0.90 / 1.08 vs z1 / z3 at 16K, L3 0.63* / 0.75*; at 64K L1 0.85 / 1.12, L3 0.45* / 0.59*. pv L3 vs z3: 0.80 (vm 16K), 0.60* (vm 64K); database volumes 16K pv@3 0.74*-0.92, pv-cover@3 0.74*-0.94; level 1 0.97-1.19. ft (Lunix) 0.22-0.29* at 16K, 0.37-0.59* at 64K; ft Constellation 0.39* (all objects), 0.14* (under 64 KiB); without the selector pass 0.96 and 0.89. Constellation database objects: pv/db vs z3 0.88-0.89 at 4 MiB, 0.72*-0.80 at 1 MiB; z4 passes there (0.88-0.94). Decompress passes 0.8x for every arm except level-3 dictionaries on SQLite SE volumes at 16 KiB (0.796-0.806) and level-3 dictionaries at 64 KiB on the SE volumes (0.69*-0.82). The selector alone costs 5.9 (vm 16K), 7.8 (SE pages), 6.4 (TLC pages), 1.5 (vm 64K), 3.3 (all file objects), 13.8 (under 64 KiB) CPU-s/GB, about 90-100 microseconds per call.
**Timing deviations.** T1: dictionary arms use the C library (libzstd 1.5.7); only the selector runs in Rust. T2: host-tree and raw-device cells use the vm-ubuntu sample and dictionaries ("stand-in"). T3: no cpufreq governor in the guest. T4: the first campaign's Lunix DB-page view was cut from data-file bytes (superseded for D2 by the volume-wide cells). T5: the first campaign timed PostgreSQL only (superseded for D2 by A1.2). T6: `oracle` and database `pv-class` not timed. T7: sample bytes 1.07 GB; the under-64-KiB file cell is 0.26 GB. T8: TLC data relayed to the timing host and deleted there afterwards; only timings remain.

## 9. Figures
- [Saving by arm per corpus (Q1)](figures/fig-q1-arms.svg): S with BCa intervals, 1 MiB (thin bar) and 16 MiB (wide pale bar), D1 floor 3 pp.
- [Per-class saving (Q1)](figures/fig-q1-class.svg): ft@3, bs-dist@3, oracle@3 per class, 16 KiB.
- [Per-volume dictionaries on database images (Q2)](figures/fig-q2-pv.svg): N of pv and pv-cover (better level) against each image's counterfactual at 16 KiB; open markers fail the cap; D2 floor 2 pp.
- [Sweep (Q2)](figures/fig-q2-sweep.svg): pv@3 saving by dictionary size and sample fraction.
- [Database files by object size (Q3)](figures/fig-q3-size.svg): median DB-set saving at level 3 against zstd-3.
- [Constellation file objects (D5)](figures/fig-d5.svg): ft@3 and the per-object oracle against zstd:3 with file-level intervals, floors 5 and 1 pp.
- [Compress CPU against saving](figures/fig-cpu.svg): vm-ubuntu 16 and 64 KiB and the se-askubuntu x PostgreSQL volume cell; dashed line = 0.8x cap against zstd-1.
