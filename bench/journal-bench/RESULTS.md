# P0.5 results: journal microbenchmark (ADR-4)

Spike: `spikes/journal-bench` (`journal-bench`, io-uring `=0.7.16`, hdrhistogram `=7.6.0`, clap 4.6.7). Reproduce:
`cd spikes/journal-bench && cargo build --release && sudo ./run-journal.sh && python3 analyze.py out/results.jsonl`.
Host: AMD EPYC (Zen 5), 16 vCPUs, SMT off, kernel 7.0.0 Ubuntu, local NVMe SSD with `write_cache = write through` (so FUA/flush
reach the medium), ext4 root; journal thread pinned to CPU 0, background reader to CPU 1. Date 2026-11-12, revision uncommitted.
Raw: `raw/results.jsonl` (848 cells: `results-pass1.jsonl` 656, `results-pass2.jsonl` 192), `raw/hw.txt`; generated tables: `tables.md`.

## What was run
- Strategies: `group` B in {1,4,16,64} (plus B=64 with collect windows 50 and 200 us), `linked` WRITE...WRITE->FSYNC chains B in {1,4,16,64}
  with up to 4 chains in flight, `dsync` (RWF_DSYNC per write), `odsync` (O_DSYNC file). Strategy D (two files) was skipped as the plan allows.
- File systems: ext4, xfs, btrfs (each an image on a loop device with `--direct-io=on`, 12 GiB, `nosuid,nodev`), btrfs `nodatacow`
  (`chattr +C`), a directory on the root ext4 (`root-dir`, the real disk), and `noatime` variants of ext4, xfs, btrfs.
  Preallocated zero-filled 8 GiB journal file (written once with dd before the cells); growing-file control and background-reader
  (64 KiB sequential O_DIRECT reads on the same file) controls for R in {4k, 64k}.
- Records: R = 4 KiB and 64 KiB everywhere; 16 KiB and 256 KiB at saturation; **512 B was not run separately** because it is padded to 4 KiB and issues
  the identical I/O. Offered load: the plan's 50 K rec/s was run everywhere, 10 K and 200 K for 4 KiB, closed-loop saturation everywhere.
- **Deviation: the 50 K/s offered load cannot be sustained by this disk.** A durable flush costs ~1.8 ms (ext4 loop, flush p50), i.e. at most ~550
  flushes/s, so at 50 K rec/s every configuration is saturated (rec/s below 95 % of offered) except group B=64 at 15 K rec/s. A second pass
  therefore added **2 000 and 5 000 rec/s** offered-load cells (ext4, xfs, btrfs, root-dir; primary strategies only); the decision rule is
  evaluated at those rates. btrfs never sustained any of them with any strategy (its flush p50 is 6-13 ms).
- 3 s per cell, one run per cell (no repetitions, unlike the ublk matrices): single-run noise on this shared host is large (btrfs and
  several xfs cells differ by 2-5x between neighbouring cells); only large effects should be read from the tables.
- Loop-backed file systems add a layer (see plan); the `root-dir` rows are the direct-disk comparison.

## Decision rule (ADR-4, fixed in advance)
Linked `WRITE->FSYNC` wins if on all three file systems at R=4 KiB and 64 KiB it has median durable latency <= 0.9x group commit's best B and p99 not worse than +10 %,
or >= 10 % higher saturation throughput at equal p99. Preallocation is mandatory if the growing-file control is >= 20 % slower at p99; O_DSYNC-per-write is
rejected unless within 10 % of group commit at B=1.

### Decision-rule inputs (non-saturated cells only)

