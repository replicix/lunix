# fsync scope: should Lunix narrow what a FLUSH or FUA persists under `flush = s3`?

Date: 2026-11-12
Author: design reviewer (Fable)
Status: recommendation for the maintainer; proposes ADR-28. Nothing in DECISIONS.md or DESIGN.md has been edited.
Inputs: [fsync-trace/RESULTS.md](../../../bench/fsync-trace/RESULTS.md) (measured), [2026-11-12-fsync-background-staging.md](2026-11-12-fsync-background-staging.md) (modelled), [2026-11-12-fua-and-flush-semantics.md](2026-11-12-fua-and-flush-semantics.md) (code reading), [crypto-kat/RESULTS.md](../../../bench/crypto-kat/RESULTS.md) F5, [radixmap/RESULTS.md](../../../bench/radixmap/RESULTS.md) findings 1-2.

## 1. Recommendation

**v1: keep prefix cuts for FLUSH and FUA, add no new object kinds, and ship a small set of scheduling rules ("flush-lane discipline") that bound fsync latency without changing the format or the consistency contract.** Concretely: a dedicated connection pool for the commit PUT, an in-flight byte cap and a rate share for bulk uploads, a sync-mode cap on unflushed bytes that throttles admission, and parallel 4 MiB segments for a flush group over the DELTA cap. Reserve one field in the DELTA commit so staging of raw journal batches can be added later without a format change.

**Defer:** background staging of unflushed writes to S3 (all four variants of the staging note §6). Revisit after phase 3 with measured PUT latency by object size and a measured fsync-latency run under the mixed workload; the modelled gain is about 260 ms to 150 ms on a 140 ms floor and the modelled AWS request bill is 3x to 11x.

**Reject:** the FUA-only fast path (fua note §7 options B, C, D), pipelined commits that name objects not yet durable (staging variant B), multipart Complete as the commit (variant C), per-file sync, and dropping `UBLK_ATTR_FUA`. Reword ADR-3's rejection of "arbitrary subsets": the reason to keep cuts is that every commit is an image users and mirrors see, not that journaling file systems need write order (they do not; fua note §3).

## 2. Reasoning

### 2.1 What the device actually receives

- An ext4 or xfs fsync is one `PREFLUSH|FUA` bio, delivered as an empty FLUSH request followed within tens of microseconds by one FUA write of 4 KiB or less (fsync-trace F1, sequence in "Representative sequence"). fdatasync of an append is identical. Under `flush = s3` that is **two sequential durable steps, so two PUT round trips per fsync**, about 140 ms on S3 Standard at the measured 70 ms p50 (ADR-27) and about 14 ms on Express One Zone. DESIGN §20 says "about 1 PUT RTT"; that holds only for data-only fdatasync (overwrite of pre-zeroed space: PostgreSQL WAL, 41% of SQLite WAL commits in s6). No mechanism in this review changes this: the FUA write does not exist until the FLUSH completes, and Lunix cannot see the pairing (fua note §1.1, §7).
- The flush group carries everything any writer completed since the previous flush point (F2): 4.1 MiB at a 200 MB/s stream, 19 MiB p50 and 38 MiB max at 1 GB/s, at an unchanged 50 flushes/s. The small file contributes 4 KiB plus 16-24 KiB of ext4 journal. Without `sync_file_range` pacing (s2p) the stream contributed 24 KiB p50 and 8-12 MiB bursts in a 45 s run, because the stream had not yet reached the dirty thresholds; a writer sustained past about 15 s at 200 MB/s on a 30 GiB host (10% dirty ratio) enters continuous writeback, and s2a is then the honest steady state (my inference from the s2p write rates of 24-54 MB/s against the offered 200 MB/s; not measured over longer runs).
- A bare FUA (no FLUSH ahead of it) arises only from `O_DIRECT|O_DSYNC` overwrite of already-written blocks (s4: 200 FUA/s, zero FLUSH, 0-8 KiB between them; fua note §2.3, §4). None of PostgreSQL, InnoDB, RocksDB, SQLite, etcd, bbolt or Redis use it by default; the producers are QEMU `cache=directsync`, `fio --direct=1 --sync=1` and a PostgreSQL developer setting (fua note §4).
- Databases on their own volume produce tiny flush groups: 4-28 KiB per fsync in s1/s3/s6, with an occasional 204 KiB SQLite checkpoint. For them the flush-critical PUT is already at the 65-70 ms floor (staging note §3.3); nothing below removes the floor.

