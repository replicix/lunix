# M17.B: GC on the real stack, and the N25 mark cost

Date 2026-11-17. Branch `p1/m17b-stack` (base `origin/main` `fe151c2`). Reproduce on the test host from the
repository root: `bash bench/phase1/gc/run.sh churn` and `bash bench/phase1/gc/run.sh mark`.

## Host and versions

- CPU: AMD EPYC (Zen 5), 16 vCPUs, SMT off, 31 GiB RAM, local SSD, ext4.
- Kernel 7.0.0; governor: n/a (virtualised, no cpufreq; not settable).
- Rust: the repository's toolchain (`rust-toolchain.toml`), release builds, no `target-cpu=native`.
- Pinned to CPUs 0-7 (`taskset -c 0-7`), `-j 8`. Other workloads may run on CPUs 8-15.
- No root. Single runs; the figures are counts and ratios, not timings, except the mark cost table (one run per cell).

## Statistics line

churn (32 MiB x 10, 4 configurations): store / live data 1.012 to 1.053 (bound: 1.3 x live + 4 x largest pack +
16 MiB); mark of a 64 M-chunk volume 52.8 s, longest 50 ms step 62 ms; GC kill loops: every `gc.*` point killed at
its first, middle and last hit in 6 configurations, `lunix-fsck` without error after every kill.

## Churn on the stack (`crates/lunixd/tests/stack_gc.rs`, `churn_*`)

Workload: a 32 MiB region of a 64 MiB volume rewritten 10 times (even rounds: sequential whole chunks; odd rounds: a
random mix of partial chunks, repeated seeds (dedup) and write-zeroes), a commit per round, the whole device read
back against the oracle after every round, 3 s of idle ticks between rounds; then idle ticks with a forced commit
every 2 s until the store stops shrinking. GC: a pass every second, grace 10 s (`read_timeout` 0, the smallest the
collector takes), everything else the `volume.json` defaults. Figures from `lunix-fsck --json` at rest.

| config | live (unique clen) | store (segments + packs + index + commits) | ratio | peak during churn | largest pack | evidence objects |
|---|---|---|---|---|---|---|
| plain 16 KiB | 21 066 KiB | 22 103 KiB | 1.049 | 86 306 KiB | 63 KiB | 10 |
| encrypted 16 KiB | 21 247 KiB | 22 366 KiB | 1.053 | 86 805 KiB | 63 KiB | 10 |
| plain 64 KiB | 23 306 KiB | 23 590 KiB | 1.012 | 123 635 KiB | 34 KiB | 7 |
| encrypted 64 KiB | 21 783 KiB | 22 092 KiB | 1.014 | 122 933 KiB | 34 KiB | 7 |

Every config is under the plan's bound (1.3 x live + 4 x the largest commit's map-pack bytes + one segment target)
and reads equal the oracle in every round. The index is the largest overhead at 16 KiB (771 to 775 KiB, 3.6 %).

**The 2 GiB x 10 cell** (plan; `LUNIX_GC_CHURN_MIB=2048`, plain 64 KiB, after the M17.B stack review fixes,
2026-11-18): live 1 361 MiB, store at rest 1 645 MiB, **ratio 1.209** (bound 1 788 MiB); peak during the churn
16.0 GiB, about 12 x live (each round writes 2 GiB in about 90 s; the grace and a pass over 32 K chunks with deletes
and compaction take longer, so GC catches up only once the churn stops); 256 whole-device reads under GC, all equal
to the oracle; 1 264 s. With the review's changes the 32 MiB cells read 1.016 to 1.056.