| fs config | R @ offered rec/s | best group (p50/p99 us) | best linked (p50/p99 us) | linked p50/group p50 | latency clause | saturation clause (linked vs group rec/s, p99) | O_DSYNC/RWF_DSYNC within 10% of group B=1 | growing-file p99 / prealloc p99 (group B=16) |
|---|---|---|---|---|---|---|---|---|
| ext4 | 4K@2000 | group B=16 (4919/71958) | linked B=64 (6918/16294) | 1.41 | no | no (0.27x, 76284 vs 5632) | no | 1.13 |
| ext4 | 4K@5000 | group B=64 (4899/7889) | linked B=16 (783811/2441085) | 160.00 (linked saturated) | no | no (0.27x, 76284 vs 5632) | no | 1.13 |
| ext4 | 4K@10000 | group B=64 (5738/23872) | linked B=64 (928514/1775239) | 161.80 (linked saturated) | no | no (0.27x, 76284 vs 5632) | no | 1.13 |
| ext4 | 64K@2000 | group64 B=64 W=50 (36733/324534) | linked B=16 (897581/2191524) | 24.44 (linked saturated) | no | no (0.53x, 290456 vs 69009) | no | 1.07 |
| xfs | 4K@2000 | group B=64 (290456/525337) | linked B=64 (947388/1239417) | 3.26 (linked saturated) | no | no (0.49x, 96666 vs 52953) | no | 1.12 |
| xfs | 4K@5000 | group64 B=64 W=200 (4180/6050) | linked B=64 (310903/543162) | 74.38 (linked saturated) | no | no (0.49x, 96666 vs 52953) | no | 1.12 |
| xfs | 64K@2000 | group64 B=64 W=200 (5157/7086) | linked B=16 (14172/41058) | 2.75 | no | no (0.37x, 93848 vs 11002) | no | 1.09 |
| xfs | 64K@5000 | group B=64 (5403/7975) | linked B=64 (752353/1435500) | 139.26 (linked saturated) | no | no (0.37x, 93848 vs 11002) | no | 1.09 |
| root-dir | 4K@2000 | group B=64 (10846/32195) | linked B=1 (146276/761790) | 13.49 (linked saturated) | no | no (0.11x, 2156 vs 3625) | no | 1.31 |
| root-dir | 4K@5000 | group64 B=64 W=200 (17547/190841) | linked B=1 (581435/1667236) | 33.14 (linked saturated) | no | no (0.11x, 2156 vs 3625) | no | 1.31 |
| root-dir | 4K@10000 | group64 B=64 W=200 (1876/3770) | linked B=4 (830472/1636827) | 442.68 (linked saturated) | no | no (0.11x, 2156 vs 3625) | no | 1.31 |
| root-dir | 64K@2000 | group64 B=64 W=50 (4284/17891) | linked B=64 (280756/2902458) | 65.53 (linked saturated) | no | no (0.42x, 6734 vs 5411) | no | 1.18 |

### Recommended primitive per file system

| fs config | linked wins at all of R=4 KiB and 64 KiB (latency or throughput clause) | recommendation |
|---|---|---|
| ext4 | no | group commit |
| xfs | no | group commit |
| root-dir | no | group commit |

### Saturation comparison (closed loop; rule: linked if >= 10 % more records/s at equal p99)

| fs config | R | best group (rec/s, p50/p99 us) | best linked (rec/s, p50/p99 us) | linked/group rec/s | linked p99 <= group p99 | clause met |
|---|---|---|---|---|---|---|
| ext4 | 4 KiB | group B=64 (14990, 4633/5632) | linked B=64 (4113, 61080/76284) | 0.27 | no | no |
| ext4 | 64 KiB | group64 B=64 W=200 (3125, 14385/69009) | linked B=64 (1644, 149946/290456) | 0.53 | no | no |
| xfs | 4 KiB | group64 B=64 W=200 (2764, 21725/52953) | linked B=16 (1367, 44040/96666) | 0.49 | no | no |
| xfs | 64 KiB | group64 B=64 W=200 (7838, 8049/11002) | linked B=64 (2933, 85656/93848) | 0.37 | no | no |
| btrfs | 4 KiB | group64 B=64 W=50 (380, 135528/489161) | linked B=64 (1082, 222036/417595) | 2.85 | yes | YES |
| btrfs | 64 KiB | group B=16 (295, 46105/160039) | linked B=64 (422, 699400/832045) | 1.43 | no | no |
| btrfs-nodatacow | 4 KiB | group64 B=64 W=200 (7674, 7864/11026) | linked B=16 (1426, 23118/304349) | 0.19 | no | no |
| btrfs-nodatacow | 64 KiB | group B=64 (3195, 17105/47382) | linked B=64 (803, 199492/546832) | 0.25 | no | no |
| root-dir | 4 KiB | group B=64 (36511, 1478/3625) | linked B=1 (3861, 967/2156) | 0.11 | yes | no |
| root-dir | 64 KiB | group B=16 (7395, 1722/5411) | linked B=4 (3099, 5153/6734) | 0.42 | no | no |
| ext4-noatime | 4 KiB | group B=64 (7023, 5509/43418) | linked B=16 (1376, 41222/105382) | 0.20 | no | no |
| ext4-noatime | 64 KiB | group64 B=64 W=50 (2826, 12558/346030) | linked B=64 (1353, 164233/392430) | 0.48 | no | no |
| xfs-noatime | 4 KiB | group B=64 (2617, 21184/61407) | linked B=64 (1440, 150077/295961) | 0.55 | no | no |
| xfs-noatime | 64 KiB | group B=64 (2512, 18678/103547) | linked B=64 (613, 390857/701497) | 0.24 | no | no |
| btrfs-noatime | 4 KiB | group B=64 (948, 66585/75563) | linked B=64 (3535, 71238/86049) | 3.73 | no | no |
| btrfs-noatime | 64 KiB | group B=64 (344, 136577/741868) | linked B=64 (1682, 98566/566231) | 4.89 | yes | YES |


