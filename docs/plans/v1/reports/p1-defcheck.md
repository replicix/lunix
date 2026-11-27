# p1-defcheck: real-daemon check of the new GC defaults (D) against C2 and the old defaults (O)

Branch `p1/defcheck` from `origin/main` 6774b437. Measurement only, never merged: no product code changed. The harness of `origin/p1/pgracemix` (396baa34) was brought over
(`tests/perf/*`, `bench/phase1/perf/*`, `w4kv.fio` among them); new: `tests/perf/defcheck.sh` (driver), `bench/phase1/perf/defcheck.py` (tables, built on `c5t6.py`), an `[defcheck]`
entry in `tools/ownership.toml`. Results: `bench/phase1/perf/results/defcheck/` (`tables.md`, `summary.json`, per block the rep table, fio JSON, final counters, key log lines; paths scrubbed).

Setup as pgracet6 (remote test host, kernel 7.0.0, 16 CPUs, nothing else running): one release build of 6774b437, 20 GiB tmpfs store, 16 GiB integrity volume with 64 KiB chunks, 1 GiB journal,
4 queues, daemon on CPUs 0-7, fio on 8-15, `destage_workers = 8`, 2 GiB working set, fresh fill, w4k (write-only 4K random) 60 s + 5 s ramp, QD 128 x 8 jobs, 5 reps, then the 30 s crc32c verify
phase, `lunix fsck --deep`. Fresh daemon and volume per block; blocks interleaved D, C2, O, O, C2, D (n = 2 per arm); 6 of 6 driver rc 0. Each block started once cpu and io PSI `some avg10`
had been below 1 % for 10 s in a row with no fio or lunixd alive (as pgracemix). `log_level = debug`. Onset is the collector's line `gc: the engine's pressure level changed raised=true`.

Arms (every arm also has `throttle_pace_store_bytes = true`, as in pgracet6, because that key is still off by default; it is not a GC key):

* D: no explicit GC keys, so `gc_pressure_grace_ms = 2000`, `gc_fast_recheck = true`, `gc_condemn_on_cut = off`.
* C2: the defaults plus `gc_condemn_on_cut = "on"`.
* O: `gc_pressure_grace_ms = 0`, `gc_fast_recheck = false`, `gc_condemn_on_cut = "off"`.

## Verdict

**The new defaults (D) do not pass the pgracet6 rep-1 gate for the short grace.** Two of five clauses fail in both D blocks (first delete at most 3 s: 8.8 and 5.9 s; at least 6 GiB deleted within 5 s of the onset: 0.00 GiB in both),
the longest wait fails in one (5.4 s in b01, 3.6 s in b06) and the stops clause in the other (b06: 1 stop, b01: 0); throttled time passes in both (9.8 and 7.9 s). C2 passes all five in both blocks; O fails all five in both blocks.
The cause is visible in the first-delete and 5 s columns: with `gc_condemn_on_cut = off` no segment is condemned ahead of the onset, so there is no backlog for the first recheck to delete at once (C2: 12.2-12.3 GiB in 5 s, first delete 0.08-0.09 s); D waits for a mark to condemn, then for the 2 s grace.

**How much of C2's gain D keeps** (O to C2 is the gain; medians of the two blocks):

| measure | O | D | C2 | D keeps |
|---|---|---|---|---|
| rep 1 IOPS | 19,602 | 60,498 | 67,877 | 85 % |
| rep 1 throttled s | 42.7 | 8.8 | 3.0 | 85 % |
| rep 1 longest wait s | 36.4 | 4.5 | 1.1 | 90 % |
| rep 1 p99.9 s | 6.85 | 0.65 | 0.61 | 99 % |
| reps 2-5 IOPS | 17,876 | 64,336 | 62,887 | 103 % (D at or above C2, within block spread) |
| reps 2-5 p99.9 s | 2.94 | 0.62 | 0.62 | 100 % |

In the steady state D equals C2 (IOPS 63.7-65.0 k against 62.3-63.4 k, p99.9 0.62 s, 0 stops, longest wait 1.3 s median against 2.1 s); the whole of the difference is in the first run after the fill, where D is
about 11 % lower in IOPS, spends about 6 s more throttled, and has a longest wait of 3.6-5.4 s against 1.0-1.2 s. Against O, D removes the stalls: one stop in 10 D reps (b06 rep 1) against 4-5 stops in rep 1 of both O blocks.

## Table (median over the two blocks of an arm; worst block in brackets)

| metric | D (defaults) | C2 | O (old defaults) |
|---|---|---|---|
| rep 1: first delete after onset s | 7.32 (8.78) | 0.08 (0.09) | 33.58 (35.79) |
| rep 1: stops | 0 (1) | 0 (0) | 4 (5) |
| rep 1: longest write wait s | 4.5 (5.4) | 1.1 (1.2) | 36.4 (38.5) |
| rep 1: throttled s | 8.8 (9.8) | 3.0 (3.4) | 42.7 (45.1) |
| rep 1: GiB deleted within 5 s of onset | 0.00 (0.00) | 12.29 (12.23) | 0.00 (0.00) |
| rep 1: IOPS | 60,498 (60,120) | 67,877 (66,372) | 19,602 (19,209) |
| rep 1: p99.9 s | 0.65 (0.67) | 0.61 (0.62) | 6.85 (6.88) |
| reps 2-5: IOPS (median of reps) | 64,336 (63,690) | 62,887 (62,343) | 17,876 (17,634) |
| reps 2-5: p99.9 s | 0.62 (0.62) | 0.62 (0.63) | 2.94 (3.25) |
| reps 2-5: longest write wait s (median of reps / worst rep) | 1.3 (1.3) / 2.8 (3.5) | 2.1 (2.7) / 3.1 (3.8) | 8.7 (10.9) / 15.0 (19.7) |
| reps 2-5: stops (4 reps) | 0 (0) | 0 (0) | 2 (4) |
| pgrace_deletes (whole block) | 16,360 (16,340) | 16,200 (16,128) | 0 (0) |
| pgrace_min_wait_ms (least; key 2000) | 2,017 (2,000) | 2,005 (2,000) | 0 (0) |
| mark share of the coordinator, all reps % | 2.98 (3.03) | 2.97 (3.10) | 1.29 (1.30) |
| mark share, reps 2-5 % | 3.14 (3.17) | 3.11 (3.23) | 1.45 (1.46) |

"Worst" is the largest value, except IOPS, GiB, pgrace_deletes and pgrace_min_wait (smallest). "GiB within 5 s" is the bytes freed by collector pass lines ending in the 5 s after the onset; "first delete" is the earlier of the first
pass line that deleted anything and the first `a proposed segment was deleted` line after the onset (pass lines are an upper bound by the pass wall). Mark share is the collector's mark-phase time over the repetition windows, mean over reps. The O mark share is
low because the throttled workload leaves the collector less to mark, not because marking got cheaper. Per-block rows, including the D blocks' own values: `tables.md`.

## Rep-1 gate for the short grace, per block

| block | arm | first delete <= 3 s | 0 stops | longest wait <= 5 s | throttled <= 22 s | >= 6 GiB in 5 s |
|---|---|---|---|---|---|---|
| b01 | D | 8.78 s FAIL | 0 pass | 5.4 s FAIL | 9.8 s pass | 0.00 FAIL |
| b06 | D | 5.87 s FAIL | 1 FAIL | 3.6 s pass | 7.9 s pass | 0.00 FAIL |
| b02 | C2 | 0.09 pass | 0 pass | 1.0 pass | 3.4 pass | 12.23 pass |
| b05 | C2 | 0.08 pass | 0 pass | 1.2 pass | 2.6 pass | 12.34 pass |
| b03 | O | 35.79 FAIL | 5 FAIL | 34.2 FAIL | 45.1 FAIL | 0.00 FAIL |
| b04 | O | 31.37 FAIL | 4 FAIL | 38.5 FAIL | 40.2 FAIL | 0.00 FAIL |

## Safety, every block

All six blocks: fio errors 0 in every rep and in the verify phase (crc32c read-back clean, rc 0), `lunix fsck --deep` exit 0, `read_revalidate_exhausted` 0, `extent_auth_failures` 0, `read_revalidated` 0, EIO/ENOSPC log lines 0,
`fulls + enospc` 0. `pgrace_min_wait_ms` is at or above 2000 in every D and C2 block (2000-2034), 0 in O (no pressure grace). WARN lines: none in D and C2 blocks; the four O blocks' warnings
(6 lines in each of b03 and b04: "writes wait for space", "urgent compaction stopped" and "compaction stopped for this pass" with error `no space`, and in b04 "commit headroom is short")
come with the old defaults filling the 20 GiB store (store used about 18.0 GB against a limit of about 18.0 GB); pgracet6's arm B had the same warnings in b09.

## Remarks and limits

1. n = 2 per arm; the two D blocks differ in rep 1 (first delete 8.8 against 5.9 s, one stop against none) and agree on the reading: no backlog, no 6 GiB. The verdict on the gate does not depend on the spread (0.00 GiB in both, first delete above 3 s in both).
2. Gate clauses are read as in pgracet6 (each "in every block"). Read as a median, D still fails first delete and the 6 GiB clause and passes stops, wait (median 4.5 s) and throttled.
3. Quiet host: gated on PSI as in pgracemix. The load average at block start is the residue of the previous block (1.3 for b01, 105-118 after), recorded in `tables.md`; the driver's own load-average flag reads 1 for the later blocks for that reason only. Compare arms inside this run only.
4. The mark share of D (2.98 %) equals C2 (2.97 %), within the pgracet6 figure for 64K (C 3.3, C2 3.0, B 3.8).
5. No product code, goldens or formats changed. No Rust written. No pricing.

## Clean-up

Remote tree, results copy and the tmpfs removed (see the hand-off); no daemon, fio or ublk device left. Commits signed, no trailers.
