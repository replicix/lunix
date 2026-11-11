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
  and why. All are *proposed* as of 2026-11-11.

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
- [Fact-check](research/2026-11-11-fact-check.md): primary-source
  verification of the design-critical claims.
