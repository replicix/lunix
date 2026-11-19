# Lunix Glossary

This glossary defines the terms used across the Lunix docs. The terms
mean exactly this in [DESIGN.md](DESIGN.md) and
[DECISIONS.md](DECISIONS.md).

- **Volume:** a thin block device with a uuid, a size and fixed
  chunk/codec/crypto settings, exposed as `/dev/ublkbN` (symlink
  `/dev/lunix/<name>`).
- **Family:** the key domain: a volume plus its snapshots and (post-v1)
  clones. It shares one keyring. A volume dedups only against its own
  chunks and its frozen lineage bases (ADR-13, ADR-25).
- **Lineage:** the parent volumes and base commits a post-v1 clone reads
  through (`volume.lineage`). It is always empty in v1.
- **Hold:** `holds/<child>` in a parent volume, pinning a clone's base
  commit so that GC keeps it.
- **Reader pin:** `readers/<host>-<id>`, which keeps a time-travel
  mount's commit alive while it is mounted.
- **Chain floor:** the lowest retained `commit_seq`, recorded in each FULL.
  Unpinned commits below it may be deleted. In phase 1 it is the older of
  the last `N` retained FULL commits (default 2), so it moves with every
  FULL; commits below it are deleted after the GC grace.
- **Owner:** the single host currently allowed to write a volume. It
  holds the lease.
- **Lease:** a CAS object on the authority backend holding `epoch`,
  `owner`, `deadline` and `released`, and optionally `transferred_to`
  after an authority transfer. It gives liveness. Safety comes
  from the commit chain.
- **Epoch:** a monotonic owner generation. It is bumped on every
  ownership change, and an EPOCH commit marks it in the chain. Every
  commit kind and the lease carry it, and mount rejects a decrease. It is
  not the DEK generation (`dek_gen`).
- **Zombie:** a former owner that has not noticed it lost the lease. It is
  fenced by its next commit attempt (412).
- **write_seq:** the global write sequence number, assigned at admission
  across all queues and journal lanes. A FULL commit's `cut_seq` is a
  `write_seq`.
- **commit_seq:** a commit's position in the chain: `parent_seq + 1`, with
  the genesis commit at 0. It names the object `commits/<commit_seq>`. It
  is unrelated to `write_seq`.
- **Genesis commit:** the first commit, a FULL with an empty map, with
  `commit_seq` 0. Its `parent_hash` is the BLAKE3 of the `volume` object's
  bytes.
- **Cut:** the device state containing every write with `write_seq ≤ c`.
  It is always a legal post-power-loss image.
- **Commit:** a create-only object `commits/<commit_seq>`. Every kind
  carries `epoch`, `parent_seq` and a **parent hash**.
  - **FULL** commits name a map root and new segments.
  - **DELTA** commits carry journal records inline (sync mode).
  - **EPOCH** commits mark an owner change. An optional `authority` field
    names the backend that becomes the authority.
  - **CONFIG** commits record the backend set, roles, states and the
    authority. They are the only S3 record of membership (ADR-24).
  - **TRANSFER** commits hand the authority to another backend. A
    TRANSFER is written by CAS on the old authority, and that write is
    the transfer's linearization point.
- **Parent hash:** `parent_hash`, the BLAKE3 of the parent commit object's
  bytes as stored. It links the chain, so valid commits of two histories
  cannot be spliced (ADR-11).
- **Tip:** the highest contiguous commit on a backend.
- **Journal:** the local, O_DIRECT, append-only record of acknowledged
  but not yet destaged writes.
- **Journal lane:** one ring of journal files per ublk queue. Lanes share
  the global `write_seq` and are merged at recovery. A FLUSH is a group
  across lanes.
- **Overlay:** the in-memory LBA map of journal-resident data. Reads
  check it first.
- **Destage:** the off-path pipeline that turns overlay data into stored
  chunks: zero → hash → dedup → compress → encrypt → pack.
- **Chunk:** a fixed-size, aligned unit of the device (default 64 KiB).
  Its id is the BLAKE3 hash of its plaintext, keyed with the addressing key
  in `encrypted` and `integrity` mode and unkeyed in `plain` mode. An
  all-zero chunk is a **hole** and is never stored.
- **Addressing key:** the BLAKE3 key of the chunk id (and, separately, of
  map-node ids when encrypted). Derived from the KMK in `encrypted` mode and
  from the integrity key in `integrity` mode, by purpose. Keying closes the
  `id_len = 16` collision search, because nobody without the key can compute
  an id (ADR-6).
