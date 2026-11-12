# Implementation plans (v1)

Sequenced, self-contained work orders for Lunix (see
[`ROADMAP.md`](ROADMAP.md)). Each plan is executed by a coding model in a
fresh session; [`CONVENTIONS.md`](CONVENTIONS.md) carries the shared rules
(gates, safety rules for ublk spikes, benchmark hygiene, decision
recording, reporting format).

Layout:

- [`done/`](done/) — completed plans
- [`wip/`](wip/) — plans in progress or not yet started

Execution protocol:

1. Hand the model exactly one plan file (plus CONVENTIONS.md), in a fresh
   session.
2. The model implements until all gates in the plan are green, does NOT
   commit, and writes a report.
3. A coordinator (separate session) reviews the diff, re-runs the gates
   (and the headline benchmark runs) independently, and commits.
4. Only then does the next plan start. Plans assume all predecessors are
   committed.
5. When a plan finishes, move it from `wip/` to `done/` and update the
   table below.

| # | file | status | milestone | depends on |
|---|---|---|---|---|
| 00 | `wip/00-p0-spikes-and-gates.md` | wip (mostly done 2026-11-12; P0.6 needs traces) | phase 0: ublk spikes, trace-driven simulations, radix-map prototype, crypto review, S3 probe sweep, `replicix-core` extraction (conditional on ADR-19); decision records for the ADR gates and `bench/BASELINE.md` | — |
| 01 | `wip/01-p1-local-volume.md` | wip (not started; revised 2026-11-12 after the independent review) | phase 1: local-only volume; byte layouts decided first (M1.A0, maintainer sign-off), frozen formats (`lunix-format`), 17 crates built by parallel milestones M1-M17 (workspace, iface and testkit, crypto, journal, overlay, map, index, store, space, destage, ublk, engine, `lunixd`, CLI, fsck, local GC and compaction), exit tests, §20 performance gate and the ADR-5 chunk-size benchmark, and the zstd dictionary study (M18a: codec id 3 and a trainer/evaluator crate in replicix-core; M18b: the Lunix and Constellation study harness) | 00 |
