# Lunix

A Linux block-device daemon, written in Rust on **ublk** and **io_uring**.
It serves thin volumes from any local file system. Data is stored as
deduplicated, compressed and optionally encrypted chunks. Volumes can
replicate to one or more S3-compatible buckets, which then hold a
crash-consistent, mountable image at all times while the local disk
becomes a cache. The same daemon runs standalone or behind a Kubernetes
CSI driver, so a pod can move between regions and clouds and keep its
volume.

Lunix is the block-device sibling of
[Constellation](https://github.com/replicix/constellation). In the
Replicix universe it is the moon, and the name echoes the storage LUN.

**Status:** design phase. Nothing is implemented yet.

- Start with [docs/explanation/GOALS.md](docs/explanation/GOALS.md).
- Then read [DESIGN.md](docs/explanation/DESIGN.md) and
  [DECISIONS.md](docs/explanation/DECISIONS.md).
- The plan is [docs/plans/v1/ROADMAP.md](docs/plans/v1/ROADMAP.md).

Documentation layout: [docs/README.md](docs/README.md) (Diátaxis).
