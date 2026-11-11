# Lunix Roadmap (v1)

These are the phased milestones for v1. Sequenced work orders live under
[`wip/`](wip/) and [`done/`](done/); see [`README.md`](README.md). The spec
is [DESIGN.md](../../explanation/DESIGN.md).

Each phase builds on the previous one and ends with its gates green. From
phase 1 on, the format elements are final: the journal record, the
segment and its CCH1 extent framing, the map node, the commit kinds, the
bucket layout and the key hierarchy. Later phases add code, not
migrations. Encryption is therefore *designed* in phase 1 even where it is
implemented later.

## Phase 0: Spikes, simulations, shared crates

Phase 0 resolves the ADR gates before any format freezes.

**ublk spikes on Linux 7.0** (ublk note §9):
- a libublk null/loop baseline against `null_blk`;
- copy against USER_COPY against BATCH_IO, at 4K and 64K, QD 1/32/128;
- a kill -9 + QUIESCE recovery cycle under fio, with ext4 mounted;
- an `UPDATE_SIZE` online grow;
- FLUSH/FUA visibility.

**Trace-driven simulations** on MSR, Alibaba and Tencent block traces:
- chunk size (16, 64 and 256 KiB): cold RMW, uploaded bytes, map size and
  map-seal write amplification per commit interval;
- S3-FIFO against SIEVE against W-TinyLFU.

**Prototype** of the radix Merkle map: seal cost and memory at 1 and
10 TiB.

**Crypto review** of ADR-11.

**Shared crates:** extract `replicix-core` (`objcas`, `chunkfmt`,
`upload-concurrency`, `platform-lite`), if the maintainer accepts ADR-19.

**Exit:** a decision record per gate appended to DECISIONS.md, and a
baseline table in `bench/`.

## Phase 1: Local-only volume

**Scope:**
- `lunixd` skeleton, the control socket and `lunix volume
  create|attach|detach|status`.
- The ublk device: params, queues, recovery on restart.
- The journal and the dirty overlay.
- The destage pipeline: zero, BLAKE3, dedup index, `auto` compression,
  segments.
- The map with local commits, and DISCARD / WRITE_ZEROES.
- The space ladder with `on_full`, and online grow.
- `fsck`, including index rebuild.
- Encryption: format, key slots, AEAD.

**Exit:**
- fio verify passes.
- xfstests passes on ext4 and xfs over the device.
- A kill -9 crash loop at every step passes, with fsck clean.
- Dedup and compression ratios are reported.
- Perf meets the §20 targets for local hits and writes, or the
  shortfall is explained.

## Phase 2: One S3 backend (authority), async mode

**Scope:**
- `backend add` with the initial sync, `doctor`, and the lease with
  EPOCH commits.
- FULL commits, async cuts with `T`/`B`/`B_max`.
- Eviction (S3-FIFO), miss handling with ranged, coalesced, hedged GETs,
  and readahead.
- `volume import` (mount elsewhere).
- GC and compaction.
- The fault-injecting object store.

**Exit:**
- The "destroy local, import at the latest commit, fsck clean" loop
  passes.
- Two-daemon fencing scenarios pass.
- The Stateright model of cut/commit/lease is green.
- A volume larger than its local cache passes fio verify.

## Phase 3: Sync mode and the durability ladder

**Scope:** DELTA commits, flush-group coalescing, DELTA folding, the
`flush_timeout` policy, and runtime mode switches.

**Exit:**
- After any kill, every FLUSH-acknowledged write is present on the
  bucket.
- The fsync latency and request-cost report is published.

## Phase 4: Snapshots, clones, time travel, retention

**Scope:** snapshot pins, clones with a family keyring, read-only
`mount-at`, retention windows, shrink, and `verify`.

**Exit:** snapshot/clone churn under load passes with the GC oracle, and
time-travel mounts pass fsck.

## Phase 5: Multiple backends

**Scope:**
- Mirror roles and the state machine: backfill (hot-first, restartable,
  AIMD, budget) and tail replication.
- Source selection and hedging across backends.
- Authority transfer, `steal`, mirror GC, and `migrate plan`.
- Provider adapters (GCS generation preconditions).

**Exit:**
- Add and remove a mirror under load with no I/O errors.
- Root equality is verified across AWS, R2 and OVH.
- DR steal is tested from a mirror.

## Phase 6: Kubernetes CSI

**Scope:**
- `lunix-csi` controller and node.
- Block and Filesystem modes, RWO and RWOP, expand, snapshots and clones.
- `LunixBackendSet`, credential providers, and the Helm chart: a
  privileged `lunixd` DaemonSet plus the plugin.

**Exit:** csi-sanity passes, the kind e2e passes, and a pod reschedule
across nodes works.

## Phase 7: Mobility

**Scope:**
- Populating the heat map and the warm-start prefetcher.
- `lunix warm` on a target node.
- A `migrate` orchestration, plus the how-to guide "move a pod between
  regions and clouds".
- Cost reporting.

**Exit:** a us-east → ap-southeast cross-cloud move with documented
downtime, and warm against cold time-to-steady-state measured.

## Phase 8: Hardening and release

**Scope:**
- `lunix upgrade` via QUIESCE/recovery with no unmount.
- Fuzzing every decoder, and perf gates in CI.
- Packaging (static musl binary, deb/rpm, container image).
- Docs: tutorials, reference.

**Exit:** a release checklist modelled on Constellation's RELEASING.md.

## Later (post-v1 candidates)

- `sync-fast-wal` on S3 Express One Zone.
- `sync-quorum` / `sync-all` mirror policies.
- Peer-to-peer cache warm-up during migration.
- Per-volume zstd dictionaries.
- Cold-segment recompression.
- The SHARDS cost advisor.
- File-system-aware cache hints.
- Erasure-coded cold history.
- Object Lock "immutable history" mode.
