# p1-pgracemix: do guest reads race a segment delete under the short pressure grace? Real daemon, mixed 70/30 cell, arms C (5000 ms) and C2 (2000 ms)

Branch `p1/pgracemix` from `origin/p1/pgracet6` (2a6612ae; product code identical to `origin/main` 4e86c670, nothing under `crates/` changed). Measurement only, never merged.
New: `bench/phase1/perf/fio/w4kmix.fio` (the mixed job), `bench/phase1/perf/fio/w4kv.fio` (the verify job), `tests/perf/pgracemix.sh` (driver), a `w4kmix` cell in
`tests/perf/c5t6-cells.sh`, `bench/phase1/perf/pgracemix.py` (tables), an `[pgracemix]` entry in `tools/ownership.toml` (replacing `[pgracet6]`).
Results (`tables.md`, `summary.json`, per block the counters, fio JSON, key log lines; paths scrubbed): `bench/phase1/perf/results/pgracemix/`.
One release build of 4e86c670 on the remote test host (kernel 7.0.0, 16 CPUs, nothing else running), daemon on CPUs 0-7, fio on 8-15, 4 queues, 16 GiB integrity volume with 64 KiB chunks,
1 GiB journal, `destage_workers = 8`, 2 GiB working set, 5 reps of 60 s + 5 s ramp, QD 128 x 8 jobs, fresh fill and fresh daemon and volume per block, `lunix fsck --deep` at the end.
Arms: C `gc_pressure_grace_ms = 5000`, C2 `2000`; both `gc_condemn_on_cut = "on"`, `gc_fast_recheck = true`, `throttle_pace_store_bytes = true`, `log_level = debug`.
Blocks interleaved C, C2, C2, C (n = 2 per arm; n = 3 was not run: the counters were 0 in all four blocks and the two further blocks would not change the verdict).

## Verdict

* **2 s is safe on this read-racing workload.** `read_revalidate_exhausted` 0 and `extent_auth_failures` 0 in all four blocks, and `read_revalidated` was **0** as well: not one guest read
  (14.3 M under C, 15.9 M under C2, 30.2 M in all) lost its extent to a delete. Rate 0 per million; at these read counts the 95 % upper bound is about 0.2 per million for either arm. No `giving up` warning, no
  `read part retried` debug line, `read_stale_age` has no sample (min 0, max 0 ms: the counter is only set by a revalidation), no guest error, crc32c read-back clean, fsck clean, `pgrace_min_wait_ms` at or above the key everywhere (C 5002-5009, C2 2000-2017).
  The race path (C7) was therefore not entered by the daemon at all under this workload; its margin was not measured, only that it was not needed.
* **C2 keeps an edge over C on this mix, but only on throughput and the read p99, not on stalls.** Writes +22.7 % (reps 2-5 median of block medians 51.0 k against 41.6 k), reads +22.6 %, read p99 117 against 198 ms, rep 1 +6.1 % writes. Longest write wait and stops are the same (4.4-4.6 s median, 10 stops per block median, both arms), and p99.9 is 638 against 705 ms. C2 holds pressure for 54 % of reps 2-5, C for 73 %.

## Table (median over the two blocks of an arm; worst block in brackets)

| metric | C (5000 ms) | C2 (2000 ms) |
|---|---|---|
| read_revalidated per million guest reads | 0 (0) | 0 (0) |
| read_revalidate_exhausted | 0 (0) | 0 (0) |
| extent_auth_failures | 0 (0) | 0 (0) |
| read_stale_age min / max (ms; 0 = no sample) | 0 / 0 (0 / 0) | 0 / 0 (0 / 0) |
| guest write IOPS, reps 2-5 | 41,608 (38,241) | 51,035 (50,944) |
| guest read IOPS, reps 2-5 | 17,844 (16,407) | 21,878 (21,838) |
| guest write / read IOPS, rep 1 | 43,992 (42,284) / 18,875 (18,142) | 46,683 (45,774) / 20,032 (19,644) |
| read latency p99 ms, reps 2-5 | 198 (237) | 117 (121) |
| read latency p99.9 ms, reps 2-5 | 705 (738) | 638 (642) |
| read latency max ms, any rep | 4,148 (4,502) | 4,796 (4,921) |
| longest write wait s, reps 2-5 (per-second maximum of the write completion latency) | 4.53 (4.58) | 4.36 (4.60) |
| longest write wait s, reps 1-5 | 4.53 (4.58) | 4.74 (4.79) |
| write-throttle stops, 5 reps | 10 (13) | 10 (18) |
| pgrace_deletes | 11,664 (11,381) | 13,467 (13,315) |
| pgrace_min_wait_ms (least; key is 5000 / 2000) | 5,006 (5,002) | 2,008 (2,000) |
| pressure raised, share of reps 2-5 / rep 1 | 73 % / 57 % | 54 % / 44 % |
| guest reads in the block (5 reps + verify) | 6.49 M, 7.16 M | 8.10 M, 7.80 M |

"Worst" is the largest value, except IOPS, pgrace and raised share (smallest). Per-block rows: `tables.md`. Rep 1 is the cold start after the fill.

