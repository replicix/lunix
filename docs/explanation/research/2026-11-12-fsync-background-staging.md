# Decoupling fsync latency from bulk upload: background staging for `flush = s3`

Date: 2026-11-12
Author: research agent (Sonnet)
Question: With `flush = s3` on an S3-only volume (ADR-26, ADR-27), a FLUSH completes only when a DELTA commit holding every earlier write is on S3 (ADR-3, ADR-9, ADR-10, DESIGN §8.3). A small file's fsync therefore waits for unrelated bulk writes ("fsync entanglement"). Would continuous background staging of unflushed writes, plus one tiny create-only commit per flush group, fix this? What do comparable systems do, what do S3 mechanics and prices allow, and what are the risks?

Method note: sources were fetched on 2026-11-12 (vendor docs, the LSVD paper PDF, project docs). Prices and conditional-write facts are reused from [2026-11-11-fact-check.md](2026-11-11-fact-check.md). Anything not confirmed in this session is marked UNVERIFIED. Request-count and dollar figures are a model with stated assumptions, not measurements. Latency figures for 8-16 MiB PUTs are derived from two vendor-run data points and must be measured before they drive a decision.

## Summary of findings

1. **Staging helps only if staged objects are small or the flush does not wait for them.** A FLUSH must wait for every staged object that holds a write with `seq <= cut`, including objects still in flight. With 16 MiB staged objects at 200 MB/s, an object is always in flight and takes about 330 ms (estimate) to finish. The flush then pays that wait plus a commit PUT, which can be no better than today's design (about 260 ms modelled). Staged objects of about 1-2 MiB bring the modelled flush latency to about 125-160 ms, close to the 70 ms floor of one small PUT. They cost 95-190 PUT/s for the 200 MB/s stream, before commits.
2. **Prior art mostly avoids the problem by making the sync point local or replicated, not by pre-uploading to S3.** LSVD, Ceph RBD journaling, Neon (safekeepers) and EBS acknowledge on a fast local or replicated sink, and bulk upload is asynchronous. SlateDB and ZeroFS (S3-durable flush) pay the PUT latency per flush with batching. **JuiceFS is the closest precedent for pre-upload**: it uploads full blocks while the application is still writing, so close/fsync only has to flush the remainder and commit metadata.
3. **Plain PUT of staged objects is the portable, cheapest-to-reason-about mechanism.** Multipart upload adds a 5 MiB minimum part (except the last), a Complete round trip of unpublished latency, and orphan-upload GC, with no cost advantage. Complete accepts `If-None-Match: *` on AWS, R2 and OVH (per the fact-check), so a multipart Complete can double as the create-only commit (variant C). S3 Express append (`x-amz-write-offset-bytes`) is documented for AWS directory buckets only; MinIO's S3 Express mode implements it; other providers are UNVERIFIED.
4. **Cost.** For a 200 MB/s bulk stream (518 TB/month written), request cost on AWS S3 Standard is about $229/month today (16 MiB segments), about $310 with 16 MiB staged objects and 12 commits/s, about $465 with 8 MiB, about $774 with 4 MiB, about $1,392 with 2 MiB and about $2,627 with 1 MiB. OVH and B2 charge nothing per request, so there the choice is about latency and CPU only. S3 Express adds $0.0032/GB on all uploaded bytes, about $1,660/month for this stream, so it suits commits and small objects, not bulk.
5. **Overwrite absorption is the main hidden cost.** LSVD's own simulation shows a majority of bytes overwritten inside a 32 MB batch on some traces (write amplification 1.97 falls to 1.35 with coalescing). Staging every write at once gives that up. LSVD also forbids coalescing across batches because it would break prefix consistency; Lunix's destage today (DESIGN §5) coalesces over the async interval.
6. **Bandwidth fairness needs an application-level scheduler.** No source found documents HTTP/2 stream priorities on S3 (S3 endpoints appear to be HTTP/1.1 only; UNVERIFIED). The practical tools are separate connection pools, a token-bucket cap on the staging lane, a hard limit on staged bytes in flight, and small staged objects so a late flush never queues behind a long transfer. RocksDB's two-priority rate limiter is the closest published pattern.
7. **Encryption and dedup are compatible if staged objects are sealed journal batches.** Staged objects must be encrypted with volume keys (ADR-11) before leaving the host, so they cannot be raw records. Deduplication and per-chunk compression then happen only at destage time (the second pass), which means data is uploaded twice unless staged objects are themselves valid segments (variant A) at the price of weaker dedup.

## 1. Prior art

### 1.1 What each system acknowledges on, and whether it pre-uploads