- **Extent:** one stored chunk inside a segment: header, compressed and
  optionally encrypted payload, tag.
- **Tag:** the 16 bytes that end every extent. It is the AEAD tag on an
  encrypted volume and BLAKE3-128 over every byte of the extent before the
  tag on an unencrypted one: keyed in `integrity` mode, unkeyed in `plain`
  mode. A local read verifies it instead of recomputing the chunk id.
- **dek_gen:** the DEK generation, a `u32` in each encrypted extent's
  header and AAD. Rotation raises it and protects only new writes. It is
  unrelated to the lease and commit `epoch` (ADR-11).
- **Crypto mode:** the volume's protection level, fixed in the descriptor
  at creation (ADR-11). `encrypted`: AEAD. `integrity` (the default): an
  HMAC on every commit, keyed tags and an integrity key in the keyring.
  `plain`: unkeyed BLAKE3 checksums, ids and tags, no key material; it
  detects corruption only and protects against nobody who can write the
  bucket.
  `lunix volume create` takes `--encrypt`, `--integrity` or `--plain`.
- **Integrity key:** the key that an `integrity` volume's keyring slots
  wrap. It keys the commit and lease HMACs, the extent tags, and (through
  the addressing key derived from it) the chunk ids. A local
  volume gets a key-file slot by default (`integrity.key` beside the
  volume); `--passphrase` adds a passphrase slot.
- **Keyring:** `families/<id>/keyring`, the one mutable key object of a
  family. It holds the key slots that wrap the KMK (or, for an `integrity`
  family, the integrity key; a `plain` family has no slots). It is written by CAS on the authority, copied
  to mirrors, and carries `keyring_seq` in a MAC'd body (ADR-11).
- **Rollback floor:** `{max_commit_seq, tip_hash}` in `state/pins` version
  2, the highest verified tip this host has seen. It is raise-only and
  written after every verified attach and every durable FULL commit. Attach
  refuses a lower tip as `Rollback` and a different hash at the same height
  as `Fork`. Phase 5 adds the maximum over mirrors and a Kubernetes floor
  (ADR-31). A host with no floor cannot detect rollback.
- **Scan:** a read of an extent with no id expectation (fsck, rebuild, GC
  liveness). It must set `verify_id`. Every data read in the engine, by
  contrast, passes the leaf's `id_prefix` (rule R1, DESIGN §6).
- **KMK fingerprint:** a value derived from the KMK by purpose. The
  orchestrator supplies it at attach, and a volume with a different KMK is
  refused. The CLI pins it locally on first attach instead (ADR-11).
- **Descriptor MAC:** the MAC on the `volume` object, keyed by a
  KMK-derived key. Together with the rule that supplied key material means
  "must be encrypted", it blocks the encryption downgrade (ADR-11).
- **Segment:** an immutable packed object of extents, about 16 MiB, with
  a footer index. Its bytes are identical locally and on every backend.
- **Map:** the fixed-radix Merkle tree from chunk index to `chunk_id` plus
  location **hint**. Its root hash identifies the volume's content. Its
  fanout, `map_fanout`, is 256 or 512 (default 512), fixed per volume.
- **Hint:** a `(segment, offset, clen)` stored next to a `chunk_id` in a
  leaf, so a cold read costs one GET. It is verified on use, and falls
  back to the chunk index.
- **Chunk index:** `chunk_id → location`, kept as LSM runs (`index/<pack>`).
  It is used for dedup and for stale hints, and can be rebuilt from segment
  footers. A FULL names only the runs that were durable before it, so runs
  may lag one commit.
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
  compact (real in phase 1 for local-only volumes) → throttle → `on_full`
  (`wait` for at most `full_timeout`, 60 s by default, then ENOSPC and the
  full latch; or `error` = ENOSPC at once). ADR-32.
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
- **Flush lane:** the dedicated connection pool per backend that carries
  only commits and small DELTA bodies, so an fsync never queues behind
  bulk uploads (ADR-28). Everything else uses the **bulk lane**.
- **sync_dirty_max:** the cap on acknowledged-but-unuploaded bytes when
  `flush = s3`. It bounds fsync latency under a bulk writer (ADR-28).
- **Orphan:** a sealed segment or map pack that no commit names and whose
  id is above the highest the chain names, left by a crash before the
  commit. It is deleted at open, before the index catches up.
- **Steal:** taking ownership without a graceful release. It waits for
  the lease TTL, or `--force`.