## The workload and why it is a fair race test

* `w4kmix.fio`: one job per slice (8 jobs over the 2 GiB working set, the whole written span), `rw=randrw`, `rwmixread=30`, 4 KiB, QD 128, uniform offsets. Measured mix: 70.0 % writes, 30.0 % reads (about 51 k and 22 k IOPS under C2). Reads and writes draw from one offset stream, and
  at that rate every block is overwritten about every 10 s, so the reads land on recently overwritten blocks; the volume's 64 KiB chunks make each overwrite a read-modify-write and each compaction move live blocks, so entries change under reads continuously.
* A zipf:1.2 variant was tried in a short dry run (not kept) and held pressure only about half of the time (uniform: about 65 %), so uniform was used. The `zipf` option stays in the job file (`DIST`).
* To keep pressure up the store was a 13 GiB tmpfs (the pgracet6 arms had 20 GiB). The pressure lines (`gc: the engine's pressure level changed`) give the raised share above; the engine's limit is about 11.2 GB. That is tighter than pgracet6: `no space` stops of compaction passes (WARN, 2-5 per block) and in b03 two
  "writes wait for space" warnings appear, `fulls` and `enospc` stay 0, there is no guest error. Treat the stall figures as a stress case, not as the normal operating point.
* Verify phase after rep 5, as in pgracet6 (30 s, `verify=crc32c`, `verify_backlog=1024`, `verify_fatal=1`): errors 0 in all four blocks. `w4kv.fio` was missing from the pgracet6 branch (the report names it, the commit did not carry it) and was rewritten to that description.

## Per-read retry count and the worst case

The number of retries of one read part is **not exposed**. Only the totals `read_revalidated` (all retries) and `read_revalidate_exhausted` exist; the per-retry line is a debug line, and there were none. Estimate of the worst case from what is exposed:
`read_stale_age_max_ms` has no sample, so no read ever saw `NotFound` with a changed entry. A retry needs the read to hold its entry longer than the grace (a segment is deleted at the earliest `grace` after its flag), and the 16th retry needs 16 such rounds, each of which needs
another overwrite plus two cuts, a flag, the grace and a recheck: at 2 s that is more than half a minute of one stuck part. The slowest guest reads here took 3.8-4.9 s (during the throttle stops, when writes and reads queue together), which is above both graces, and still none revalidated:
the reader's age is measured from the part's submit inside the daemon (design note 2.2), so time spent queued before that is not counted, and this run cannot tell how long the held entries were. What it shows is that no held entry went stale at all; the distance to 16 retries is, by the argument above, the full 16 rounds.

## Safety per block

| block | arm | guest errors | crc32c read-back | `fsck --deep` | exhausted | auth failures | revalidated | WARN lines |
|---|---|---|---|---|---|---|---|---|
| b01 | C | 0 | clean | 0 | 0 | 0 | 0 | 11 (commit headroom short, compaction `no space`) |
| b02 | C2 | 0 | clean | 0 | 0 | 0 | 0 | 3 (same kinds) |
| b03 | C2 | 0 | clean | 0 | 0 | 0 | 0 | 7 (same kinds, 2 x writes wait for space) |
| b04 | C | 0 | clean | 0 | 0 | 0 | 0 | 2 (commit headroom short) |

`pgrace_uncondemned` 0, EIO/ENOSPC log lines 0, `fulls` 0 and `enospc` 0 in all blocks.

## Deviations and remarks

1. **Whole device**: the job covers the filled 2 GiB working set (all 8 slices), as the pgracet6 arms did, not the whole 16 GiB volume; reads of unwritten space would not exercise extents.
2. **Quiet-host gate**: the load average on this host stays above 100 for tens of minutes after a block (the io_uring workers count while fio runs) although vmstat shows it idle, so the 1/5-minute rule of pgracet6 would have cost about 20 minutes per block. The driver waits instead until cpu and io pressure-stall `some avg10` are below 1 % for 10 seconds in a row with no fio or daemon alive. The load averages at block start are in `tables.md` (1 minute 18-116, they are the residue of the previous block).
   Throughput is therefore from a host that was idle but not load-average quiet; compare C with C2 inside this run only.
3. Stops are large here (b03: 12 in reps 2-5) against pgracet6's 0-1 per 4 reps because of the 13 GiB store; the C2 stops per block (2, 18) and C (13, 8) overlap, so no arm difference in stalls can be claimed.
4. n = 2 per arm. The IOPS gap is two blocks against two blocks (C2 50.9-51.1 k, C 38.2-45.0 k write IOPS), consistent in sign with pgracet6 (+3.5 %) but larger; the block-to-block spread of C (38-45 k) is wide.
5. No product code, goldens or formats changed. rust-skills: no Rust written. No pricing.

## Clean-up

Remote tree, results copy and the tmpfs (unmounted) removed; no daemon, fio or ublk device left (the `ublk_drv` module was there before). Two earlier dry-run mounts and a root-owned results directory left by the dry runs were unmounted and removed too. Commits signed, no trailers.
