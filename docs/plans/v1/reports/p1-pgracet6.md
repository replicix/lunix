# p1-pgracet6: real-daemon T6 of the short segment grace (`gc_pressure_grace_ms`), arms B, C, C2

Branch `p1/pgracet6` from `origin/main` 4e86c670. Measurement only, never merged: no product code changed. The harness of `p1/c5t6` was brought over
(`tests/perf/*`, `bench/phase1/perf/*`); new: `tests/perf/pgracet6.sh` (driver) with `pgracet6-blocks.txt` (the block list), a fio read-back
verification phase after rep 5 (`bench/phase1/perf/fio/w4kv.fio`, wired into `tests/perf/c5t6-cells.sh`), `bench/phase1/perf/pgracet6.py` (tables and
gates), an `[pgracet6]` entry in `tools/ownership.toml`. Results: `bench/phase1/perf/results/pgracet6/` (`tables.md`, `summary.json`, one directory per
block; paths scrubbed to `<spike-dir>`, `<tree>`, `<home>`). Everything ran on the remote test host (kernel 7.0.0, 16 CPUs, nothing else running), one
release build of main 4e86c670 (one tree, one target directory; sources touched before the sync), 20 GiB tmpfs for the store, 16 GiB integrity volume, 1 GiB
journal, 4 queues, daemon on CPUs 0-7, fio on 8-15, `destage_workers = 8`, 2 GiB working set, w4k 60 s + 5 s ramp, QD 128 x 8 jobs, 5 reps per block.
Every arm: `gc_condemn_on_cut = "on"`, `gc_fast_recheck = true`, `throttle_pace_store_bytes = true`, `log_level = debug`; B `gc_pressure_grace_ms = 0`,
C 5000, C2 2000. 24 blocks (64K w4k 4 per arm, 16K w4k 2 per arm, 64K zipf 2 per arm), arms interleaved
(64K: B C C2 C2 B C C C2 B C2 C B; 16K: C B C2 C2 B C; zipf: B C2 C C C2 B). Each block: fresh daemon and volume, started with the 1-minute load
below 1 and the 5-minute load below 2 (start loads 0.00-0.10 / 1.96-1.99, all 24 recorded in `tables.md`), `lunix fsck --deep` at the end, 24 of 24 driver rc 0.
Onset is the collector's uncoalesced line `gc: the engine's pressure level changed raised=true`.

## Verdict

| # | gate | verdict |
|---|---|---|
| 1 | safety, every C and C2 block | **PASS** (16 of 16 blocks; also the 8 B blocks) |
| 2 | rep 1, 64K, C | **FAIL on one clause, by one block**: first delete <= 3 s 4 of 4, 0 stops 4 of 4, throttled <= 22 s 4 of 4, >= 6 GiB in 5 s 4 of 4; longest wait <= 5 s only 3 of 4 (b07: 5.4 s; median 2.8 s). C2 passes all five 4 of 4 |
| 3 | reps 2-5, 64K, C against B | **PASS** (IOPS +171 %, p99.9 0.65 s against 1.69 s, longest wait 2.1 s against 4.5 s, at most 1 rep with a stop per block) |
| 4 | 16K and zipf, C against B | **FAIL on one clause**: zipf compaction volume 85.7 % of B (limit 90-115 %); IOPS +27 to +68 %, mark share 8.5 % at 16K (B 16.5 %) and 3.3 % at zipf (B 4.8 %) pass |
| 5 | cost | **FAIL on one clause**: cut cost per journal byte of C at zipf +4.8 % of B (limit 3 %); evidence objects 0.52-0.84 x B pass; cut cost 64K -2.2 %, 16K +2.5 % pass |
| 6 | C2 against C | **5 s stays**: 64K reps 2-5 C2 IOPS +3.5 % (needs +10 %), longest wait -0.29 s (needs -1 s) |

No product bug found. Gates were applied exactly as written in the design note; where a clause is ambiguous the reading is stated below.

## Gate 1: safety

All 24 blocks: `fsck --deep` clean (exit 0), fio errors 0 in every repetition and in the verify phase (crc32c read-back while the collector works,
30 s; 1.6-2.0 M reads per C or C2 block), EIO/ENOSPC log lines 0, `fulls + enospc` 0, `read_revalidate_exhausted` 0,
`extent_auth_failures` 0, `proposal_named` 0, `drift_segments` 0, `hold_footer_errors` 0, `proposal_uncondemned` and `pgrace_uncondemned` 0, no "reference table"
lines. `pgrace_min_wait_ms` is at or above the key in every block: C 5000-5031 ms (least 5000), C2 2003-2108 ms (least 2003). No WARN or ERROR line in any
C or C2 log. (The only warnings are in B blocks b01, b09, b12: "commit headroom is short"; b09 also "writes wait for space" and "urgent compaction stopped error=no space".)
`read_stale_age_min/max_ms` is 0 / 0 in all blocks: no read ever lost its extent (`read_revalidated` 0), so there is no age sample.

**Write throttle off while headroom is low.** Every `write throttle off` inside a repetition was compared with the 2 s status samples. In the 16 C and C2 blocks none is
followed (within 2.5 s) by a headroom below `soft`; in rep 1 of the C and C2 blocks the off lines come at 18, 30-36 and 44-52 s with headroom 5.0-12.5 GiB
against soft 2.6-2.9 GiB. All 56 flagged lines are in B blocks (for example b01 rep 1 at 33.6 s: next sample 1.4 s later, `slowdown`, headroom 2,475 MiB, soft 2,574 MiB;
b05 rep 1 at 33.6 and 49.3 s: headroom 2,015 and 1,917 MiB against soft about 2,570 MiB), as in the earlier run. Quoted lines of the first: see `tables.md`
(the table of lines per block).

## Gate 2: rep 1, 64K w4k (seconds after the onset line)

| block | arm | first delete s | stops | longest wait s | throttled s | GiB deleted <= 5 s after onset (pass lines / proposal lines) | IOPS | p99.9 s |
|---|---|---|---|---|---|---|---|---|
| b01 | B | 17.12 | 0 | 13.1 | 34.8 | 0.00 / 0.00 | 33,627 | 2.37 |
| b05 | B | 15.12 | 3 | 13.3 | 34.1 | 0.00 / 0.00 | 33,465 | 1.99 |
| b09 | B | 16.34 | 1 | 13.1 | 27.4 | 0.00 / 0.00 | 32,259 | 1.79 |
| b12 | B | 16.76 | 1 | 13.3 | 36.9 | 0.00 / 0.00 | 31,372 | 2.30 |
| b02 | C | 0.08 | 0 | 2.2 | 4.8 | 9.04 / 9.04 | 62,174 | 0.62 |
| b06 | C | 0.09 | 0 | 3.4 | 2.9 | 9.11 / 9.11 | 63,327 | 0.63 |
| b07 | C | 0.09 | 0 | **5.4** | 5.0 | 8.36 / 8.36 | 59,441 | 0.68 |
| b11 | C | 0.08 | 0 | 1.2 | 5.3 | 9.16 / 9.16 | 65,799 | 0.61 |
| b03 | C2 | 0.10 | 0 | 1.5 | 3.3 | 12.05 / 11.52 | 67,683 | 0.62 |
| b04 | C2 | 0.09 | 0 | 1.8 | 3.7 | 11.81 / 11.18 | 66,645 | 0.62 |
| b08 | C2 | 0.09 | 0 | 1.0 | 3.5 | 11.66 / 9.80 | 68,239 | 0.62 |
| b10 | C2 | 0.08 | 0 | 0.9 | 4.8 | 12.09 / 11.72 | 68,715 | 0.62 |

Medians (n = 4):

| arm | first delete after onset s | stops (blocks with a stop) | longest wait s | throttled s | GiB <= 5 s of onset | IOPS | p99.9 s |
|---|---|---|---|---|---|---|---|
| B | 16.55 | 1 (3) | 13.2 | 34.4 | 0.00 | 32,862 | 2.14 |
| C | 0.09 | 0 (0) | 2.8 | 4.9 | 9.07 | 62,750 | 0.63 |
| C2 | 0.09 | 0 (0) | 1.2 | 3.6 | 11.93 (proposal lines 11.35) | 67,961 | 0.62 |

Readings: "first delete" is the earlier of the first `a proposed segment was deleted` line and the end of the first pass line that deleted anything (the latter
0.45-1.14 s after the onset, an upper bound). "GiB deleted within 5 s" is bytes freed by pass lines ending in the 5 s after the onset (all deletes); the
proposal-line figure (exact times, proposals only) agrees within 2 GiB and is above 6 GiB everywhere. Each item of the gate is read as "in 4 of 4"; read as
a median, or as 3 of 4 for the longest wait (as the plan for B did), C passes. The first delete 0.08-0.10 s after the onset is the backlog of segments condemned
more than 5 s earlier, deleted by the first recheck after the onset (the shrink-at-onset rule of the design).

## Gate 3: reps 2-5, 64K w4k (16 reps per arm)

| arm | stops (reps with a stop) | longest wait s med / p90 / worst | IOPS med / p10 / worst | p99.9 s med / p90 / worst | throttled s med |
|---|---|---|---|---|---|
| B | 1 (1) | 4.5 / 5.2 / 6.5 | 22,817 / 20,634 / 17,458 | 1.69 / 1.99 / 2.57 | 34.1 |
| C | 1 (1) | 2.1 / 3.9 / 5.1 | 61,928 / 55,916 / 53,123 | 0.65 / 0.71 / 0.74 | 5.3 |
| C2 | 0 (0) | 1.8 / 3.9 / 4.7 | 64,098 / 59,960 / 58,426 | 0.64 / 0.67 / 0.69 | 4.3 |

C against B: IOPS +171.4 %, p99.9 and longest wait far below B, reps with a stop per block 1, 0, 0, 0 (b02 has one): **PASS**. Per-block lists in `tables.md`.
Note that B's reps 2-5 IOPS (22.8 k) equals the c5t6 B figure (22.4 k); the pressure grace removes the throttling that bounds it (throttled 5 s against 34 s of 65).

## Gate 4: 16K and zipf, C against B

| cell | group | IOPS B / C / C2 | longest wait s B / C / C2 | throttled s B / C / C2 | mark share % B / C / C2 (reps 2-5) |
|---|---|---|---|---|---|
| 16K | rep 1 | 50,410 / 64,258 / 63,832 | 1.5 / 0.9 / 1.0 | 22.5 / 4.9 / 4.6 | |
| 16K | reps 2-5 | 48,078 / 63,328 / 63,056 | 1.5 / 1.0 / 1.0 | 31.7 / 7.1 / 5.7 | 16.5 / 8.5 / 8.3 |
| zipf | rep 1 | 41,729 / 70,158 / 70,763 | 13.0 / 0.6 / 0.6 | 25.8 / 2.1 / 1.5 | |
| zipf | reps 2-5 | 50,948 / 71,878 / 72,337 | 1.5 / 0.6 / 0.6 | 37.0 / 3.4 / 2.8 | 4.8 / 3.3 / 2.5 |

Compaction written per block (reps 1-5): 16K B 10.8, C 9.5, C2 9.0 GiB; zipf B 13.1, C 11.2 (**85.7 % of B, outside 90-115 %**), C2 10.8 (82.8 %). The miss is in
the low direction (less compaction work) while the guest wrote 40 % more; segments compacted per block are higher (zipf 9,927 against 6,781). The 16K mark share,
a known miss at 16.3 % in c5t6, is 8.5 % under C. Gate 4: **FAIL on the zipf compaction clause only**.

## Gate 5: cost

* `read_revalidated`: **0** in all 24 blocks (C 14.7 M reads, C2 15.0 M, B 7.4 M; the reads are the verify phase's read-back, the measured reps only write), so 0.00
  per million reads (an upper bound of about 0.2 per million at C's 14.7 M reads, 95 %). `read_revalidate_exhausted` 0, `extent_auth_failures` 0, `read_stale_age`: no sample (0..0 ms).
* pgrace: `pgrace_deletes` per block C 8,378-15,408 (total 100,384), C2 8,705-16,170 (104,272), B 0; `pgrace_bytes` C 128-239 GiB per block (1,542 GiB in total),
  C2 133-250 GiB (1,602 GiB); every delete verdict of C and C2 blocks was a pgrace delete (`segment_delete_verdicts` equal); `pgrace_min_wait_ms` 5000 (C) and 2003 (C2) over all blocks.
* Evidence objects per minute (mean over reps): 64K 26.7 / 22.4 / 20.7, 16K 22.3 / 11.7 / 11.2, zipf 27.2 / 17.4 / 13.3 for B / C / C2; maximum in any 60 s 38. C is 0.52-0.84 x B: **PASS**.
* Cut cost per journal byte at the common cut size (us per MiB, reps 1-5): 64K B 1,590, C 1,554 (**-2.2 %**), C2 1,545 (-2.8 %); 16K B 1,611, C 1,652 (**+2.5 %**), C2 1,719 (+6.7 %);
  zipf B 1,027, C 1,076 (**+4.8 %**, FAIL), C2 1,072 (+4.4 %). The plain ratios are -10.5 / +6.3 / +11.2 % (C) because C cuts larger or smaller journals than B
  (mean cut 241 against 200 MiB at 64K, 160 against 190 MiB at zipf); B's own rep-to-rep SE is 0.3-1.7 % of its mean. Gate 5: **FAIL on the zipf cost clause only**.
* Mark share of the coordinator (all reps): 64K 3.8 / 3.3 / 3.0 %, 16K 15.1 / 8.1 / 7.9 %, zipf 4.3 / 3.1 / 2.4 % (B / C / C2).

## Gate 6: C2 against C (reps 2-5)

| cell | IOPS C / C2 | longest wait median s C / C2 | reps with a stop C / C2 |
|---|---|---|---|
| 64K w4k | 61,928 / 64,098 (+3.5 %) | 2.09 / 1.80 (-0.29 s) | 1 / 0 |
| 16K | 63,328 / 63,056 (-0.4 %) | 1.00 / 0.97 | 0 / 0 |
| zipf | 71,878 / 72,337 (+0.6 %) | 0.65 / 0.64 | 0 / 0 |

**2 s does not buy 10 % more IOPS or 1 s less longest wait: 5 s stays.** At rep 1, C2 is better on the clauses of gate 2 (4 of 4 on the longest wait, 11.9 against 9.1 GiB in 5 s;
longest wait 1.2 against 2.8 s median), but gate 6 names reps 2-5.

## Deviations and remarks

1. **Fio verify** did not exist in the c5t6 harness; added as a 30 s phase after rep 5 of every block (`w4kv.fio`: 4K random write, `verify=crc32c`, `verify_backlog=1024`,
   `verify_fatal=1`, QD 32 x 8 jobs on the same slices), while the collector still works off the last rep's backlog. It does not enter the measured reps. It is the only source of reads.
2. **Reads are almost all the verify phase's**, and nothing overwrote a block between a read's lookup and its extent read in 24 blocks, so the revalidation path
   (C7) was not exercised by this run (`read_revalidated` 0, no stale-age sample); it is covered by `stack_pgrace` and the kill loops of p1/pgrace.
3. **Reading of fuzzy clauses**: gate 2 items are each "4 of 4"; "GiB deleted within 5 s" from pass lines ending in the window (upper-bound timing), proposal lines beside; gate 3 and 6
   use medians pooled over the reps of an arm; gate 4/5 IOPS "not below B by 3 %" on pooled medians per group; compaction volume is per block over reps 1-5; cut cost at the common size
   (c5t6 method). The "write throttle off" check uses the 2 s status samples (the line does not log the headroom).
4. No pins, restart or `off` blocks (not asked). Kill loops and oracles were not rerun (p1/pgrace covers them).
5. Host: kernel 7.0.0, 16 CPUs. The 5-minute load wait took about 25 minutes per block. `LUNIX_M=pgracet6` with an ownership entry on this branch only.

## Clean-up

Remote tree, results copy, tmpfs (unmounted) and logs removed; no daemon, fio or ublk device left (`/dev/ublk-control` was there before). No rust-skills conflict (no Rust written). Commits signed, no trailers.
