# Lunix Glossary

This glossary defines the terms used across the Lunix docs. The terms
mean exactly this in [DESIGN.md](DESIGN.md) and
[DECISIONS.md](DECISIONS.md).

- **Volume:** a thin block device with a uuid, a size and fixed
  chunk/codec/crypto settings, exposed as `/dev/ublkbN` (symlink
  `/dev/lunix/<name>`).
- **Family:** the key domain: a volume plus its snapshots and (post-v1)
  clones. A volume dedups only against its own chunks and its frozen
  lineage bases (ADR-13, ADR-25).
- **Lineage:** the parent volumes and base commits a post-v1 clone reads
  through (`volume.lineage`). It is always empty in v1.
- **Hold:** `holds/<child>` in a parent volume, pinning a clone's base
  commit so that GC keeps it.
- **Reader pin:** `readers/<host>-<id>`, which keeps a time-travel
  mount's commit alive while it is mounted.
- **Chain floor:** the lowest retained commit seq, recorded in each FULL.
  Unpinned commits below it may be deleted.
- **Owner:** the single host currently allowed to write a volume. It
  holds the lease.
- **Lease:** a CAS object on the authority backend holding `epoch`,
  `owner`, `deadline` and `released`. It gives liveness. Safety comes
  from the commit chain.
- **Epoch:** a monotonic owner generation. It is bumped on every
  ownership change, and an EPOCH commit marks it in the chain.
- **Zombie:** a former owner that has not noticed it lost the lease. It is
  fenced by its next commit attempt (412).
- **seq:** the global write sequence number, assigned at admission.
- **Cut:** the device state containing every write with `seq ≤ c`. It is
  always a legal post-power-loss image.
- **Commit:** a create-only object `commits/<seq>`.
  - **FULL** commits name a map root and new segments.
  - **DELTA** commits carry journal records inline (sync mode).
  - **EPOCH** commits mark an owner change.
  - **CONFIG** commits record the backend set, roles, states and the
    authority. They are the only S3 record of membership (ADR-24).
  - **TRANSFER** commits hand the authority to another backend. A
    TRANSFER is written by CAS on the old authority, and that write is
    the transfer's linearization point.
- **Tip:** the highest contiguous commit on a backend.
- **Journal:** the local, O_DIRECT, append-only record of acknowledged
  but not yet destaged writes.
- **Overlay:** the in-memory LBA map of journal-resident data. Reads
  check it first.
- **Destage:** the off-path pipeline that turns overlay data into stored
  chunks: zero → hash → dedup → compress → encrypt → pack.
- **Chunk:** a fixed-size, aligned unit of the device (default 64 KiB).
  Its id is the (keyed) BLAKE3 hash of its plaintext. An all-zero chunk
  is a **hole** and is never stored.
- **Extent:** one stored chunk inside a segment: header, compressed and
  optionally encrypted payload, tag.
- **Segment:** an immutable packed object of extents, about 16 MiB, with
  a footer index. Its bytes are identical locally and on every backend.
- **Map:** the fixed-radix Merkle tree from chunk index to `chunk_id` plus
  location **hint**. Its root hash identifies the volume's content.
- **Hint:** a `(segment, offset, clen)` stored next to a `chunk_id` in a
  leaf, so a cold read costs one GET. It is verified on use, and falls
  back to the chunk index.
- **Chunk index:** `chunk_id → location`, kept as LSM runs. It is used
  for dedup and for stale hints, and can be rebuilt from segment footers.
- **Backend:** an S3 location holding a copy of the volume.
  - The **authority** holds the lease and must support conditional
    writes.
  - A **mirror** is a byte-identical follower.
- **Cursor:** the highest commit a backend holds.
- **Backend states:** Joining → CatchingUp → InSync → Draining → Removed.
- **Mode:** a durability preset. `local` (no backend), `async` (commit
  every T or B, cap `B_max`), `sync` (FLUSH waits for a DELTA commit) or
  `write-through` (every write waits for one).
- **write_ack / flush:** the two durability settings behind the modes
  (ADR-27). Each is `local` or `s3`, and says where data must be before
  a write or a FLUSH completes.
- **Space ladder:** what happens as local space runs out: evict clean →
  compact → throttle → `on_full` (`wait` with an optional timeout, or
  `error` = ENOSPC).
- **Heat map:** a `heat/<seq>` object of hot ranges and first-touch
  order, used to warm a new owner's cache.
- **Authority transfer:** moving the arbiter role from one backend to
  another while the pod keeps running: TRANSFER on the old authority,
  then EPOCH on the new one (ADR-24).
- **Local tier:** the owning host's local storage for a volume. Its
  role is `store` (complete copy), `cache` (partial, evictable) or
  `none` (no files; S3 only). It can change live (ADR-26).
- **Topology:** local only (`store`, no backend), local + S3, or S3 only
  (`none`).
- **Write buffer:** with no local tier, the memfd ring of journal
  records that takes writes before they are committed to S3.
- **Keeper:** the systemd FD store or `lunix-keeper` process that holds
  the write buffer's memfd, so it outlives a `lunixd` restart.
- **Sink switch:** moving new writes from one write sink (journal or
  write buffer) to the other at a seq barrier, without pausing I/O.
- **Hydrate:** copy every live segment into the local tier, turning
  `cache` into `store`.
- **Steal:** taking ownership without a graceful release. It waits for
  the lease TTL, or `--force`.