| System | Sync point (what an fsync/commit waits for) | Bulk upload to object store | Pre-upload ahead of the sync point? |
|---|---|---|---|
| LSVD (EuroSys '22) | Commit barrier goes to the local cache SSD; "when it completes, all preceding LSVD writes will be durable" (paper §3.2) | Writes are batched (8 or 32 MB) into sequence-numbered objects and PUT in the background | Yes, but as lazy batching, not for the sync point. The sync point never depends on S3. |
| Neon | Transaction commit waits for a quorum of safekeepers (Paxos) to acknowledge WAL; no wait for pages | Pageserver materialises and uploads layers asynchronously | Not applicable. Durability is a replicated disk sink. |
| Ceph RBD journaling | Each write is recorded in the journal before the image; a mirror replays the journal | Mirror is asynchronous | No. The journal is a second cluster write: "expect write latencies to nearly double". |
| AWS EBS | Replicated volume; snapshots go to S3 asynchronously | Snapshots only | Not applicable. UNVERIFIED in this session (EBS docs fetched carried no replication detail). |
| SlateDB | `await_durable` waits until the WAL batch or SST is on the object store; the write call itself returns after the in-memory WAL | WAL buffer flushes when full or at a flush interval | Flush interval batching only. Group commit, not staging. |
| ZeroFS | NBD FLUSH/FUA reply only after data is durable; 9P fsync returns after stable storage | Extents packed into segments up to 256 MiB; unflushed file frames are buffered | Not documented. UNVERIFIED whether data is uploaded before FLUSH. |
| JuiceFS (default) | close/fsync flushes pending data and then commits metadata ("upload first, then commit") | Full blocks upload as a chunk fills, before close | **Yes.** Blocks are pre-uploaded; the commit is a metadata transaction. |
| JuiceFS (`--writeback`) | Commit first, upload later from a local staging directory | Background | Opposite model: the local disk is the sync point. |

Sources:
- LSVD paper: https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf (read §3.1, §3.2, §3.3, §4.6 and footnote 8).
- Neon: https://neon.com/docs/introduction/architecture-overview
- Ceph RBD: https://docs.ceph.com/en/latest/rbd/rbd-mirroring/
- SlateDB: https://slatedb.io/docs/design/writes/ ; the 100 ms default flush interval seen in a docs.rs test config is secondary: https://docs.rs/crate/slatedb/0.12.1/source/tests/db.rs
- ZeroFS: https://github.com/Barre/ZeroFS
- JuiceFS: https://juicefs.com/docs/community/guide/cache/

### 1.2 Observations that matter for Lunix

- **LSVD's design choice is to avoid the problem rather than solve it.** Its sync point is the local SSD, so a fsync never queues behind S3 at all. Lunix's `flush = s3` S3-only topology has no such sink (ADR-26: the journal is a RAM sink, `lunix-keeper` holds it across daemon restarts, not across host loss). Lunix with `flush = local` already gets the LSVD behaviour; the question here is only the `s3` setting.
- **LSVD recovery rule is relevant to staging.** Backend objects are named by sequence number. On recovery the daemon accepts the consecutive prefix and deletes "stranded" objects past a gap (paper §3.3). Lunix's commit chain plays the same role: a staged object not referenced by any commit is exactly a stranded object, and recovery or GC may delete it.
- **LSVD does not coalesce across batches** (footnote 8: it "would break the LSVD consistency guarantees"). Under a prefix-consistency contract, staging a write and then later overwriting it means both versions are on S3; the later cut simply wins by `seq`. This confirms that staging gives up cross-batch overwrite absorption (see §5.1).
- **JuiceFS shows the cost of the approach.** It pre-uploads, and the docs for `--writeback` warn about metrics for staged blocks and about leaks if staging files are deleted. Whether JuiceFS reclaims orphan blocks from crashed clients through `juicefs gc` is UNVERIFIED here.
- **SlateDB and ZeroFS (S3 flush) both accept PUT-latency fsync.** SlateDB documents `await_durable` as optional per write, which is the same lever as Lunix's `write_ack`/`flush` split. Neither is documented to stage bulk data ahead of the sync point.
- **S2 / s2-lite** (a third-party description relayed by a search result, not verified) says data is on object storage before acknowledgement and the flush interval defaults to 50 ms for a remote bucket. UNVERIFIED.

## 2. S3 mechanics for staging

### 2.1 Comparison of mechanisms

| Mechanism | What is uploaded ahead | What FLUSH does on S3 | Notes |
|---|---|---|---|
| **Plain PUT of staged objects** (A) | Whole objects, each sealed and durable on completion | 1 PUT of the small remainder (or inline in the commit) + 1 create-only commit PUT that names staged objects | Works on every backend. Objects are immutable, so retries and zombies are harmless. A commit must come after the objects it names (ADR-9), so the two PUTs are sequential unless variant B is adopted. |
| **Multipart parts uploaded ahead, Complete at FLUSH** (C) | Parts of one future object | Last UploadPart (any size) + CompleteMultipartUpload; with `If-None-Match: *` the Complete creates `commits/<seq>` | 5 MiB minimum part except the last; at most 10,000 parts; Complete cost and latency unpublished. Key must be fixed at Create time. |
| **S3 Express append** (D) | Appended bytes in one directory-bucket object | 1 append PUT (single-digit ms) | AWS directory buckets only (documented); 10,000 appends per object; no S3 Lifecycle on directory buckets. |
| Conditional writes | Not a staging mechanism; the fencing primitive for the commit | `If-None-Match: *` on the commit PUT or on Complete | See the fact-check §A for the provider matrix. |

### 2.2 Multipart upload facts (AWS S3)

Primary source: https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html , https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html , https://docs.aws.amazon.com/AmazonS3/latest/API/API_CompleteMultipartUpload.html

| Item | Value |
|---|---|
| Part size | 5 MiB to 5 GiB; no minimum for the last part |
| Parts per upload | 1 to 10,000 (part numbers need not be consecutive, except with checksums, which require consecutive numbers starting at 1) |
| Maximum object size | 48.8 TiB |
| Upload lifetime | No expiry; must be completed or aborted explicitly |
| Billing while pending | Parts are billed as storage, bandwidth and requests until the upload is completed or aborted |
| Complete semantics | S3 concatenates parts in part-number order. The docs say processing "could take several minutes" for large objects. The server sends `200 OK` early and then whitespace; an error can be embedded in a `200` body, so the client must parse the body. |
| Conditional Complete | `If-None-Match: *` and `If-Match` are accepted. On `409 ConditionalRequestConflict` the whole upload must be re-initiated and re-uploaded. |
| Part overwrite | Re-uploading a part number replaces the part. |
| Listing | `ListParts` returns at most 1,000 parts and does not include parts still uploading. The docs say to keep your own part and ETag list for Complete. |

- **Complete latency for a few small parts is unpublished.** AWS gives no number; the only statement is the worst case ("several minutes"). Treat it as UNVERIFIED and measure. Complete does not copy bytes on the client path, but S3's server-side assembly time against the parts' total size is not documented.
- **The 5 MiB minimum constrains the flush.** Parts uploaded ahead must be at least 5 MiB, which is the same as a staged object of at least 5 MiB. That is the regime in which the in-flight tail (§3.3) is worst. Only the last part (the flush remainder plus commit record) may be small.
- **Cloudflare R2:** minimum 5 MiB, maximum 10,000 parts, every part except the last must be the same size, incomplete uploads are aborted after 7 days by default, configurable by lifecycle policy. Source: https://developers.cloudflare.com/r2/objects/multipart-objects/ . The same-size rule would force a fixed part size and padding for the final part sequence (UNVERIFIED how R2 treats a short last part when earlier parts differ).
- **Backblaze B2 large files:** 5 MB minimum part (except the last), 5 GB maximum, at least two parts per large file; B2 does not support conditional PUT (fact-check), so it cannot arbitrate commits. Source: https://www.backblaze.com/docs/cloud-storage-large-files
- **Express directory buckets:** the same 5 MiB to 5 GiB part rule; incomplete uploads have no expiry; "Directory buckets don't support S3 Lifecycle configurations" (https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-express-using-multipart-upload.html). Abandoned uploads must therefore be aborted by Lunix itself.
- **GCS** has no S3-style multipart in its native API. Compose merges 1 to 32 objects per request, leaves sources in place, and accepts a generation precondition on the destination (https://docs.cloud.google.com/storage/docs/composing-objects). Compose could stitch staged objects into one commit-sized object, but that is an extra GCS-specific request on the flush path. Class A pricing for Compose was not on the page (UNVERIFIED).
- **Request pricing of multipart operations** was not stated on the AWS pricing page extract. AWS says only that the requests of a multipart upload are billed over its lifetime. The usual reading is that UploadPart, Create and Complete are billed at PUT/POST rates; UNVERIFIED in this session. The model below assumes PUT rates.

### 2.3 S3 Express One Zone append

Source: https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html

- Append is a `PutObject` with `x-amz-write-offset-bytes` set to the current object size. Directory buckets in the S3 Express One Zone storage class only.
- At most 5 GB per append and **10,000 appends per object**; an object created by multipart upload counts its parts toward the 10,000. Exceeding it returns `TooManyParts`; `CopyObject` resets the count.
- Each append is billed as a PUT request.
- No minimum append size, so a commit-sized append is allowed.
- Requests need `CreateSession` authorisation. The docs page does not describe the failure mode for a wrong offset or a concurrent appender (UNVERIFIED; it is the natural fencing mechanism but must be confirmed). It also does not say whether conditional headers combine with append.
- **Which providers support append.** AWS directory buckets: documented. MinIO: documented as part of its S3 Express mode (https://docs.min.io/aistor/reference/aistor-server/s3-express/ , search result only). Ceph RGW: has its own append operation with a Ceph-specific header (`x-rgw-next-append-position`), not the AWS header (Quincy docs, https://docs.ceph.com/en/quincy/radosgw/s3/objectops/ ). R2, Tigris, OVH, Scaleway, GCS, B2, Wasabi, Hetzner, DigitalOcean: UNVERIFIED (no documentation found). Azure Blob has a native Append Blob with conditional headers (fact-check §A), but that is not the S3 API.
- **Consequence.** Append is an AWS-and-MinIO accelerator, which matches ADR-10's rejection of "an S3-Express-only sync tier" as the only path.

### 2.4 Garbage collection of staged objects and abandoned multipart uploads

| Leftover | How it arises | Cleanup |
|---|---|---|
| Staged object never referenced by a commit | Crash before the commit, overwritten data discarded, fenced zombie | Lunix GC: treat unreferenced objects under the staging key space as orphans, apply the existing condemn, grace, re-check, DELETE sequence (DESIGN §11), fenced on the lease epoch. That GC mark phase walks retained commits, so unreferenced staged segments fall out of the mark set; the orphan-sweep itself is an inference from §11, not stated there. |
| Staged object referenced by a commit that never became durable (variant B) | Crash between the PUTs | Same sweep, plus a rule that a commit naming a missing object is invalid and is never a chain tip. |
| Incomplete multipart upload | Crash or fencing between Create and Complete | Bucket lifecycle `AbortIncompleteMultipartUpload` (`DaysAfterInitiation`, prefix filter; the docs example uses 7 days; the minimum is believed to be 1 day, UNVERIFIED). Explicit `AbortMultipartUpload` by the daemon on recovery. Directory buckets: no lifecycle, so explicit listing and abort only. |
| Object-level lifecycle `Expiration` | Not usable safely | Staged objects that a commit later references cannot be renamed on S3. A prefix-based expiry would delete committed data, so GC by reachability is required. A tag-based filter would need `x-amz-tagging` on every PUT and a tag removal request when promoted (UNVERIFIED cost and support across providers). |

- **Billing of leaks.** Pending multipart parts cost storage until aborted (AWS). R2 aborts at 7 days by default. B2 behaviour for unfinished large files is not documented on the page fetched (UNVERIFIED).
- **GC delete requests** are free on AWS S3 per the pricing page extract ("DELETE and CANCEL requests are free"). Delete pricing on R2, GCS and the others was not checked.

## 3. Latency and cost data

### 3.1 PUT latency by object size

| Source | Object | Result | Quality |
|---|---|---|---|
| Fact-check, topicpartition.io benchmark | S3 Standard, same region, 500 KB, 100 runs | PUT p50 69.75 ms, p95 101 ms, p99 137 ms | Secondary, one run |
| Same blog family | 128 KB PUT | about 53 ms (average only) | Secondary |
| AWS guidance | small objects | "roughly 100-200 ms" consistent latency; "tens of ms" median for under 512 KB | Primary, vague |
| Turso / AWS blog (Express) | 4 KB, same AZ, 1,000 ops | PUT avg 6.4 ms, p99 7 ms; GET avg 3.8 ms, p99 4 ms | Primary-adjacent |
| IDrive e2 vendor benchmark | AWS, one thread | 256 KiB at 4.03 MiB/s; 5 MiB at 34.94 MiB/s | Vendor-run, not independent |
| dev.to comparison | 1 MB objects | Express p99 12 ms, R2 p99 47 ms | Secondary, unclear method |

Sources: https://topicpartition.io/misc/AWS-S3-PUT-latency-benchmark ; https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html ; https://aws.amazon.com/blogs/storage/how-turso-built-a-transactional-database-using-amazon-s3-express-one-zone/ ; https://dl.idrive.com/s3-storage-e2/e2-performance-stats ; https://dev.to/johalputt/benchmark-aws-s3-express-one-zone-vs-cloudflare-r2-for-high-throughput-object-storage-3k9d

**No source found gives 8 or 16 MiB PUT latency directly.** A linear model fitted to the two vendor points (256 KiB at 62 ms, 5 MiB at 143 ms) gives a base of about 58 ms plus about 17 ms per MiB (about 59 MiB/s per stream):

| Object size | Modelled single-stream PUT time (Standard) | Status |
|---|---|---|
| 4 KiB | about 58 ms (the 500 KB p50 measurement is 70 ms) | estimate |
| 1 MiB | about 75 ms | estimate |
| 2 MiB | about 92 ms | estimate |
| 4 MiB | about 126 ms | estimate |
| 8 MiB | about 194 ms | estimate |
| 16 MiB | about 331 ms | estimate |

- The S3 documentation recommends horizontal scaling over many connections and byte-range parallelism for GET, with aggressive timeouts and retries for latency-sensitive work (https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-guidelines.html). Splitting a 16 MiB staged object into parallel multipart parts could cut the 331 ms single-stream time, at the price of Create and Complete round trips. Not measured.
- S3 request-rate guidance is "at least 3,500 PUT/COPY/POST/DELETE per second per prefix" (fact-check), so even 190 PUT/s fits under one prefix.

### 3.2 Per-request prices (us-east-1 list prices from the fact-check, fetched 2026-11-11)

| Provider | PUT (write) per 1,000 | Other charges relevant here | Conditional commit support |
|---|---|---|---|
| AWS S3 Standard | $0.005 | GET $0.0004 per 1,000; storage $0.023/GB-month | Yes |
| AWS S3 Express One Zone | $0.00113 | **$0.0032/GB upload and $0.0006/GB retrieval on all bytes**; GET $0.00003 per 1,000; storage $0.11/GB-month | Yes |
| Cloudflare R2 Standard | $0.0045 (Class A $4.50 per million) | Egress free | Yes (PUT, Complete) |
| OVHcloud Object Storage | $0 ("no charges for API calls") | Per-GB storage price not extracted | Yes (PUT, Complete) |
| Google Cloud Storage Standard | $0.005 (Class A) | Needs `x-goog-if-generation-match` rather than S3 headers | Different header |
| Backblaze B2 | $0 (Class A free on pay-as-you-go) | Storage from $6.95/TB-month; egress free up to 3x stored | **No** (mirror only) |

### 3.3 Why latency, not only request count, decides the design

A FLUSH with cut `c` needs every write `<= c` durable. With staging, those writes sit in three places: (1) already in completed staged objects, (2) in staged objects in flight, (3) in the unstaged tail. The flush waits for (2) to finish, uploads (3), and then writes the commit. Under ADR-9 ("every object a commit names is durable before the commit is created") the commit PUT is sequential after those uploads.

Modelled critical path per flush, 200 MB/s bulk stream, single-stream PUT times from §3.1, commit PUT = about 58-70 ms:

| Staged object size | Fill time at 200 MB/s | In-flight remaining at flush (newest object, uniform over its fill time) | + commit PUT | Modelled FLUSH latency | Staged PUT/s |
|---|---|---|---|---|---|
| 16 MiB | 84 ms | 247-331 ms | about 65 ms | **about 310-400 ms** | 11.9 |
| 8 MiB | 42 ms | 152-194 ms | about 65 ms | about 215-260 ms | 23.8 |
| 4 MiB | 21 ms | 105-126 ms | about 65 ms | about 170-190 ms | 47.7 |
| 2 MiB | 10.5 ms | 82-92 ms | about 65 ms | about 145-160 ms | 95.4 |
| 1 MiB | 5.2 ms | 70-75 ms | about 65 ms | about 135-140 ms | 190.7 |
| Floor (no bulk in flight) | - | 0 | about 65 ms | about 65-70 ms | - |

Assumptions: staged objects are dispatched as soon as they fill; PUTs are not queued behind each other; the link has headroom; the newest in-flight object contains writes before the cut. The estimate omits waiting on older in-flight objects, which finish earlier. If several objects are in flight (the large objects take 2-4 fill times), the flush waits for the oldest-completion bound, which is still the newest object.

For comparison, the **current design** (§8.3 and §5): one DELTA group PUT in flight per volume; a group over the 8 MiB DELTA cap uploads segments first and then the commit (two sequential PUT stages). Assume 140 ms for the two sequential small stages. If the effective upload rate is `B` MB/s and the bulk rate is 200 MB/s, a group's duration is `T = 140 ms / (1 - 200/B)`, and the fsync waits for the current group and then its own, about `1.5 T`:

| Effective upload bandwidth `B` | Group time `T` | Groups/s | Modelled fsync latency (1.5 T) |
|---|---|---|---|
| 1,000 MB/s | 175 ms | 5.7 | about 260 ms |
| 400 MB/s | 280 ms | 3.6 | about 420 ms |
| 250 MB/s | 700 ms | 1.4 | about 1.05 s |

Findings from the two tables:
- Staging with 8-16 MiB objects does not clearly beat the current design at a roomy link, and it adds cost. Its benefit is that latency no longer depends on how close the link is to saturation, because the staging lane has its own admission control (§4).
- The latency lever is **staged object size** (or equivalently the lag of the staging pipeline), not the commit size. Small staged objects (1-2 MiB) are required to approach the 70 ms floor.
- With a pipelined commit that is issued before the staged objects complete (variant B), the sequential commit term (about 65 ms) disappears, and latency approaches the in-flight remainder alone.
- Express One Zone shortens every term (4 KB PUT about 6-7 ms; 1 MB p99 reported at 12 ms), so staging on Express makes the in-flight term small even with 8 MiB objects (8 MiB Express latency not measured, UNVERIFIED).

### 3.4 Request and dollar estimate for the stated workload

Workload: bulk writer 200 MB/s sustained (not deduplicated or compressed, worst case) and a small-file writer at 50 fsync/s with negligible bytes (under 1 MB/s). One month = 30 days = 2,592,000 s. Bulk volume = 518.4 TB per month.

Model parameters (all assumptions):
- (a) Today: segments of 16 MiB (DESIGN §6 target); groups/s from the 1,000 MB/s row above (5.7 groups/s, one commit PUT each). A group of about 35 MB is over the 8 MiB DELTA cap, so it goes as segments plus a commit.
- (b) Staging: staged objects of 8 or 16 MiB; the small writer's 50 fsync/s are coalesced into 12 commit groups/s (one commit in flight at about 70-80 ms). The tail is inlined in the commit, so there is no extra object per flush. Staged objects become the segments referenced by the commit, so there is no second upload of the same bytes in this variant.
- Sensitivity: 50 commits/s (pipelined commits, no coalescing).
- All requests are PUT-class. Multipart is not used. Free requests (DELETE) excluded. Bytes ingress is free on all listed providers (not verified for each).

| Scenario | PUT/s | PUT per month | AWS S3 Standard | GCS Standard | R2 Standard | AWS Express (request part only) | OVH, B2 |
|---|---|---|---|---|---|---|---|
| (a) Today, 16 MiB segments + 5.7 commits/s | 17.6 | 45.7 M | $229 | $229 | $206 | $52 | $0 |
| (b) Staged 16 MiB + 12 commits/s | 23.9 | 62.0 M | $310 | $310 | $279 | $70 | $0 |
| (b) Staged 8 MiB + 12 commits/s | 35.8 | 92.9 M | $465 | $465 | $418 | $105 | $0 |
| (b) Staged 4 MiB + 12 commits/s | 59.7 | 154.8 M | $774 | $774 | $697 | $175 | $0 |
| (b) Staged 2 MiB + 12 commits/s | 107.4 | 278.4 M | $1,392 | $1,392 | $1,253 | $315 | $0 |
| (b) Staged 1 MiB + 12 commits/s | 202.7 | 525.4 M | $2,627 | $2,627 | $2,364 | $594 | $0 |
| (b) Staged 16 MiB + 50 commits/s | 61.9 | 160.5 M | $802 | $802 | $722 | $181 | $0 |
| (b) Staged 8 MiB + 50 commits/s | 73.8 | 191.4 M | $957 | $957 | $861 | $216 | $0 |

Notes on the table:
- (a)'s 5.7 commits/s is much lower than (b)'s 12 because today's design is limited by the long group time. The extra commits in (b) are the price of low fsync latency: roughly 6 more small PUTs per second.
- **Express data-volume charge dominates its request charge.** $0.0032/GB on 518,400 GB is about $1,659/month (about $1,545 if GB means GiB), before request fees and before storage. Use Express for commits and small tail objects only, not for bulk staged data. Rates and the all-bytes rule are from the fact-check (AWS blog, April 2025 change).
- Storage of staged data is the same as for committed data unless it is garbage. Garbage is addressed in §5.
- 12 groups/s assumes a Standard commit PUT of about 70-80 ms. At Express latency (about 7 ms) a single in-flight commit could run at up to about 140 groups/s, so the small writer's 50 fsync/s need no coalescing at all (all values ASSUMED).
- Per-request prices for UploadPart, Create and Complete were not confirmed (§2.2). A multipart design adds at least Create and Complete per object, which, at one MPU per flush group, adds about 24 requests/s at 12 groups/s.
- Outbound request counts for (b) with Express staged and Standard destage (variant D) add a second upload of the same bytes on Standard: about 11.9 PUT/s at 16 MiB (about $229/month) on top of the Express charges.

## 4. Bandwidth fairness: protecting the flush-critical upload

What published systems do:

| Technique | Evidence |
|---|---|
| Priority classes in a shared rate limiter | RocksDB uses `IO_HIGH` for flush and `IO_LOW` for compaction; higher priority is "more likely" to be served first, controlled by a fairness parameter (default 10); `IO_USER` always first. Source: https://github.com/facebook/rocksdb/wiki/Rate-Limiter |
| Separate connection pools / horizontal connections | S3 guidance: spread requests over separate connections, no per-bucket connection limit (https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-guidelines.html). A dedicated flush pool keeps a warm connection that never queues behind a 16 MiB body. |
| Aggressive timeouts and hedged retries | Same AWS page: a slow first request is likely to be beaten by a retry that takes a different path. Lunix's design already budgets hedged reads (cache-consistency note §6); the same budget can apply to the commit PUT. |
| HTTP/2 stream priorities | **Not available to rely on.** No official AWS statement found; secondary sources say S3 accepts only HTTP/1.1 (UNVERIFIED), so one body per connection and no multiplexed priorities. |
| Kernel-level shaping (`fq`, `tc`, `SO_PRIORITY`, socket buffer limits) | Standard Linux practice; no S3-specific source. UNVERIFIED for effectiveness over TLS with several connections to one endpoint. |
| Write-back throttling by dirty bytes | LSVD's `backlog` parameter bounds queued backend writes before blocking (asch/dis repository, search result only; UNVERIFIED in detail). Lunix already has `B_max` backpressure (ADR-10). |

Implications for Lunix, as constraints rather than a choice:
- The flush lane needs a connection that is idle when the FLUSH arrives. That means a small dedicated pool (two or three connections) and **no body larger than the flush budget on it**.
- The staging lane needs an in-flight byte cap, because every byte in flight when a flush arrives is a byte the flush waits for (§3.3). The cap is the real tuning knob: lag budget `L` ms means at most `200 MB/s x L` bytes in flight.
- Rate limiting the staging lane below the link rate is what keeps the flush lane from queueing behind it on shared NIC and path buffers. A token bucket at about 80% of measured goodput is a plausible starting point (no source; to be tested).
- If staging cannot keep up with the writer (bulk rate above link rate), the volume must apply backpressure to writes regardless of design; staging only moves where the queue sits.

## 5. Risks

### 5.1 Wasted uploads and loss of overwrite absorption

- DESIGN §5 destage prefers chunks "whose newest record is older than the async interval, so overwrites are absorbed". Staging uploads each write soon after it happens, so a block overwritten within the window is uploaded twice (once in a staged object, once more later) and the older copy becomes garbage.
- LSVD gives a measured magnitude, at a 32 MB batch window on CloudPhysics week-long traces: on traces w66 and w41 "a majority of bytes are overwritten while batching and never sent to the backend", with write amplification falling from 1.97 to 1.35 and from 1.44 to 1.14. Write amplification is under 1.5x for almost all traces. Most workloads gain little from coalescing; a few gain a lot. Source: LSVD paper §4.6 (https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf). Lunix's window is `T` = 30 s in async mode, much wider than 32 MB at 200 MB/s (160 ms), so the loss compared with async destage is larger than the paper's.
- Mitigations (no source; design ideas): stage only data older than a minimum age `A` (for example 100-500 ms) and let the flush upload the younger tail inline; stage only sequential or large writes (a heuristic that classifies the bulk stream, leaving small random writes for the flush); stage only when the dirty backlog exceeds a threshold. Each raises the unstaged tail the flush must upload, so it trades directly against §3.3.
- Garbage created by staging increases GC and compaction work, which scales with write volume (DESIGN §11 compacts segments under 50% live).

### 5.2 Crash leftovers

- A crash leaves staged objects with no referencing commit. They are harmless to correctness, because a commit names every object it needs and a mount follows commits, not listings (ADR-9). They cost storage until GC removes them (§2.4).
- For variant B (commit issued in parallel with staged objects), a crash can leave a commit that names missing objects. Rules needed: mount treats the chain as ending before such a commit; the FLUSH was never acknowledged, so the contract is intact. A later commit could not be created above it without the same missing-object check, which must be added to the "find the tip by LIST plus probing forward" procedure (ADR-9).
- A zombie owner (fenced) can keep uploading staged objects after losing the lease. Segment numbers carry the epoch (`seg = (epoch << 40) | counter`, DESIGN §6), so these never collide with the new owner's objects, and GC can attribute them. The zombie cannot publish a commit (its slot is taken).
- Failed or retried PUTs can leave duplicate objects with different keys unless keys are deterministic per staged batch.

### 5.3 Deduplication and compression

- The staged object must be a sealed unit. If it is a valid segment (variant A), it has already gone through the destage stages of DESIGN §5 (zero scan, BLAKE3, dedup lookup, compress, encrypt), which are slower than raw copy (zstd-1 is "the bottleneck" at 0.4-2 GB/s per core, DESIGN §5), and dedup across staged objects is weaker because the open segment is smaller and each staged object is cut early.
- If instead staged objects are raw journal batches (compressed and encrypted as a unit but not deduplicated, like a DELTA commit body), destage later re-reads them, builds deduplicated segments and re-uploads. That doubles upload bytes and adds a GET path for destage (S3 GET prices $0.0004 per 1,000 on Standard). On an S3-only volume the data is in RAM at destage time only if the memory buffer still holds it (256 MiB default `mem_buffer`, DESIGN §13.1), which at 200 MB/s is about 1.3 s of writes, so a second pass would usually have to GET it back. This is the main architectural cost of "staged = raw journal batches".
- Cold RMW for partial chunks (DESIGN §13.1) happens at destage, not staging, and is unaffected for variant A only if the staged segment is formed from whole chunks; partial-chunk staging would require raw records.

### 5.4 Encryption

- ADR-11 keys data per extent from `chunk_id` plus a random salt, with AES-256-GCM, and keys live only in daemon memory (DESIGN §15). A staged object leaves the host and must be sealed with volume keys exactly like segments and DELTA bodies. Staged raw journal batches therefore cannot be written plaintext "just for staging".
- Key derivation for a raw journal batch has no `chunk_id`, so it needs its own derivation context (volume key plus object id plus counter). That is a new construction for the ADR-11 review; not designed here.
- The commit that names a staged object should include its authentication data (hash or MAC) so a swapped object is detected; the existing commit MAC (DESIGN §8.2 `mac`) is the natural place.

### 5.5 Other

- **Ordering across staged objects.** Staged objects must be assigned in `seq` order so that "all objects up to `c`" is a prefix, as LSVD's object numbering does. Out-of-order completion is fine because the commit waits for all of them.
- **Memory.** Staged-but-uncommitted bytes still live in the RAM buffer (and the keeper's memfd) until a commit names them; staging does not free memory, only upload bandwidth.
- **Mirrors (ADR-24).** Staged objects must also reach other backends before a commit that names them is published there; staging multiplies upload fan-out by the number of backends.
- **Request rate and CPU.** 190 PUT/s with TLS and signing is a visible CPU cost in the Rust client, and each PUT is a failure point needing retry and hedging.

## 6. Design options for Lunix

These are variants for a later reviewer; none is recommended here. All keep ADR-3 (prefix cuts) and the create-only commit chain (ADR-9). "Floor" is the single-small-PUT latency (about 70 ms on S3 Standard).

### Variant 0 (reference): no staging, scheduler fixes only

Keep DESIGN §8.3. Cap DELTA group size at the DELTA cap, issue segment uploads for a group in parallel with a pre-built commit when the group is small, and give the flush lane its own connection pool and a bounded in-flight budget.
- Latency: about 1.5 T of §3.3 and rises as the link saturates; no new GC load.
- Cost: unchanged ($229/month in the model).
- Does not remove entanglement, only caps it. Listed so the others are compared against a real baseline.

### Variant A: lag-bounded staging of sealed small segments

Background lane seals and uploads a staged object whenever `S` MiB of records have accumulated or `Lms` has passed (S in 1-4 MiB). Objects are ordinary segments (`segments/<seg>`), encrypted and compressed, referenced by the commit. A FLUSH waits for in-flight staged objects, uploads the unstaged tail inline in the DELTA, then writes the commit. In-flight bytes are capped at `200 MB/s x lag`.
- Latency: about 135-190 ms for S of 1-4 MiB (§3.3), dominated by in-flight staging and the sequential commit.
- Cost: $774-$2,627/month for S of 4 down to 1 MiB on Standard; $0 on OVH or B2; about 3x to 11x the baseline request bill.
- Pros: one mechanism for all providers; staged objects are final segments, so no second upload; GC is the existing orphan sweep.
- Cons: small segments weaken dedup and inflate GC and compaction work (segment target is 16 MiB, DESIGN §6); loses overwrite absorption (§5.1); a compaction pass is probably needed to merge small staged segments into 16 MiB ones, which re-uploads bytes (not costed).

### Variant B: staging plus parallel (pipelined) commit

As A, but with 8-16 MiB staged objects, and the commit is issued at the same time as the last staged PUTs and the tail. The FLUSH completes when all of them complete. A mounting reader treats a commit as valid only if every object it names exists, and recovery stops the chain at the first invalid commit.
- Latency: about max(in-flight remainder, tail PUT, commit PUT): roughly 150-330 ms with 8-16 MiB objects, so it removes the commit term but not the in-flight term; combine with 2-4 MiB objects to get about 90-130 ms.
- Cost: staged-object cost as in A; commit cost unchanged.
- Pros: removes one sequential round trip.
- Cons: **changes the ADR-9 invariant** ("every object a commit names is durable before the commit is created"); needs tip-finding rules and a fuzzing target for "commit names missing object"; hedged retries of staged PUTs now race with the commit; mirrors need the same rule.

### Variant C: multipart upload with CAS Complete as the commit

For each flush group, open one multipart upload on the future commit key `commits/<seq>`, upload the staged pieces as parts (each at least 5 MiB), and at FLUSH upload the tail plus the commit record as the final (small) part and send Complete with `If-None-Match: *`. The completed object is both data and commit.
- Latency: last UploadPart plus Complete; same two sequential round trips as A, plus Complete's unpublished latency (UNVERIFIED).
- Cost: staged parts count as PUTs; add Create and Complete per commit (+24 requests/s at 12 groups/s); the commit slot must be known when the upload starts, and a race lost at Complete (412 or 409) wastes the parts (re-upload required).
- Pros: no separate small staged objects; parts of 8-16 MiB cost the same per byte as variant A with larger objects; no orphan references (an unfinished upload is invisible until Complete).
- Cons: 5 MiB minimum part pins the in-flight term at the 8-16 MiB values of §3.3; Complete not supported on GCS (needs a different path), unconfirmed on MinIO; commits are large and carry data, so FULL commits and GC must treat them as data holders; abandoned uploads need `AbortIncompleteMultipartUpload` or explicit aborts (and Express has no lifecycle).

### Variant D: Express-One-Zone fast lane (AWS and compatible only)

Optional accelerator in line with ADR-10's `sync-fast-wal`. Staged objects and commits for sync are written to a directory bucket (single AZ), either as separate PUTs or as appends to a per-epoch log object; destage later rebuilds Standard segments. The authority (Standard bucket) stays the durable chain in the 'normal' sense, with Express as a write-through tier, or the Express log is itself the sync point and is folded asynchronously.
- Latency: 4 KB PUT about 6-7 ms; the in-flight term shrinks with every object size (8 MiB Express latency UNVERIFIED), so the FLUSH could approach 15-40 ms.
- Cost: $0.0032/GB on all uploaded bytes, about $1,660/month for the 200 MB/s stream, plus requests and Express storage ($0.11/GB-month) for the time staged data is retained, plus the Standard destage upload ($229/month at 16 MiB).
- Pros: lowest latency; append allows one tiny request per flush; 10,000-append limit is reachable in minutes at 12 flushes/s, so roll to a new object about every 14 minutes (`TooManyParts` otherwise).
- Cons: single-AZ durability for acknowledged writes (weaker than Standard, a ADR-26 consideration); not portable (AWS and MinIO's mode only); no lifecycle, so Lunix does all cleanup; fencing semantics of `x-amz-write-offset-bytes` under concurrent appenders must be verified (UNVERIFIED).

### Comparison at a glance

| Variant | Modelled FLUSH latency under 200 MB/s bulk | PUT/s (incl. 12 commits/s) | AWS Standard $/month | Portability | Touches ADR-9 invariant |
|---|---|---|---|---|---|
| 0 reference | about 260 ms, grows to about 1 s near saturation | 17.6 | $229 | all | no |
| A, 4 MiB | about 170-190 ms | 59.7 | $774 | all | no |
| A, 2 MiB | about 145-160 ms | 107 | $1,392 | all | no |
| A, 1 MiB | about 135-140 ms | 203 | $2,627 | all | no |
| B, 8 MiB | about 150-200 ms | 35.8 | $465 | all | **yes** |
| B, 2 MiB | about 90-100 ms | 107 | $1,392 | all | **yes** |
| C | about 215-400 ms (5 MiB+ parts) | 24-60 | $310-$465 plus Create/Complete | no GCS, MinIO unconfirmed | no (Complete is the create-only commit) |
| D | about 15-40 ms (estimate) | 24-36 on Express | about $1,660 + $229 + requests | AWS (and MinIO mode) | no |

All latencies are modelled from §3.3 and are not measured.

## 7. Open questions and measurements needed

1. Measured single-stream and parallel PUT latency at 1, 2, 4, 8, 16 MiB on the target regions (Standard, Express, R2, OVH), p50 and p99, with and without a concurrent 200 MB/s bulk stream sharing the NIC. This is the load-bearing number for §3.3.
2. CompleteMultipartUpload latency for 1-4 parts totalling under 20 MiB, on AWS, R2, OVH and MinIO. No published number was found.
3. Whether UploadPart, Create and Complete are billed at PUT rates on each provider (AWS pricing page extract did not map them).
4. Failure mode of an S3 Express append with a wrong `x-amz-write-offset-bytes`, and whether it can fence a zombie appender.
5. Whether R2, Tigris, OVH, Scaleway and GCS support append-style writes (no documentation found).
6. Whether S3 endpoints accept HTTP/2 (no official statement found), and how effective NIC-level shaping is for isolating the flush lane.
7. Whether ZeroFS or JuiceFS reclaim orphaned blocks after client crashes, and how ZeroFS orders data upload relative to an NBD FLUSH (docs fetched do not say).
8. Effective overwrite ratio of target workloads (databases, package builds, VM images) over a 100 ms-1 s window, to size the loss in §5.1.
9. Behaviour of the 10,000-append limit and `CopyObject` reset for a long-lived Express log (documented, untested).
10. Cost of the needed compaction pass in variant A when small staged segments must be merged into 16 MiB segments (not modelled).

## Sources

- Lunix: `/home/bra/cvs/replicix/lunix/docs/explanation/DECISIONS.md` (ADR-3, ADR-9, ADR-10, ADR-26, ADR-27); `/home/bra/cvs/replicix/lunix/docs/explanation/DESIGN.md` (§4.1, §5, §6, §8.1-8.3, §11, §13.1, §15); `research/2026-11-11-fact-check.md`; `research/2026-11-11-prior-art.md`.
- AWS: https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html ; .../mpuoverview.html ; .../mpu-abort-incomplete-mpu-lifecycle-config.html ; .../API/API_CompleteMultipartUpload.html ; .../directory-buckets-objects-append.html ; .../s3-express-using-multipart-upload.html ; .../optimizing-performance-guidelines.html ; https://aws.amazon.com/s3/pricing/ ; https://aws.amazon.com/blogs/storage/how-turso-built-a-transactional-database-using-amazon-s3-express-one-zone/
- Cloudflare R2: https://developers.cloudflare.com/r2/objects/multipart-objects/
- Backblaze: https://www.backblaze.com/docs/cloud-storage-large-files
- GCS Compose: https://docs.cloud.google.com/storage/docs/composing-objects
- MinIO S3 Express mode: https://docs.min.io/aistor/reference/aistor-server/s3-express/ ; Ceph RGW object operations: https://docs.ceph.com/en/quincy/radosgw/s3/objectops/
- LSVD paper: https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf
- Neon: https://neon.com/docs/introduction/architecture-overview ; Ceph RBD mirroring: https://docs.ceph.com/en/latest/rbd/rbd-mirroring/
- SlateDB: https://slatedb.io/docs/design/writes/ ; ZeroFS: https://github.com/Barre/ZeroFS ; JuiceFS cache: https://juicefs.com/docs/community/guide/cache/
- RocksDB rate limiter: https://github.com/facebook/rocksdb/wiki/Rate-Limiter
- Latency: https://topicpartition.io/misc/AWS-S3-PUT-latency-benchmark ; https://dl.idrive.com/s3-storage-e2/e2-performance-stats ; https://dev.to/johalputt/benchmark-aws-s3-express-one-zone-vs-cloudflare-r2-for-high-throughput-object-storage-3k9d
