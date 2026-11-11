# Fact-check of the Lunix research notes (live primary sources)

Date: 2026-11-11
Author: research agent (Sonnet)
Question: Several of the seven 2026-11-11 research notes were written partly from memory and flag items UNVERIFIED. This pass re-checks the load-bearing claims (S3 conditional-write support per provider, list prices, S3 latency, S3 request-rate limits, ublk flag history, the LSVD paper numbers, ZeroFS) against live primary sources fetched on 2026-11-11: vendor docs, price lists, kernel source at tagged releases, crates.io, the paper PDF. Where only secondary evidence (GitHub issues, blog benchmarks) exists, the row says so. Where a source could not be reached, the row says UNRESOLVED.

## Summary

- **Backblaze B2 does NOT support conditional PutObject.** Backblaze's own docs are silent, but a ClickHouse issue (opened 2026-10-22, B2 EU) and a live probe report `501 NotImplemented` for `If-None-Match: *`. Proxmox ships a skip-header quirk. Treat B2 as a non-CAS backend (mirror only). This upgrades "UNVERIFIED" in `cache-consistency` §6.3 and `novel-ideas` to a firm "no" (secondary evidence).
- **Conditional-write matrix, hard facts**: AWS S3 supports `If-None-Match: *` and `If-Match` on PutObject, CompleteMultipartUpload and CopyObject (CopyObject since 2025-10-29), in general purpose AND directory (Express One Zone) buckets. R2, MinIO, OVH, Tigris, Scaleway and Azure support PutObject CAS. R2 and OVH also support it on CompleteMultipartUpload; MinIO's compat tables do not list CompleteMultipartUpload (only CreateMultipartUpload), so MinIO multipart CAS needs a probe. **GCS has no S3-style conditional PUT**: its XML API documents only `x-goog-if-generation-match` (0 = create-only) for writes, and `If-None-Match`/`If-Match` only for GET/HEAD. Lunix needs a GCS-specific code path (or the JSON API).
- **Ceph RGW**: conditional PUT dates back to 2014 (tracker #8562), but it is still being fixed in 2026: versioned buckets are not atomic (tracker 80925, two `If-None-Match: *` PUTs can both succeed), suspended-versioning bug 80325 is being backported to Tentacle/Squid, and the loser of two racing `If-Match` writes can get a 500. CopyObject destination conditions are still an open PR (#71588). Rule: run the probe, never trust the header on RGW versioned buckets.
- **DigitalOcean Spaces is unreliable for CAS** (reports of `If-Match` ignored/quoted-ETag quirks, no DO statement), **Wasabi honored `If-None-Match: *`** in one third-party test (2026-10-08), **Hetzner** lists "Conditional PUT/DELETE operations on versioned Buckets" as an exception (unversioned buckets: undocumented). Probe all three.
- **Prices, AWS, us-east-1, price list published 2026-11-09**: S3 Standard PUT $0.005/1000, GET $0.0004/1000, storage $0.023/GB-mo; Express One Zone PUT $0.00113/1000, GET $0.00003/1000, storage $0.11/GB-mo, plus $0.0032/GB upload and $0.0006/GB retrieval charged on ALL bytes (the 512 KB threshold was removed in April 2025). The note's "~8x Standard per GB" is wrong: it is 4.8x ($0.11 vs $0.023). Inter-region from us-east-1 is $0.01/GB to us-east-2 and $0.02/GB to Ireland/Tokyo (the notes' flat $0.02 is only correct for most pairs). Internet egress $0.09/GB first 10 TB after 100 GB/month free.
- **ublk "Since" column in `ublk-io-uring-rust` has several wrong kernel versions** (table in section E): UNPRIVILEGED_DEV is 6.3 (not ~6.5), CMD_IOCTL_ENCODE 6.4 (not ~6.6), USER_COPY 6.5 (not ~6.7), USER_RECOVERY_FAIL_IO 6.13 (not ~6.9), UPDATE_SIZE 6.16 (not ~6.15), BUF_REG_OFF_DAEMON 6.17, BATCH_IO 7.0 (not ~6.19), SHMEM_ZC 7.1, IO_DESC_SIZE 7.3. `UBLK_ATTR_VOLATILE_CACHE` (bit 2) and `UBLK_ATTR_FUA` (bit 3) exist and map to `BLK_FEAT_WRITE_CACHE` / `BLK_FEAT_FUA` as the note says.
- **libublk**: crates.io latest = 0.4.8 (2026-08-29). 0.5.0 is only on `main` (Cargo.toml says 0.5.0; no `v0.5.x` git tag; no crates.io release). Pin 0.4.8.
- **S3 request limits (3,500 / 5,500 per prefix per second) are still current**, worded "at least" and "gradually scaling"; AWS now also says S3 Express can do "hundreds of thousands" of requests/s.
- **S3 latency**: AWS only publishes "roughly 100-200 ms" for small-object latency (and "tens of ms median" for requests < 512 KB). The note's "p50 15-40 ms" for Standard small PUT has no primary source; the one measured Standard data point (500 KB, same region, 100 runs) is PUT p50 70 ms / p99 137 ms, GET p50 26 ms / p99 86 ms. Express: Turso/AWS blog 4 KB same-AZ PUT avg 6.4 ms, p99 7 ms; GET avg 3.8 ms, p99 4 ms (no p50 published). The note's Express "p99 ~15-30 ms" is too pessimistic for the best case.
- **LSVD paper (EuroSys '22)**: now have real numbers (section F). Key: 24x fewer backend I/Os per 16 KB random write than RBD (0.25 vs 6), 50,000 client IOPS at under 10% backend load vs RBD 12,000 IOPS, 11x faster than bcache+RBD sustained, GC threshold 70% greedy, 8 or 32 MB objects, read-cache blocks 128 KB. `prior-art`'s "~100K IOPS on Ceph 17.2.0" is from the lsvd-rbd README, not the paper (paper used Ceph Octopus 15.2.3).
- **ZeroFS**: dual-licensed **AGPL-3.0 and a commercial license**; NBD device sizes are fixed at creation (README: "to resize, disconnect, delete, and recreate"), and the README never mentions dedup. Latest release v2.3.6 (2026-10-27). `prior-art` is right.

## A. Conditional writes per provider

Legend: PUT = PutObject, CMU = CompleteMultipartUpload, COPY = CopyObject. "secondary" = GitHub issue/blog, not vendor docs.

| Claim | Verdict | Value | Source |
|---|---|---|---|
| AWS S3 general purpose: PUT `If-None-Match: *` and `If-Match: <etag>` | CONFIRMED | Both headers on PUT, CMU and COPY. `If-None-Match` takes only `*`. 412 on mismatch, 409 `ConditionalRequestConflict` on concurrent conflicting write (retry; for CMU re-initiate whole MPU). `If-Match` needs s3:PutObject+s3:GetObject. If-Match on missing/delete-marker object = 404. SigV4 required. No extra charge, failed requests still billed. | https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html ; https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html ; https://docs.aws.amazon.com/AmazonS3/latest/API/API_CompleteMultipartUpload.html |
| AWS dates: If-None-Match Aug 2024, If-Match Nov 2024, COPY Oct 2025 | CONFIRMED (Nov, Oct); Aug only by URL path | If-Match on PUT/CMU announced 2024-11-25; conditional COPY announced 2025-10-29. The Aug 2024 announcement URL is `/2024/08/amazon-s3-conditional-writes` (page not opened). | https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes ; https://aws.amazon.com/about-aws/whats-new/2025/10/amazon-s3-conditional-write-functionality-copy-operations |
| S3 Express One Zone (directory buckets) supports conditional PUT/CMU/COPY | CONFIRMED | Both Nov-2024 (PUT/CMU, `If-Match`) and Oct-2025 (COPY) announcements say "general purpose and directory buckets". Conditional DeleteObject/DeleteObjects also supported on directory buckets. The PutObject/CMU API pages carry no "not supported for directory buckets" caveat on the conditional headers (whereas Object Lock, SSE-C, versioning are marked unsupported). Bucket-policy enforcement keys documented for general purpose only. Express ETags are random strings, not MD5. | https://aws.amazon.com/about-aws/whats-new/2025/10/amazon-s3-conditional-write-functionality-copy-operations ; https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-requests.html ; https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-express-differences.html |
| Cloudflare R2 PUT and CMU `If-Match`/`If-None-Match` | CONFIRMED | PUT: If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since implemented. CMU: same four. Conditions sent on CreateMultipartUpload are evaluated at initiation only and NOT re-evaluated at completion: send them on CMU. | https://developers.cloudflare.com/r2/api/s3/api/ ; https://developers.cloudflare.com/r2/api/s3/extensions/ |
| R2 CopyObject conditional | CORRECTED: no plain `If-Match`/`If-None-Match` on COPY | Only source-side `x-amz-copy-source-if-*`; destination conditions use Cloudflare's own `cf-copy-destination-if-*` headers (beta, not atomic with source checks, not S3-portable). | https://developers.cloudflare.com/r2/api/s3/extensions/ |
| GCS, S3-interop XML API: `If-None-Match: *` / `If-Match` on PUT | CORRECTED: not documented | XML API `If-Match`/`If-None-Match` documented only for HEAD/GET Object. Writes use `x-goog-if-generation-match: 0` (create-only, "only if the object does not currently exist") or `<generation>` (CAS). Whether the interop endpoint honours AWS-style headers on PUT is not stated; the notes' wording "not per docs" is right. | https://docs.cloud.google.com/storage/docs/xml-api/reference-headers ; https://docs.cloud.google.com/storage/docs/request-preconditions |
| GCS native generation match | CONFIRMED | JSON API `ifGenerationMatch` (0 = must not exist), XML header `x-goog-if-generation-match`; mismatch = 412. | https://docs.cloud.google.com/storage/docs/request-preconditions |
| GCS: preconditions unsupported on XML multipart uploads (in `cache-consistency` §6.3) | UNRESOLVED | Not found in the pages fetched. | - |
| Azure Blob native ETag conditions | CONFIRMED | `If-Match`, `If-None-Match` (ETag or `*` = only if not exists) on Put Blob, Put Block List, Copy Blob (destination), Append Block, Delete etc.; unmet write condition = 412. Write ops accept only ONE conditional header (two = 400; exceptions: If-None-Match+If-Modified-Since, If-Match+If-Unmodified-Since). Put Block does not support conditions. | https://learn.microsoft.com/en-us/rest/api/storageservices/specifying-conditional-headers-for-blob-service-operations |
| MinIO PUT `If-None-Match`/`If-Match` | CONFIRMED (docs); release number UNRESOLVED | AIStor compat table: PUT, COPY, CreateMultipartUpload list both headers; CMU is NOT listed. MinIO blog says conditional writes were merged Feb 2023 (self-reported, not tied to a release tag). | https://docs.min.io/aistor/developers/s3-api-compatibility/ ; https://docs.min.io/community/minio-object-store/reference/s3-api-compatibility.html ; https://www.min.io/blog/leading-the-way-minios-conditional-write-feature-for-modern-data-workloads |
| Ceph RGW conditional PUT | CORRECTED: exists since 2014 but buggy | Tracker #8562 "Conditional PUT on ETag" resolved 2014-12-12 (merge commit 7925b82; no fixed-in release field). 2026 problems: tracker 80925 (versioned buckets not atomic, PR #72118), 80325 (suspended versioning, backport PR #72050 to Tentacle, Squid backport tracked), 80906 (loser of racing If-Match gets 500), CopyObject destination conditions still an open PR #71588 (501 for remote zonegroup/D4N/POSIX/DBStore). Squid v19.2.7 (2026-10-12) notes "fix conditional Delete, MultiDelete and Put" (pr#65932). Official `objectops` docs (dev version) still document no `If-Match` on PutObject. First-release number: UNRESOLVED. | https://tracker.ceph.com/issues/8562 ; https://github.com/ceph/ceph/pull/72118 ; https://github.com/ceph/ceph/pull/71588 ; https://github.com/ceph/ceph/pull/72050 ; https://docs.ceph.com/en/latest/releases/squid/ ; https://docs.ceph.com/en/latest/radosgw/s3/objectops/ |
| OVHcloud Object Storage (S3 Standard / High Perf) | CONFIRMED PUT+CMU; storage-class split UNRESOLVED | PUT and CMU: `If-Match` (bare ETag or `*`) and `If-None-Match` (only `*`); DeleteObject: `If-Match` only. Only for unencrypted or SSE-S3 objects (SSE-C not yet). Both headers together = 400. 409 `ConditionalRequestConflict` -> retry (restart MPU). The page does not distinguish Standard vs High Performance classes. An older Proxmox forum thread reported 501 on OVH; docs now say supported. | https://docs.ovhcloud.com/pl/guides/storage-and-backup/object-storage/s3-conditional-writes.md |
| Backblaze B2 S3 API | CORRECTED: NOT supported | B2 docs never mention the headers. ClickHouse issue #127483 (2026-10-22): `If-None-Match: *` PutObject returns `501 NotImplemented`. Proxmox PBS has a `skip-if-none-match-header` quirk for B2. (secondary) | https://github.com/ClickHouse/ClickHouse/issues/127483 ; https://www.backblaze.com/docs/cloud-storage-s3-compatible-api ; https://lists.proxmox.com/pipermail/pbs-devel/2025-July/014234.html |
| Wasabi | UNRESOLVED (positive secondary evidence) | No Wasabi doc page found. A third-party real-bucket test on 2026-10-08 (us-west-2) got `412 PreconditionFailed` for `If-None-Match: *` overwrite; If-Match and concurrent races not tested. | https://github.com/haileyok/engram-garden/pull/12 |
| Hetzner Object Storage | UNRESOLVED | Docs list "Conditional PUT/DELETE operations on versioned Buckets" among exceptions to S3 compatibility; headers not named; unversioned behaviour undocumented; COPY only within same bucket. | https://docs.hetzner.com/storage/object-storage/supported-actions/ |
| Scaleway | CONFIRMED (search excerpt) | Docs state `If-None-Match`/`If-Match` supported for PutObject, CopyObject, and CompleteMultipartUpload (example given); CreateMultipartUpload/UploadPart/UploadPartCopy do not accept them; bucket policies can enforce via `s3:if-none-match`/`s3:if-match`. Direct fetch of the page returned 403, so this rests on search-result excerpts of the page. | https://www.scaleway.com/en/docs/object-storage/api-cli/using-conditional-writes/ |
| DigitalOcean Spaces | UNRESOLVED (negative secondary evidence) | Compat page never mentions conditional headers. Reports: `If-Match` on PUT silently ignored/overwrites (denoland/celld #137, Aug 2026, hypothesis only), another reports unquoted-ETag requirement and 412 instead of 404; a SlateDB thread claims proper CAS only in some regions. No DO statement. Treat as non-CAS unless the probe passes. | https://github.com/denoland/celld/issues/137 ; https://docs.digitalocean.com/products/spaces/reference/s3-compatibility/ |
| Tigris | CONFIRMED | `If-Match` and `If-None-Match` (incl. `*`) on PUT; send `X-Tigris-Consistent: true` so the check runs on the leader; for multi-region writers use a single-region or multi-region bucket. | https://www.tigrisdata.com/docs/objects/conditionals/ |

## B. List prices

AWS figures are from the official price-list JSON (offer files `AmazonS3`, `AmazonS3Express`, `AWSDataTransfer`, us-east-1, publicationDate 2026-11-09 / 2026-11-10). Fetched 2026-11-11.

| Claim | Verdict | Value | Source |
|---|---|---|---|
| S3 Standard PUT/COPY/POST/LIST | CONFIRMED | $0.005 per 1,000 | https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonS3/current/us-east-1/index.json |
| S3 Standard GET and other | CONFIRMED | $0.0004 per 1,000 ($0.004 per 10,000) | same |
| S3 Standard storage | CONFIRMED | $0.023/GB-mo first 50 TB; $0.022 next 450 TB; $0.021 over 500 TB | same |
| S3 Express One Zone storage | CONFIRMED | $0.11/GB-mo (was $0.16 before 2025-04-10) | https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonS3/current/us-east-1/index.json ; https://aws.amazon.com/blogs/aws/up-to-85-price-reductions-for-amazon-s3-express-one-zone |
| Express PUT/COPY/POST/LIST | CONFIRMED | $0.00113 per 1,000 (4.4x cheaper than Standard) | same |
| Express GET | CONFIRMED | $0.00003 per 1,000 (13x cheaper than Standard) | same |
| Express per-GB request-size charges | CORRECTED: apply to all bytes | Upload $0.0032/GB, retrieval $0.0006/GB, "now apply to all bytes transferred rather than just portions of requests greater than 512 KB" (since 2025-04-10). Consequence: 4 KB write cost = $0.00113/1000 + $0.0032/GB ~ negligible, but 1 MiB PUT ~ $1.13e-6 + $3.1e-6 = ~$4.2 per million, so the Express request advantage vanishes for large segments. | https://aws.amazon.com/blogs/aws/up-to-85-price-reductions-for-amazon-s3-express-one-zone |
| Express storage "~8x Standard" (novel-ideas) | CORRECTED: 4.8x | $0.11 / $0.023 | as above |
| Internet egress AWS | CONFIRMED | $0.09/GB first 10 TB/mo, $0.085 next 40 TB, $0.07 next 100 TB, $0.05 over 150 TB; first 100 GB/mo free (aggregated, all services) | https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AWSDataTransfer/current/us-east-1/index.json ; https://aws.amazon.com/s3/pricing/ |
| Inter-region AWS from us-east-1 | CORRECTED: varies by pair | us-east-1 -> us-east-2 $0.01/GB; -> eu-west-1 $0.02/GB; -> ap-northeast-1 $0.02/GB | same |
| GCS Class A / Class B (single region, Standard) | CONFIRMED | $0.005 / $0.0004 per 1,000 (hierarchical-namespace buckets $0.0065 / $0.0005) | https://cloud.google.com/storage/pricing |
| GCS Standard storage us-east1 and GCP internet egress first tier | UNRESOLVED | Price tables are rendered client-side; WebFetch/curl could not extract the numbers; Cloud Billing Catalog needs an API key. | https://cloud.google.com/storage/pricing ; https://cloud.google.com/vpc/network-pricing |
| Azure internet egress first tier | CONFIRMED | First 100 GB/mo free; next 10 TB: $0.087/GB (Microsoft premium global network) or $0.08/GB (routing-preference transit ISP), Zone 1 (North America/Europe) | https://azure.microsoft.com/en-us/pricing/details/bandwidth/ |
| R2 Standard | CONFIRMED | Storage $0.015/GB-mo; Class A $4.50/M; Class B $0.36/M; egress free; free tier 10 GB, 1M A, 10M B per month | https://developers.cloudflare.com/r2/pricing/ |
| R2 Infrequent Access | (new) | $0.01/GB-mo, Class A $9.00/M, Class B $0.90/M, retrieval $0.01/GB, 30-day minimum | same |
| OVH Object Storage requests/egress | CONFIRMED (policy); storage price UNRESOLVED | Product page: "no charges for API calls, incoming traffic, or outgoing traffic". Standard (HDD) and High Performance (NVMe) are single-zone classes, 1-hour minimum billing, no retrieval fee. Per-GB storage price not extractable (prices page truncated/JS). | https://www.ovhcloud.com/en/public-cloud/object-storage/ |
| Backblaze B2 | CONFIRMED / CORRECTED | Storage from $6.95/TB/mo (30-day basis, byte-hour billing). Egress free up to 3x average monthly storage, then $0.01/GB; free to partner CDNs/compute. Class A/B/C calls free on pay-as-you-go; Class D (e.g. list/some admin) $0.004 per 10,000 after 2,500/day free. The note's "$0.40/M Class B" is outdated. | https://www.backblaze.com/cloud-storage/pricing |

## C. Latency

| Claim | Verdict | Value | Source |
|---|---|---|---|
| AWS-published S3 Standard small-object latency | CONFIRMED | "consistent small object latencies ... of roughly 100-200 milliseconds" (guidance page); a separate page says for requests under 512 KB "median latencies are often in the tens of milliseconds" and suggests retrying after 2 s. No percentile table is published. | https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html ; https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-design-patterns.html |
| S3 Standard measured, same region | measured (secondary, one run) | 500 KB, eu-north-1, EC2 client, 100 runs: PUT avg 73.8 / p50 69.75 / p95 101 / p99 137 ms; GET avg 28.1 / p50 26.13 / p95 38.9 / p99 86 ms. (The article's TL;DR swaps PUT/GET p50; use the raw table.) For 4 KB objects a different (averages-only) blog gives ~19 ms GET and ~53 ms for 128 KB PUT. | https://topicpartition.io/misc/AWS-S3-PUT-latency-benchmark |
| Note's "S3 Standard p50 small PUT 15-40 ms, p99 100-300+ ms" | UNRESOLVED | Plausible for 4 KB but no primary source; AWS's own figure is 100-200 ms "roughly", measured 500 KB PUT p50 is ~70 ms. Keep the note's UNVERIFIED flag; measure on the target region. | - |
| S3 Express One Zone "single-digit ms" | CONFIRMED (AWS claim) | "consistent single-digit millisecond data access"; "up to 10x faster", requests 80% cheaper than Standard (docs wording predating the April-2025 cuts). | https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-express-one-zone.html |
| Express measured | CONFIRMED (AWS blog, Turso) | 4 KB, same AZ, 1,000 ops: PUT avg 6.4 ms, p95 7, p99 7 ms; GET avg 3.8 ms, p95 4, p99 4 ms. p50 not published. Note's "p99 ~15-30 ms" is pessimistic for same-AZ best case (cross-AZ or concurrent load will be worse; unmeasured). | https://aws.amazon.com/blogs/storage/how-turso-built-a-transactional-database-using-amazon-s3-express-one-zone/ |

## D. Request-rate limits

| Claim | Verdict | Value | Source |
|---|---|---|---|
| 3,500 PUT/COPY/POST/DELETE and 5,500 GET/HEAD per second per partitioned prefix | CONFIRMED (still current) | "at least 3,500 PUT/COPY/POST/DELETE or 5,500 GET/HEAD requests per second per partitioned Amazon S3 prefix"; no limit on prefixes per bucket; scaling is gradual with temporary 503 Slow Down; sustained > 5,000 rps to few objects can trigger 503. S3 Express: "hundreds of thousands of requests per second". | https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html ; https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-design-patterns.html |

## E. ublk kernel facts

Method: fetched `include/uapi/linux/ublk_cmd.h` from github.com/torvalds/linux at every tag v6.0 ... v7.0 (plus v7.1, v7.2-rc1, v7.3-rc1, v7.3, v7.4-rc1) and recorded the first tag whose header defines each macro. "Introduced" = first release (merge window closes at rc1, so a tag containing the macro means that release). The v7.2 final tag fetch was rate limited (HTTP 429); v7.2-rc1 and v7.3-rc1 were used instead. Driver checked: `drivers/block/ublk_drv.c` at v6.14, v6.15, v7.0.

Flags present in the v7.0 header: bits 0-18 (19 flags). The master/v7.3+ header has bits 0-20.

| Flag (bit) | Introduced | Verdict vs `ublk-io-uring-rust` note |
|---|---|---|
| SUPPORT_ZERO_COPY (0) | defined 6.0, functional 6.15 | CONFIRMED "~6.15": the 6.0 flag was the old unimplemented remap idea; v6.14 driver masks it out (`UBLK_F_ALL & ~SUPPORT_ZERO_COPY`); v6.15 driver implements it with `UBLK_U_IO_REGISTER_IO_BUF`/`UNREGISTER_IO_BUF` (io_uring registered buffers). Not available with UNPRIVILEGED_DEV (v7.0 header comment). |
| URING_CMD_COMP_IN_TASK (1) | 6.0 | CONFIRMED |
| NEED_GET_DATA (2) | 6.0 | CONFIRMED |
| USER_RECOVERY (3) | 6.1 | CONFIRMED |
| USER_RECOVERY_REISSUE (4) | 6.1 | CONFIRMED |
| UNPRIVILEGED_DEV (5) | 6.3 | CORRECTED: 6.3 (note said ~6.5) |
| CMD_IOCTL_ENCODE (6) | 6.4 | CORRECTED: 6.4 (note said ~6.6) |
| USER_COPY (7) | 6.5 | CORRECTED: 6.5 (note said ~6.7) |
| ZONED (8) | 6.6 | CONFIRMED |
| USER_RECOVERY_FAIL_IO (9) | 6.13 | CORRECTED: 6.13 (note said ~6.9) |
| UPDATE_SIZE (10) | 6.16 | CORRECTED: 6.16 (note said ~6.15) |
| AUTO_BUF_REG (11) | 6.16 | CONFIRMED |
| QUIESCE (12) | 6.16 | CONFIRMED |
| PER_IO_DAEMON (13) | 6.16 | CONFIRMED |
| BUF_REG_OFF_DAEMON (14) | 6.17 | CORRECTED: 6.17 (note said ~6.16-6.17) |
| BATCH_IO (15) | 7.0 | CORRECTED: 7.0 (note said ~6.19; v6.19 header has no BATCH_IO) |
| INTEGRITY (16) | 7.0 | CONFIRMED; header: "Requires UBLK_F_USER_COPY" |
| SAFE_STOP_DEV (17) | 7.0 | CONFIRMED; adds `UBLK_CMD_TRY_STOP_DEV` (stop only if no openers) |
| NO_AUTO_PART_SCAN (18) | 7.0 | CONFIRMED |
| SHMEM_ZC (19) | 7.1 | CORRECTED: 7.1 (note said "7.1 or 7.2 UNVERIFIED"); present in v7.1-rc1 and v7.1 |
| IO_DESC_SIZE (20) | 7.3 | CORRECTED: 7.3 (absent in v7.2-rc1, present in v7.3-rc1) |
| anything else | - | No further `UBLK_F_*` defined up to v7.4-rc1 (21 flags total). |

| Claim | Verdict | Value | Source |
|---|---|---|---|
| `UBLK_ATTR_VOLATILE_CACHE` exists | CONFIRMED | `(1 << 2)`; in `ublk_ctrl_start_dev` (v7.0): sets `lim.features \|= BLK_FEAT_WRITE_CACHE` | https://raw.githubusercontent.com/torvalds/linux/v7.0/drivers/block/ublk_drv.c (lines ~4350-4353) |
| `UBLK_ATTR_FUA` exists | CONFIRMED | `(1 << 3)`; only honored inside the VOLATILE_CACHE branch: sets `BLK_FEAT_FUA` | same |
| Other attrs | (new) | `UBLK_ATTR_READ_ONLY (1<<0)`, `UBLK_ATTR_ROTATIONAL (1<<1)`; all four present since v6.0 | https://raw.githubusercontent.com/torvalds/linux/v7.0/include/uapi/linux/ublk_cmd.h |
| Latest `libublk` on crates.io | CONFIRMED 0.4.8 | max_version = max_stable = 0.4.8 (2026-08-29). 0.4.7 (2026-08-15) yanked; 0.4.6 2026-05-08; 0.4.5 2025-10-14. | https://crates.io/api/v1/crates/libublk |
| libublk 0.5.x released? | CORRECTED: no | `main` Cargo.toml says `version = "0.5.0"`, but the repo's newest git tag is v0.4.8 and crates.io has no 0.5.x. The note correctly says "repo main is 0.5.0"; do not depend on 0.5 yet. | https://raw.githubusercontent.com/ming1/libublk-rs/main/Cargo.toml ; https://crates.io/api/v1/crates/libublk |
| Linux kernel naming (context) | note | Tags v7.3 and v7.4-rc1/rc2 exist as of this check, so "current mainline" is 7.4-rc; the dev host runs 7.0. | https://github.com/torvalds/linux/tags |

## F. LSVD (EuroSys 2022)

Paper PDF read in full text (pdftotext): Hajkazemi, Aschenbrenner, Abdi, Kaynar, Mossayebzadeh, Krieger, Desnoyers; EuroSys '22, Rennes, April 5-8 2022, pp. 628-643, DOI 10.1145/3492321.3524271. Source: https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf

| Claim | Verdict | Value |
|---|---|---|
| Venue/year | CONFIRMED | EuroSys 2022 (matches the correction already in `prior-art`) |
| Headline IO efficiency vs RBD | CONFIRMED (new numbers) | One 16 KB client random write = 0.25 backend disk I/Os for LSVD vs 6 for RBD (24x). Load test: LSVD 50,000 client IOPS with backend disks over 90% idle; RBD only 12,000 IOPS. |
| vs RBD + bcache write-back | CONFIRMED (new numbers) | Sustained write-back "more than 11x faster than bcache+RBD" (25x advantage in backend efficiency); write-back speed 2x-8x faster, LSVD reaching over 600 MB/s to the remote store; block-level writes up to 30% faster than bcache locally, sync-heavy filesystem workloads up to 4x faster. Filebench on ext4 vs RBD+bcache: varmail 4x, oltp 1.25x, fileserver 0.8x (LSVD slower). Crash test: LSVD image recovered cleanly; RBD+bcache image did not. |
| Test setup | (new) | Ceph Octopus 15.2.3; 10 GbE; config 1 = 4 nodes with 32 SATA SSDs, config 2 = 9 nodes with 62 HDDs; client NVMe 800 GB P3700 (700 GiB cache); RBD triple replication vs LSVD on Ceph RGW with 4+2 erasure coding; 80 GiB volumes. |
| Object (batch) size | CONFIRMED | "configured size (e.g. 8 or 32 MB)"; LSVD writes cluster around 1 MB chunk when Ceph splits 4 MB RADOS objects (EC 4+2). |
| Garbage collection | CONFIRMED | Triggered when live-data/total-object-size drops below fixed threshold, 70% in experiments; greedy cleaning (least-utilized objects first); GC deletes only objects older than the latest checkpoint; with GC on, invalid data stays <= 30% of total. |
| Cache design | CONFIRMED | Log-structured write-back cache on SSD (journal-like, order preserving) + separate read cache + log-structured block store. Read-cache map at 128 KB granularity, 24 B/entry, < 200 MB RAM per TB of read cache; write cache map ~1.2 GB per TB of write cache (16 KB mean write + 4 KB overhead), < 300 MB per TB of cache SSD with 20% write cache; object map assumes mean extent 128-256 KB. |
| Consistency | CONFIRMED | Prefix consistency if local cache is lost; all committed writes recovered after client crash; maps checkpointed periodically (write-cache map to fixed SSD location, object map to numbered object). |
| `prior-art` "~100K write IOPS, Ceph 17.2.0, 32 OSDs 30% CPU" | CORRECTED attribution | Those are lsvd-rbd README figures, not the EuroSys paper. The paper's numbers are above (Ceph 15.2.3). The README's 64 KB read-cache block differs from the paper's 128 KB. |

## G. ZeroFS

| Claim | Verdict | Value | Source |
|---|---|---|---|
| License | CONFIRMED | GNU AGPL v3 (GitHub API: `agpl-3.0`) and README states "Dual-licensed under the GNU AGPL v3 (fully featured, for open source use) and a commercial license" | https://api.github.com/repos/Barre/ZeroFS ; https://raw.githubusercontent.com/Barre/ZeroFS/main/README.md ; https://www.zerofs.net/licensing |
| Dedup | CONFIRMED (absent) | README has no mention of dedup; architecture is 32 KiB extents, compressed, encrypted, packed into immutable segment objects with an LSM metadata store; no content-addressing described. | https://raw.githubusercontent.com/Barre/ZeroFS/main/README.md |
| Online resize | CONFIRMED (absent) | NBD section: "Sizes are fixed at creation: to resize, disconnect, delete, and recreate." New device files are picked up at runtime, but existing ones cannot grow. | same |
| Latest release / activity | (new) | v2.3.6 published 2026-10-27; repo pushed 2026-11-06 | https://api.github.com/repos/Barre/ZeroFS/releases |
| Conditional writes / B2 | (related) | README uses conditional puts for fencing; ClickHouse issue cites ZeroFS#95 as hitting the same B2 limitation. | https://github.com/ClickHouse/ClickHouse/issues/127483 |
| NBD durability | (new) | "FLUSH and FUA replies return only after data is durable", multi-connection flush covers all connections. | README |

## Recommendations for Lunix

1. Keep the capability probe (`probe_cas_semantics`) as the single source of truth per backend; this pass shows vendor docs and real behaviour diverge (Ceph versioned buckets, DigitalOcean, OVH historically 501, B2 501). Probe: create-only twice, stale `If-Match`, concurrent racers, quoted vs unquoted ETag, delete-then-If-Match (404 vs 412), CMU conditional, and behaviour on a versioning-enabled bucket.
2. Write a GCS driver using `x-goog-if-generation-match` (or JSON `ifGenerationMatch`) instead of requiring S3 headers; do not assume GCS interop honours `If-None-Match` on PUT.
3. Classify backends: CAS-capable (AWS, R2, MinIO, Azure, OVH, Scaleway, Tigris, GCS via generation) vs mirror-only (B2, probably Spaces, unverified Wasabi/Hetzner/Ceph-versioned). Fence only through a CAS-capable primary.
4. When modelling Express One Zone cost, include the $0.0032/GB upload and $0.0006/GB retrieval on all bytes: for segments >= ~256 KB the per-GB charge dominates the request charge, so Express only pays off for small journal PUTs (the WAL use case), not for packs.
5. Fix the ublk "Since" column in `ublk-io-uring-rust` before it is used to gate features by kernel version; use the table in section E. Since all flags needed (USER_RECOVERY, CMD_IOCTL_ENCODE, UPDATE_SIZE, QUIESCE) are in 6.16 or earlier, a 6.16+ minimum is defensible; BATCH_IO and SAFE_STOP_DEV need 7.0; SHMEM_ZC needs 7.1.
6. Depend on `libublk = 0.4.8` (or a git pin of main if 0.5 features are needed); revisit when 0.5 is released.
7. Use the LSVD paper's design parameters as starting points (8-32 MB objects, GC at 70% greedy with checkpoint-aware deletes, 128 KB read-cache granularity) and cite the paper, not the lsvd-rbd README, for performance claims.

## Open questions / UNVERIFIED

- GCS Standard storage price and GCP internet-egress first tier (pages render client-side; not retrieved). Cloud Billing Catalog API with a key would settle it.
- OVH per-GB storage price for Standard vs High Performance.
- Ceph RGW: first release with conditional PUT (2014 tracker has no release field), and whether Squid/Reef conditional behaviour is atomic on unversioned buckets. Test on the actual cluster.
- MinIO release that introduced conditional writes (blog says Feb 2023, no tag cited); CMU not listed in the compat tables.
- Wasabi, Hetzner, DigitalOcean: no authoritative vendor statement found; only third-party tests/issues (Wasabi positive, DO negative/mixed, Hetzner exception listing).
- Scaleway page itself returned 403 to the fetcher; the "supported" verdict relies on the search engine's excerpt of the page.
- Express One Zone: p50 latency and cross-AZ latency not published; Turso's numbers are same-AZ, 4 KB, 1,000 ops.
- GCS claim "preconditions unsupported on XML multipart uploads" not checked.
- Aug-2024 AWS date for `If-None-Match` inferred from the announcement URL only.
- The v7.2 final tag header was not fetchable (HTTP 429); IO_DESC_SIZE = 7.3 rests on rc1 tags.
- Prices change; AWS values are from the price-list files published 2026-11-09/10, R2/B2/Azure from vendor pages fetched 2026-11-11.

## Sources

- AWS: https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html ; .../conditional-requests.html ; .../API/API_PutObject.html ; .../API/API_CompleteMultipartUpload.html ; .../s3-express-differences.html ; .../s3-express-one-zone.html ; .../optimizing-performance.html ; .../optimizing-performance-design-patterns.html ; https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes ; https://aws.amazon.com/about-aws/whats-new/2025/10/amazon-s3-conditional-write-functionality-copy-operations ; https://aws.amazon.com/blogs/aws/up-to-85-price-reductions-for-amazon-s3-express-one-zone ; https://aws.amazon.com/blogs/storage/how-turso-built-a-transactional-database-using-amazon-s3-express-one-zone/ ; https://aws.amazon.com/s3/pricing/ ; price lists https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/{AmazonS3,AmazonS3Express,AWSDataTransfer}/current/us-east-1/index.json
- Cloudflare R2: https://developers.cloudflare.com/r2/api/s3/api/ ; https://developers.cloudflare.com/r2/api/s3/extensions/ ; https://developers.cloudflare.com/r2/pricing/
- GCS: https://docs.cloud.google.com/storage/docs/request-preconditions ; https://docs.cloud.google.com/storage/docs/xml-api/reference-headers ; https://cloud.google.com/storage/pricing ; https://cloud.google.com/vpc/network-pricing
- Azure: https://learn.microsoft.com/en-us/rest/api/storageservices/specifying-conditional-headers-for-blob-service-operations ; https://azure.microsoft.com/en-us/pricing/details/bandwidth/
- MinIO: https://docs.min.io/aistor/developers/s3-api-compatibility/ ; https://docs.min.io/community/minio-object-store/reference/s3-api-compatibility.html ; https://www.min.io/blog/leading-the-way-minios-conditional-write-feature-for-modern-data-workloads
- Ceph: https://tracker.ceph.com/issues/8562 ; https://github.com/ceph/ceph/pull/72118 ; /pull/71588 ; /pull/72050 ; https://docs.ceph.com/en/latest/releases/squid/ ; https://docs.ceph.com/en/latest/radosgw/s3/objectops/
- OVH: https://docs.ovhcloud.com/pl/guides/storage-and-backup/object-storage/s3-conditional-writes.md ; https://www.ovhcloud.com/en/public-cloud/object-storage/
- Scaleway: https://www.scaleway.com/en/docs/object-storage/api-cli/using-conditional-writes/
- Backblaze: https://www.backblaze.com/docs/cloud-storage-s3-compatible-api ; https://www.backblaze.com/cloud-storage/pricing ; https://github.com/ClickHouse/ClickHouse/issues/127483 ; https://lists.proxmox.com/pipermail/pbs-devel/2025-July/014234.html
- Hetzner: https://docs.hetzner.com/storage/object-storage/supported-actions/ ; DigitalOcean: https://docs.digitalocean.com/products/spaces/reference/s3-compatibility/ ; https://github.com/denoland/celld/issues/137 ; Wasabi test: https://github.com/haileyok/engram-garden/pull/12 ; Tigris: https://www.tigrisdata.com/docs/objects/conditionals/
- Latency benchmark: https://topicpartition.io/misc/AWS-S3-PUT-latency-benchmark
- Kernel: https://raw.githubusercontent.com/torvalds/linux/<tag>/include/uapi/linux/ublk_cmd.h (tags v6.0 to v7.4-rc1) ; https://raw.githubusercontent.com/torvalds/linux/{v6.14,v6.15,v7.0}/drivers/block/ublk_drv.c
- libublk: https://crates.io/api/v1/crates/libublk ; https://github.com/ming1/libublk-rs
- LSVD: https://rs3lab.github.io/assets/papers/2022/hajkazemi-aschenbrenner:lsvd.pdf
- ZeroFS: https://github.com/Barre/ZeroFS (README, LICENSE, API metadata)