## Outcome

- **Group commit (one `FSYNC(DATASYNC)` per group, ADR-4's current text) is confirmed; linked `WRITE->FSYNC` is not adopted.** On ext4, xfs and the real-disk
  directory the latency clause fails (linked is 1.4x-2.8x slower at sustainable load, and above ~2 K rec/s the 4-chain linked variant cannot sustain the load at all) and
  the saturation clause fails by a wide margin (linked reaches 0.11x-0.55x of group commit's records/s). Chains in flight serialise on the file system journal, so
  the extra parallelism does not help, and fsync latency per chain is higher (flush p50 2.6-5 ms vs 1.8 ms).
- **btrfs is the exception that does not meet the rule.** On btrfs default and btrfs `noatime` linked chains reach 1.4x-4.9x the saturation throughput of group commit
  (btrfs fsync takes 6-20 ms, so overlapping chains hide latency), and the p99 clause holds in two of the four btrfs cells (4 KiB default, 64 KiB `noatime`), but not consistently,
  and linked loses on `nodatacow` btrfs (0.19x-0.25x). A per-fs default for btrfs is not justified; the journal file should be `nodatacow` there (group commit on
  `nodatacow` btrfs reaches 7 674 rec/s vs 380 rec/s on default btrfs, 20x).
- **Preallocation:** growing-file p99 vs preallocated (group B=16, 4 KiB at 50 K): 1.13 (ext4), 1.12 (xfs), 1.31 (root-dir ext4), 1.07-1.18 at 64 KiB. The >= 20 % rule fires only for the root-dir 4 KiB case; preallocation is recommended
  everywhere (cheap) and mandatory by the rule on the real disk.
- **O_DSYNC / RWF_DSYNC per write is rejected:** 43-1041 rec/s (vs 2.6 K-36.5 K for group commit on ext4, xfs and root-dir at saturation) and never within 10 % of group commit at B=1 once any batching is allowed.
- Background sequential reader (destage-like): no clear effect on ext4/xfs medians at saturation; other cells are within the noise of single runs (root-dir 64 KiB group B=16 fell from 7 395 to 665 rec/s with the reader, a possible
  real interference on the shared disk, one run only).
- Closed-loop best group commit (4 KiB): ext4 15 K rec/s, xfs 2.8 K, btrfs 0.4 K, btrfs nodatacow 7.7 K, root-dir 36.5 K (flush p50 ~3 us because the real disk reports `write through` and the flush is nearly free there; the loop-backed images pay ~2-13 ms per flush).
  The loop layer clearly changes flush cost (ext4-loop 1.8 ms vs root-dir 3 us), so the loop-backed numbers say more about the ordering/pipelining behaviour than about absolute latency.

## Proposed decision record (for the lead to merge)
ADR-4 gate outcome (2026-11-12): **accepted (gate)** for "one `fdatasync` per sync point with group commit". Linked `WRITE->FSYNC` did not beat it on ext4, xfs or the real-disk directory (rule not met);
btrfs shows higher saturation throughput for linked chains but inconsistently, so no per-fs default. Preallocated zero-filled journal file; `chattr +C` on btrfs. O_DSYNC per write rejected.
Input text for DESIGN section 4: a sync point costs 1 FUA + 1 FLUSH on every tested fs (P0.4), group size >= 16 amortises it; sustainable durable rate on loop-backed fs was ~500 flushes/s.