Not measured here (the stack does not expose the collector's counters; `GcControl` is not wired, see the report):
compaction write amplification and the dead share of surviving map packs (N8). The 2 GiB x 10 cell is above.

## N25: the mark cost (`crates/lunix-gc/tests/mark_cost.rs`)

The real map (`lunix-map`, fanout 512) over the testkit's in-memory store and index, 16 KiB chunks, every chunk
distinct and live, three retained roots sharing every leaf (a quiet volume). Real clock. "Whole mark" is
`lunix_gc::liveness`; "pass mark" is the same mark in the collector's 50 ms steps.

| chunks | volume at 16 KiB | leaves | leaves²/32 lookups | whole mark | per chunk | pass mark: steps, total | longest step | peak RSS (whole test) |
|---|---|---|---|---|---|---|---|---|
| 1 048 576 | 16 GiB | 2 048 | 131 072 | 0.33 s | 0.32 µs | 7, 0.33 s | 52.1 ms | 0.4 GiB |
| 4 194 304 | 64 GiB | 8 192 | 2.1 M | 1.49 s | 0.36 µs | 29, 1.50 s | 53.9 ms | 1.5 GiB |
| 16 777 216 | 256 GiB | 32 768 | 33.6 M | 8.49 s | 0.51 µs | 159, 8.57 s | 58.7 ms | 5.1 GiB |
| 67 108 864 | 1 TiB | 131 072 | 537 M | 52.8 s | 0.79 µs | 1 027, 55.5 s | 62.2 ms | 20.2 GiB |

Reading: the per-chunk cost grows 2.5x from 1 M to 64 M chunks (the quadratic skip-set term showing), and a step
still ends within 25 % of its budget at 1 TiB (a unit is 32 leaves: its traversal of the whole root grows with
the root, so the overrun grows too). At 1 TiB a mark takes about a minute of one core every pass. The range walk of
N25 removes the quadratic term; it is not needed for phase 1 volumes up to 1 TiB, and needed before 8 TiB
(projected from these cells, about 60 ns per skip-set lookup; at 8 TiB 1 M leaves, about 34 G lookups, a mark near 35 min and a unit near 0.1 s). Peak RSS is the in-memory fake volume, not the collector.

## GC crash loops (`crates/lunixd/tests/stack_gc_kill.rs`)

Every `gc.*` point (9 of rule 10 plus the urgent pass's own) and the points only GC reaches
(`store.evidence.*`, `index.after_condemn_flag`) killed by `SIGKILL` at the first, a middle and the last hit of a
workload (prep child killed in its first commit, a planted `*.tmp`; then 6 rounds of a hot 8 MiB region rewritten in
its own cut, every other round three chunks of four, plus random writes, with 13 s idle phases of ticks and a forced
commit every 2 s). After each kill: `lunix-fsck` no error and no `index_lost`, journal damage above the durable
bound, open under `refuse`, device equal to an oracle prefix at least as long as the durable one, writes and
read-back, clean close, second open equal, store self-check clean, `lunix-fsck` exit 0 at rest. Command:
`LUNIX_STACK_REQUIRE_FSCK=1 LUNIX_KILL_HITS=all cargo test --release -p lunixd --features testsupport,crashpoints
--test stack_gc_kill gc_kill_loop -- --nocapture --test-threads=6`.

| config | points | reached | runs | kills | time | findings after the kills (warnings) | at rest |
|---|---|---|---|---|---|---|---|
| plain 16 KiB | 15 | 14 | 42 | 42 | 2 214 s | crash_leftover 6, index_extra 1, leftover 6, orphan 16 | notes only |
| plain 64 KiB | 15 | 14 | 42 | 42 | 1 963 s | crash_leftover 6, index_extra 1, leftover 6, orphan 22 | notes only |
| integrity 16 KiB | 15 | 14 | 42 | 42 | 2 164 s | crash_leftover 4, index_extra 1, leftover 6, orphan 15 | notes only |
| integrity 64 KiB | 15 | 14 | 42 | 42 | 2 018 s | crash_leftover 7, leftover 6, orphan 22 | notes only |
| encrypted 16 KiB | 15 | 14 | 42 | 42 | 2 156 s | crash_leftover 7, index_extra 1, leftover 6, orphan 16 | notes only |
| encrypted 64 KiB | 15 | 14 | 42 | 42 | 2 060 s | crash_leftover 3, leftover 6, orphan 15 | notes only |

Unreached in every config: `gc.urgent.after_mark` only (the space ladder's compact step is not wired; this workload
never runs out of space). `index_extra` appears after a kill at `gc.delete.after_unlink` (the file is gone, the index
entries are not yet dropped; gc.md's crash table) and is gone at rest. The six ran in parallel on 8 CPUs.