### 2.2 How often each mechanism would help

| workload | shape at the device | staging gain | FUA fast path gain |
|---|---|---|---|
| database, dedicated volume (Postgres, MySQL, SQLite, etcd) | FLUSH (+FUA) at commit rate; 4-28 KiB groups; checkpoint bursts paced over minutes | none: PUT time is the floor already | none: bare FUA absent by default |
| VM image (`cache=none`/`writeback`) | guest bulk writes plus guest fsyncs in one file: the s2a mix | yes when the guest streams: modelled 260 ms to 150-190 ms at 200 MB/s | none (FLUSH-based) |
| VM image `cache=directsync`, fio `--sync=1` | bare FUA per guest write | none | yes, but no production workload found |
| CI scratch, build trees | bulk, few fsyncs; should run `--unsafe-flush` or async | irrelevant | irrelevant |
| file server (NFS/SMB on ext4/xfs) | COMMIT/close fsyncs mixed with bulk copies | yes, modestly; fsync latency is rarely the bottleneck | none |

Both mechanisms pay off only for a mixed bulk-plus-fsync writer on one volume with `flush = s3` on Standard-class latency. That is GOALS' "laptop / single server" scenario with a VM on it, and nothing else in the motivating list.

### 2.3 Staging: what the model says and what it costs

