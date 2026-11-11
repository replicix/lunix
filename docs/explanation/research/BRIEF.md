# Lunix research brief (shared context for research agents)

Lunix is a new Replicix subproject: a high-performance, low-latency Linux
block-device daemon written in Rust, using **ublk** (Linux userspace block
device driver) and **io_uring**. It is the block-device sibling of
**Constellation** (`/home/bra/cvs/replicix/constellation`, a distributed POSIX
FS on S3 written in Rust; read its `docs/explanation/DESIGN.md`,
`DECISIONS.md`, and `crates/` as needed). Dev host: Linux 7.0, `ublk_drv`
module present.

Product vision (from the owner):

- Create a block device on any local file system. Data is kept as chunks
  addressed by hash (dedup); all-zero blocks are not stored; chunks are
  compressed; optional encryption (like Constellation).
- Without a backend the local FS is the store: the user sets a device size,
  writes succeed while FS space remains; when it runs out (below a settable
  free-space floor), writes either block until space appears or fail with EIO
  / ENOSPC (configurable).
- One or more **S3 backends** can be attached at any time. Once attached, the
  local FS becomes a cache (write-through or write-back), an initial sync
  starts, and Lunix replicates chunks plus the metadata needed to mount the
  device elsewhere. With S3, the device can be larger than local disk (local
  FS = cache with eviction).
- Sync mode (slow; S3 always current, every FLUSH waits for S3) or async mode
  (bounded data loss window chosen by the user). In async mode S3 must always
  hold a *consistent, mountable, crash-consistent snapshot* of the device as of
  the last sync point — losing the local FS loses only writes after that point.
- Single writer is the norm; requiring S3 conditional writes (If-None-Match /
  If-Match) is a design choice to evaluate.
- Volume grow (and shrink).
- Use standalone or as a **Kubernetes CSI driver** (RWO block / filesystem
  volumes) so pods can move between regions/clouds and keep their volume
  (cold cache at the new place).
- **Multiple S3 backends, mirrored, added/removed live**: e.g. a pod moves
  from the USA to Asia; add an Asian bucket, Lunix syncs USA->Asia (possibly
  cross-vendor), then the USA bucket is removed gracefully.
- Goal: rock-solid and ultra fast, while S3 is the durable backing store.
  Novel/clever algorithms and features are explicitly welcome.

## Output conventions for research notes

Write ONE Markdown file at the path you were given, in
`/home/bra/cvs/replicix/lunix/docs/explanation/research/`. Structure:

1. `# Title` then `Date: 2026-11-11`, `Author: research agent (Sonnet)`, `Question:` (one paragraph).
2. `## Summary` — 5-12 bullets, the conclusions that matter for Lunix.
3. Body sections with concrete facts, numbers, API names, kernel/crate
   versions. Prefer primary sources (kernel docs/source, papers, vendor docs,
   crate docs, benchmarks).
4. `## Recommendations for Lunix` — concrete, opinionated, with trade-offs.
5. `## Open questions / UNVERIFIED` — flag anything you could not confirm.
6. `## Sources` — URLs (and repo paths for local code).

Do not create any other files; do not run `git`. Be specific and dense; a
good note is 300-700 lines. Do not pad.
