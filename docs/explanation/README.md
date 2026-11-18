# Explanation

This section covers why Lunix exists, how the architecture fits together,
and the decisions that shaped it. Background reading, not procedures.

Included here are:

- [Glossary](GLOSSARY.md): volume, family, owner, epoch, cut, commit,
  segment, map, hint, backend states. Read it first.
- [Goals](GOALS.md): goals, non-goals, motivating scenarios and prior art.
- [Design](DESIGN.md): the spec. It covers the ublk device, journal,
  destage, segments, Merkle map, commits and fencing, cache, GC, mirrors,
  resize, security, CSI, the failure matrix and the performance targets.
- [Decisions (ADRs)](DECISIONS.md): what was chosen, what was rejected,
  and why. All accepted on 2026-11-12, except ADR-32 (write back-pressure),
  proposed on 2026-11-18.

## Research notes

These are dated studies with sources. Cite them rather than redoing the
research. Items marked UNVERIFIED were not confirmed against a primary
source.

- [Research brief](research/BRIEF.md): the shared context and note format
  given to the research agents.
- [Constellation reuse](research/2026-11-11-constellation-reuse.md): what
  Lunix can depend on, extract or copy from Constellation, with a
  component table.
- [ublk + io_uring + Rust](research/2026-11-11-ublk-io-uring-rust.md):
  ublk feature flags, crash recovery, online resize, libublk, performance
  and containers.
- [Prior art](research/2026-11-11-prior-art.md): LSVD, ZeroFS, s3backer,
  Neon, SlateDB, EBS, Ceph RBD, Longhorn, VDO and dm-cache, plus the
  design lessons and pitfalls.
- [Data path](research/2026-11-11-data-path-hash-compress-encrypt.md):
  chunk size, hashing, zero detection, compression, encryption and
  threading, with benchmarks on an i9-14900K.
- [Cache, consistency, S3 protocol](research/2026-11-11-cache-consistency-s3-protocol.md):
  block semantics, local layout, commit chain and fencing, eviction and
  readahead, GC, S3 economics and the out-of-space policy.
- [CSI, multi-backend, mobility](research/2026-11-11-csi-multibackend-mobility.md):
  the CSI shape, fencing, mirror states, egress costs and a region-move
  runbook.
- [Novel ideas](research/2026-11-11-novel-ideas.md): a ranked list of
  techniques and the pitch for signature features.
- [Map leaf encoding](research/2026-11-11-map-leaf-encoding.md): compact
  block-map leaves. It covers a survey of qcow2, dm-thin, VDO, ZFS and
  LSVD, the security of shortened chunk ids, and benchmarks on an AMD EPYC
  (Zen 5) host. The chosen encoding (runs plus bit-packed columns, 16 B ids) cuts
  the leaves from 768 to 294-352 MiB/TiB. The benchmark code is in
  [spikes/leafenc](../../spikes/leafenc/).
- [Fact-check](research/2026-11-11-fact-check.md): primary-source
  verification of the design-critical claims.
- [Write back-pressure and the full volume](research/2026-11-18-write-backpressure.md):
  why a write can park for ever today, the rate law of a store with a GC
  grace, prior art (OpenZFS, RocksDB, LFS, SSDs, dm-thin), the paced
  admission band and bounded full-volume behaviour proposed as ADR-32,
  with the test list and an implementation sketch.
