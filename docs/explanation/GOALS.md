# Lunix Goals

Lunix is a Linux block-device daemon: it presents volumes as `/dev/ublkbN`
devices through **ublk** (the kernel's userspace block driver), stores
their data as compressed, deduplicated, optionally encrypted chunks on any
local file system, and can replicate them to one or more S3-compatible
buckets that then become the durable truth while the local file system
turns into a cache. Rust, io_uring, one daemon per host. It is the block
sibling of [Constellation](https://github.com/replicix/constellation)
(the moon of the Replicix universe; the name echoes the storage LUN).

Status: **design phase** (2026-11-11). Nothing is implemented. The spec is
[DESIGN.md](DESIGN.md); every choice in it is an ADR in
[DECISIONS.md](DECISIONS.md), currently *proposed*.

## High level goals

- **A block device on any local file system.** `lunix volume create db
  --size 200G --dir /data/lunix` gives a thin-provisioned device on ext4,
  xfs, btrfs, ZFS or anything else POSIX. All-zero blocks and discarded
  ranges cost nothing; data is deduplicated by content hash and compressed.
- **Honest space semantics.** Without a backend the local file system is
  the store: writes succeed while space remains above a configurable free
  floor; at the floor writes either **wait** (optionally with a timeout) or
  **fail with ENOSPC**, per volume. Reads never fail for lack of space.
- **S3 attachable at any time.** `lunix backend add db s3://bucket/prefix`
  starts an initial sync; from then on the local store is a cache with
  eviction and the volume can be **larger than the local disk**.
- **Three topologies, switchable live (ADR-26).**
  - **Local only**, **local + S3**, or **S3 only**. S3 only means no
    local files at all, for diskless or ephemeral nodes.
  - The local tier (`store`, `cache`, `none`) and every backend can be
    added or removed while the device is in use, with no I/O errors and
    no detach.
  - S3-only FLUSHes are durable on the bucket (sync mode). A keeper
    process holds the RAM write buffer across daemon restarts.
- **S3 always holds a mountable, crash-consistent image.** Whatever the
  mode, the newest commit on the bucket is a legal post-power-loss state
  of the device (a prefix cut at a flush boundary). Losing the host and its
  local disk loses at most the writes after the last commit.
- **The user picks the data-loss window.**
  - **async** (default): commits every *T* seconds or *B* dirty bytes. The
    worst-case loss after the host and its disk die is bounded by *T* and *B*.
  - **sync**: every FLUSH/FUA waits for S3 durability, using group commit
    and one round trip per flush group.
  - **write-through**: every write waits for S3 durability. Nothing
    acknowledged is lost even when the whole client machine is lost.
  - These are presets of two settings: `write_ack` and `flush`, each
    `local` or `s3` (ADR-27). The fsync of a file system on the device
    arrives as FLUSH/FUA.
  - **local** (no backend): FLUSH waits only for the local disk.
- **Single writer, enforced.** One host writes a volume at a time.
  Conditional writes on the authority bucket fence a stale or partitioned
  writer by construction, without relying on clocks.
- **Mirrors you can add and remove live.** A volume can have several
  buckets in different regions or with different vendors. Adding one
  backfills it in the background. Removing one drains it gracefully.
  Reads go to the nearest healthy copy. This is how a workload moves from
  the USA to Asia without downtime beyond the final cutover.
- **Kubernetes-native and portable.** A CSI driver serves RWO / RWOP
  volumes in Block or Filesystem mode. The volume ID is self-describing,
  and the bucket is the only source of truth. A pod can move between
  clusters, regions and clouds and keep its volume; the cache at the new
  place starts cold.
- **Fast.**
  - Cache hits and local writes run at local-NVMe class latency, with
    pinned io_uring queue threads and no hashing or compression on the
    acknowledgement path.
  - Cold reads fetch with parallel, coalesced and hedged ranged GETs.
  - Targets are in DESIGN.md §20 and are gated by benchmarks.
- **Online grow, careful shrink, cheap snapshots and time travel (v1).**
  - Snapshots, read-only time-travel mounts at any retained commit,
    rollback, retention policies and per-snapshot space accounting come
    from retained commits.
  - Thin clones are post-v1. The format already supports them
    (ADR-25).
- **Encryption and integrity.**
  - Optional end-to-end encryption, with keys that never reach the bucket
    in clear.
  - Content-addressed chunks verified on every fetch.
  - A Merkle block map, so one root hash proves that two mirrors are
    identical.
- **Restartable.** The daemon can crash or be upgraded without unmounting
  the file system on top: I/O stalls and resumes through ublk user
  recovery.
- **Thoroughly tested.**
  - Crash loops ("kill -9, destroy the local disk, mount from S3, fsck").
  - Fencing tests with two daemons.
  - Fault-injecting object stores.
  - fio verify, plus xfstests on file systems layered on the device.

## Non-goals

- **Multi-writer block access** (shared-disk clusters, RWX block). Use
  Constellation for shared files.
- **A file system.** Lunix stores blocks; the user runs ext4/xfs/... on top.
- **Kernel modules.** Only the in-tree `ublk_drv` is used.
- **Non-Linux hosts.** ublk is Linux-only. Kernels older than the floor
  in ADR-17 are not supported.
- **Cross-tenant global dedup.** Dedup stays inside a volume family:
  a volume plus its frozen lineage bases (post-v1 clones). Lineage-based
  sharing is explicit. See ADR-13 and ADR-25.
- **Erasure coding across clouds in v1.** The commit format leaves room for
  it (ADR-15).
- **Synchronous multi-site write quorums in v1.** Mirrors are asynchronous
  followers of one authority. `sync-all` is a later option.

## Motivating scenarios

1. **Laptop / single server:** a 2 TB thin volume for VMs or a database on
   a 500 GB SSD, backed by an R2 or OVH bucket; dedup and compression keep
   the local footprint small, the bucket keeps it safe.
2. **Kubernetes stateful pod:** Postgres on a Lunix RWOP volume; node dies;
   pod reschedules to another node or another cluster; the volume mounts
   from the bucket and loses at most the async window.
3. **Region move:** a pod in us-east must move to ap-southeast. The
   steps:
   1. Add an Asian bucket.
   2. Lunix backfills it while the pod runs.
   3. Pre-warm a node in Asia from the local bucket.
   4. Stop the pod; the final commit reaches both buckets; the pod
      starts in Asia.
   5. Retire the US bucket.
4. **Golden images (post-v1, thin clones):** a hundred CI volumes cloned
   from one base volume share its chunks in the bucket and in each node's
   cache. In v1 a restore is a full copy.
5. **Ransomware / bad deploy:** mount the volume read-only as it was an
   hour ago, from retained commits; optionally Object-Lock the bucket.

## Prior art

Surveyed in [research/2026-11-11-prior-art.md](research/2026-11-11-prior-art.md).
Closest designs: LSVD (log-structured virtual disk over S3, research
code), ZeroFS (NBD/NFS over S3 on an LSM, AGPL, no dedup), s3backer, Neon's
pageserver, EBS snapshots with lazy hydration, Ceph RBD mirroring,
dm-thin / dm-writecache / VDO. None combines block semantics, a
continuously crash-consistent bucket image, dedup + compression +
encryption, and live cross-vendor mirroring.