- Latency is set by the size of the object in flight when the FLUSH arrives, not by the commit size (staging note §3.3). 16 MiB staged objects give 310-400 ms, no better than today's modelled 260 ms at a 1,000 MB/s link; 1-2 MiB objects give 135-160 ms against a 65-70 ms floor. These are a two-point linear fit (58 ms + 17 ms/MiB), not measurements.
- Request cost on AWS S3 Standard for the 200 MB/s stream: $229/month today, $774 at 4 MiB, $1,392 at 2 MiB, $2,627 at 1 MiB (§3.4). The same curve applies to any design that uploads the stream in objects of that size, staged or not: bytes per PUT = stream rate times object interval. Staging's unique contribution is only that the upload starts earlier, which shaves roughly half a PUT time off the critical path. On OVH and B2 requests are free, so there the price is latency, CPU (about 190 TLS PUT/s at 1 MiB) and the losses below; B2 cannot be an authority (no conditional PUT).
- **Destage interaction (ADR-4, ADR-12, ADR-13, DESIGN §5).** Staged objects must leave the host sealed with volume keys (ADR-11), so they are either (a) sealed segments, which means destage runs early: overwrite absorption is lost (LSVD measured write amplification 1.97 to 1.35 and 1.44 to 1.14 from a 32 MB window on some traces, staging note §5.1; Lunix's async window is 30 s), the LZ4 probe and zstd-1 run on data that may be overwritten, and every partial-chunk write needs a base-chunk RMW before it can be hashed, which on an S3-only volume is a GET (DESIGN §13.1); or (b) raw journal batches, which need a new key-derivation context (staging note §5.4), are uploaded a second time as segments at destage, and must be GET back when the 256 MiB `mem_buffer` (1.3 s at 200 MB/s) no longer holds them. Today's DELTA body is already (b) for groups under the 8 MiB cap, so (b) is the natural extension and (a) is a different pipeline.
- **Memory.** Staged bytes stay in the journal or memfd until a commit names them (§5.5), so staging does not relieve the S3-only memory ladder (ADR-26 point 3).
- **Mirrors (ADR-24).** Each staged object is copied to every mirror before the commit that names it; fan-out multiplies by the backend count.
- **GC.** Orphan staged objects after a crash or fence need the reachability sweep of DESIGN §11; harmless to correctness, extra work proportional to write volume.

### 2.4 FUA-only fast path: legal, but it buys non-cut commits for a workload nobody runs

- Legality is settled: `flushed prefix + {FUA write}` is a legal post-power-loss image under the block-layer contract, and jbd2, xfs, btrfs and the databases in scope order with FLUSH/FUA and checksums, never with write order (fua note §1, §3, §6). ADR-3's rejection text is wrong about the reason and should say so.
- The gain exists only for bare FUA with a bulk backlog (§2.1). For the dominant FLUSH-then-FUA pair the FUA commit is already minimal: it holds only writes completed in the microseconds between the FLUSH's completion and the FUA's arrival.
- The price is structural: `(last_commit_cut, cut]` framing becomes a seq set; replay must be highest-seq-per-block for writes, discards and holes across commits; `cut_seq`, `chain_floor`, retention and mirrors must understand non-contiguous commits; `mount-at --time`, `rollback` and the §8.6 restore-point table need a cut/non-cut marker, because a non-cut image is a state no instant of the guest's own view ever equalled (fua note §6). Option D (side objects never published) avoids the marker but adds an object kind and breaks the ADR-26 copy invariant on a mirror that cannot see it. All of this for QEMU `cache=directsync` and fio.
- Dropping `UBLK_ATTR_FUA` would remove bare FUA at the source but turns each `O_DSYNC` write into write plus full FLUSH and disables iomap's write-through path (fua note §7); no gain.

### 2.5 Why prefix cuts stay, and for which reason

Keep "every commit is a cut" (ADR-3) because:
1. Every commit is a published image: a restore point (ADR-25), a time-travel mount target, a rollback target, and a mirror's possible failover state (DESIGN §8.6, ADR-24). A cut is a state the guest passed through; a non-cut image is a crash image the guest never saw. Users reasoning about "the volume as of 10:04" need the former.
2. The chain stays linear and single-numbered: `cut_seq`, `chain_floor`, `(last_cut, cut]` DELTA framing, FULL folding, retention thinning and GC roots all work on one integer. crypto-kat F5 adds `parent_hash` and chain verification from the newest FULL; a second validity notion ("commit names a missing object", staging variant B) would sit beside it and needs its own tip-finding rule.
3. Real disks persist non-prefix subsets rarely; Lunix would do so reliably, which widens exposure to latent guest bugs (fua note §3). Not a correctness argument, a blast-radius one.

The old reason ("ordering matters to journaling file systems") is not supported by the code and should be replaced; see the ADR-3 amendment in §3.

### 2.6 The simpler alternatives capture most of the benefit

- **Lane isolation.** The commit PUT must never queue on the NIC or in a connection behind a 16 MiB segment body. S3 is HTTP/1.1 (one body per connection, staging note §4), so this is a connection-pool rule: a flush lane of 2-3 connections that carries only commits and bodies under the DELTA cap, and a bulk lane with an in-flight byte cap and a token bucket at a share of measured goodput. RocksDB's two-priority rate limiter is the published pattern. Request cost: zero.
- **A sync-mode unflushed-byte cap.** Near link saturation the baseline degrades to about 1 s per fsync because groups grow (staging note §3.3 table: 1.05 s at a 250 MB/s link). Staging bounds that with its in-flight cap; the same bound comes from throttling admission when unflushed bytes not yet in a PUT exceed `sync_dirty_max`: fsync latency is then at most `cap / goodput + 2 RTT`. At saturation the writer must be throttled under any design (staging note §4); this puts the throttle where ADR-10's `B_max` already lives. Request cost: zero.
- **Parallel small segments for over-cap groups.** A 19 MiB group (1 GB/s case) as one 16 MiB plus one 3 MiB segment then a commit is modelled at about 400 ms; as five 4 MiB segments in parallel then a commit, about 190 ms. 4 MiB is inside ADR-7's 4-64 MiB range; compaction merges later. Request cost: more PUTs only when a group is already large, which is the same curve staging would be on.
- **Per-volume isolation and authority class.** A database on its own volume has 4-28 KiB groups and is at the floor already (§2.1). Express One Zone as the authority (already in ADR-26 consequences) cuts every term tenfold (PUT p99 about 7 ms) but charges $0.0032/GB on all uploaded bytes, about $1,660/month for the 200 MB/s stream (staging note §3.4), so it suits low-volume latency-sensitive volumes, not bulk. Documenting this and warning in the CLI when a `flush = s3` volume shows large flush groups is cheaper than any mechanism.

Together these turn the realistic bad case (mixed writer, roomy link) from about 260 ms to roughly 190 ms and bound the saturated case to the cap, against staging's modelled 135-190 ms with lost absorption and 3x to 11x the AWS request bill. The residual gap is about one half of a PUT on a 140 ms fsync floor.

### 2.7 S3-only (ADR-26) and write-through (ADR-27)

- S3-only volumes run `flush = s3` by construction, so they are where this matters most. The sync dirty cap must sit under `mem_buffer` (default 256 MiB) and becomes the first rung of the memory ladder ("cut and commit sooner"), which ADR-26 point 3 already describes without a number. Staging would not free RAM (§2.3).
- Write-through (`write_ack = s3`) is, in effect, continuous upload of every write with one PUT in flight; FLUSH completes at once and a FUA fast path is a no-op (fua note §6 condition 6). A user who wants data flowing to S3 continuously already has this setting; the lane rules apply unchanged.

### 2.8 Strongest counter-argument, and why I still choose this

*"Entanglement is total and measured; a 2 GB/s link is not what a laptop has; near saturation the baseline is 1 s per fsync and staging holds 150-190 ms; on OVH requests are free; LSVD and JuiceFS show pre-upload is a known pattern. Shipping v1 without it makes `flush = s3` unusable for the one scenario GOALS lists first."*

Answer: the saturated case is bounded by the dirty cap with zero new objects, because at saturation writes must be throttled under every design. What staging adds over the cap is about half a PUT time, bought with lost overwrite absorption, early destage or double upload, a new key-derivation context, orphan GC and mirror fan-out, on numbers that are a two-point fit. JuiceFS pre-uploads whole 4 MiB blocks of a file system that knows file boundaries; LSVD's sync point is a local SSD. Neither faces Lunix's constraint that a staged object must be a sealed, mountable unit named by a cut. The right move is to reserve the field, measure PUT latency by size and the end-to-end mixed workload in phase 3, and add raw-journal-batch staging then if the gap is still worth it.

## 3. Proposed ADR-28 text

```
## ADR-28: FLUSH and FUA stay prefix cuts; flush-lane discipline instead of staging or a FUA fast path

**Context:**
- The maintainer asked (2026-11-12) whether an fsync of one file could
  persist only that file's data under `flush = s3`. It cannot: the device
  sees no files, and journal transactions span files. The remaining
  question was whether to shrink the cost of a FLUSH that drags in every
  write completed before it.
- Measured on kernel 7.0 (bench/fsync-trace/RESULTS.md): an ext4 or xfs
  fsync is one empty FLUSH request followed within microseconds by one
  FUA write of 4 KiB or less; the flush group carries everything written
  since the previous flush point (4.1 MiB at a 200 MB/s stream, 19 MiB
  p50 and 38 MiB max at 1 GB/s) at an unchanged flush rate; a bare FUA
  with no FLUSH ahead of it arises only from `O_DIRECT|O_DSYNC` overwrite,
  which no database in scope uses by default.
- Modelled (research/2026-11-12-fsync-background-staging.md §3): staging
  unflushed writes to S3 ahead of the FLUSH helps only with 1-2 MiB
  objects (about 135-160 ms against a 260 ms baseline and a 65-70 ms
  floor) and costs 3x to 11x the AWS request bill, loses overwrite
  absorption, and needs sealed staged objects, orphan GC and mirror
  fan-out. Research/2026-11-12-fua-and-flush-semantics.md shows that
  persisting a FUA write alone is a legal crash image but produces
  commits that are not cuts.

**Decision:**
1. **Prefix cuts stay.** A FLUSH completes when a DELTA holding every
   write with `seq <= cut` is on the authority (ADR-10). A FUA write is
   treated as a FLUSH followed by itself: its commit is a cut at its own
   `seq`. Every commit on S3 remains a cut. No background staging and no
   FUA-only commit in v1.
2. **Two upload lanes per backend** (DESIGN §9):
   - the **flush lane**: a small dedicated connection pool (default 3)
     that carries commit objects and DELTA bodies up to the DELTA cap,
     and nothing else; its PUTs get the hedged-retry budget of reads;
   - the **bulk lane**: segments, map and index packs, FULL folds, mirror
     copies, GC and compaction traffic, with an in-flight byte cap
     `upload_inflight_max` (default 32 MiB) and a token bucket at
     `upload_share` (default 0.8) of the goodput measured by `doctor` or
     by the lane itself.
3. **A sync-mode unflushed-byte cap.** With `flush = s3`, bytes that are
   acknowledged, not yet in a PUT and not covered by a commit are capped
   at `sync_dirty_max` (default 64 MiB; with local tier `none`, at most
   `mem_buffer / 4`). Above it, admission throttles, as `B_max` does in
   async mode. The fsync bound is `sync_dirty_max / goodput + 2 PUT RTT`.
4. **Over-cap flush groups** are split into segments of at most
   `flush_segment` (default 4 MiB, inside ADR-7's range) uploaded in
   parallel on the bulk lane with AIMD; the DELTA commit that names them
   follows on the flush lane (ADR-9 upload order unchanged).
5. **Format reservation.** A DELTA commit's reference list is
   `refs: [{kind: segment | journal, id, hash}]`. `journal` (a sealed raw
   journal batch uploaded ahead of a FLUSH) is reserved and unused in v1,
   so staging can be added later without a format change.
6. **Guidance and telemetry.** Metrics `flush_group_bytes`,
   `flush_latency` and `flush_lane_queue`. The CLI warns when a
   `flush = s3` volume's p50 flush group exceeds 1 MiB and recommends a
   separate volume for the bulk writer, or S3 Express One Zone as the
   authority for low-volume latency-sensitive volumes. Documentation
   states that an ext4/xfs fsync costs two PUT round trips in
   `flush = s3`.

**Rejected:**
- *Background staging of unflushed writes* (sealed small segments, raw
  journal batches, pipelined commits, multipart Complete as commit,
  Express append). Modelled gain about one half of a PUT time over the
  rules above; loses overwrite absorption (LSVD measured write
  amplification 1.97 to 1.35 on some traces); early destage forces
  partial-chunk RMW, a GET on S3-only volumes; 3x to 11x the AWS request
  bill for the 200 MB/s case; new key-derivation context; orphan GC;
  mirror fan-out. Deferred, not closed: see the gate.
- *Pipelined commits naming objects not yet durable.* Breaks ADR-9's
  invariant and adds a second validity notion beside the F5 chain
  verification.
- *A FUA-only fast path.* Legal but it introduces non-cut commits, which
  need seq-set framing, highest-seq replay across commits for writes,
  discards and holes, a cut/non-cut marker in restore points, time-travel
  and rollback, and new GC and mirror rules, for a workload (QEMU
  `cache=directsync`, fio `--sync=1`) nobody runs in production.
- *Dropping `UBLK_ATTR_FUA`.* Removes bare FUA at the source but turns
  every `O_DSYNC` write into write plus full FLUSH and switches off
  iomap's write-through path.
- *Per-file sync.* The device sees no files; journal transactions span
  files.

**Consequences:**
- DESIGN §20's "sync-mode fsync latency about 1 PUT RTT" becomes
  "about 2 PUT RTT for an ext4/xfs fsync (FLUSH then FUA, serialised by
  the block layer); 1 RTT for data-only fdatasync". Standard: about
  140 ms; Express One Zone: about 14 ms.
- Request cost is unchanged for small groups; large groups cost one PUT
  per `flush_segment`, which is the same curve any sub-16 MiB upload is
  on.
- No new object kinds, no change to the copy invariant (ADR-26), to
  mirrors (ADR-24) or to retention (ADR-25). Write-through (ADR-27) is
  unaffected: its groups already use the flush lane.

**Gate (phase 0, extends P0.10; phase 3 for the end-to-end run):**
- P0.10 adds PUT p50/p99 at 4 KiB, 1, 2, 4, 8 and 16 MiB per provider,
  single-stream and 8 parallel, with and without a concurrent 200 MB/s
  upload on separate connections. The lane rule is confirmed if the
  4 KiB PUT p99 under load stays within 1.5x its idle value.
- Phase 3: the fsync-trace s2a/s2b mixes on ext4 and xfs over a real
  `flush = s3` volume, with and without rules 2-4, on AWS Standard and
  one zero-request-fee provider. If p99 fsync latency under the 200 MB/s
  mix remains above 2x the two-PUT floor, staging of raw journal
  batches (`refs.kind = journal`) is designed as ADR-29.

**Amends:**
- ADR-3: the rejection "Arbitrary subsets at sync points. Ordering
  matters to journaling file systems." is replaced by the text below.
- ADR-10: the sync bullet gains "FUA is a cut at its own seq; an ext4/xfs
  fsync is therefore two commits"; the cost note cites this ADR for the
  group-size curve.
- DESIGN §8.3: add lanes, `sync_dirty_max`, `flush_segment`, `refs[]`.
- DESIGN §9: two connection pools per backend; `doctor` reports PUT
  latency at 4 KiB, 1 MiB, 4 MiB and 16 MiB.
- DESIGN §13.1: `sync_dirty_max` is the first rung of the memory ladder.
- DESIGN §20: fsync target corrected as above.
- DESIGN §22: add "Staging of raw journal batches ahead of FLUSH
  (ADR-28 gate)".
```

**ADR-3 amendment** (replace the second Rejected bullet):

```
- *Arbitrary subsets at sync points* (for example, persisting a FUA write
  alone and committing older writes later). Such an image is a legal
  post-power-loss state: ext4, xfs, btrfs and the databases in scope order
  with FLUSH/FUA points and checksums, not with write order
  (research/2026-11-12-fua-and-flush-semantics.md §3). It is rejected
  because every commit is a published image: a restore point, a
  time-travel or rollback target and a mirror's failover state. A cut is
  a state the guest passed through; a non-cut image is one it never saw.
  Cuts also keep the chain single-numbered (`cut_seq`, `chain_floor`,
  `(last_cut, cut]` framing) and avoid exposing guests reliably to
  non-prefix states that real disks produce only rarely. See ADR-28.
```

## 4. Measurements still needed

Phase 0 (extend P0.10 and fsync-trace; no data path required):
1. **PUT latency by object size per provider** (AWS Standard, Express One Zone, R2, OVH, MinIO): p50/p99 at 4 KiB, 1, 2, 4, 8, 16 MiB, single-stream and 8 parallel, idle and with a 200 MB/s upload running on separate connections. Settles the §3.3 model (currently a two-point fit), the `flush_segment` default, and whether lane isolation keeps the commit PUT near its idle latency on a shared NIC.
2. **fsync-trace with realistic flush latency**: repeat s1, s2a, s2b, s4, s6 on a device with a 70 ms FLUSH/FUA delay (scsi_debug `delay`, or a ublk null target), 5 minute runs, to see how fsyncs queue, how far groups grow, and whether unaided buffered writers reach continuous writeback (my s2p inference). Settles the `sync_dirty_max` default and the "2 RTT" claim under queueing.
3. **Realistic mixes**: pgbench on ext4/xfs with a concurrent `cp` of a 4 GiB file; a QEMU guest (`cache=none`) running a build; `fio --direct=1 --sync=1` beside a stream. Gives bytes-per-flush for the workloads in §2.2 rather than for a synthetic 1 MiB writer.
4. **Overwrite ratio over 100 ms to 1 s windows** on the same traces. Sizes the absorption loss staging would incur (staging note §5.1, open question 8).

Phase 3 (needs the sync-mode data path):
5. **End-to-end fsync p50/p99 under the s2a/s2b mix** with and without rules 2-4, on AWS Standard and OVH or R2. The ADR-28 gate; decides whether ADR-29 (journal-batch staging) is opened.
6. **Request bill of a 1 hour mixed run** against the §3.4 model, to replace the modelled dollar figures before the CLI warning thresholds are fixed.
7. Only if staging is revived: `CompleteMultipartUpload` latency for 1-4 small parts, and the fencing behaviour of S3 Express append with a wrong offset (staging note §7, items 2 and 4).
