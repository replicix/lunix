# P0.10: S3 probe sweep (MinIO only)

Status: **partial, by design.** No cloud-provider credentials were available. The probe
(`spikes/s3probe`) is built and tested and ran against a throwaway local MinIO only. Every real
provider is **not run: no credentials**. ADR-2's claim "authority requires conditional writes"
for the three launch providers (AWS S3, Cloudflare R2, OVH) is therefore **neither confirmed
nor refuted** by this run, and the "about 1 PUT RTT" figure for DESIGN §20 sync mode is **not
produced** (loopback MinIO says nothing about a cloud round trip).

Outcome per provider: MinIO (single-drive, unversioned): *authority-capable (gate met)*;
MinIO (same server, versioning-enabled bucket): *authority-capable (gate met)*; all others:
*not run*.

## Header

| | |
|---|---|
| dates | 2026-11-11 (two runs of the whole sweep, about 90 s apart, UTC 23:00-23:04) |
| host | AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root; kernel 7.0.0 (Ubuntu), Ubuntu 26.04.1 LTS; Docker 29.1.3 |
| privileges | unprivileged user in the docker group; no root; no credentials |
| network location label | "local docker, loopback" (client and server on the same host; no TLS) |
| pinning | container `--cpuset-cpus 8-15 --memory 2g`; probe `taskset -c 8-15`; build `-j 4`; CPUs 8-15 shared with other agents |
| MinIO | `RELEASE.2026-11-10T21-18-11Z` (commit 91b5d1aa), go1.27.4, from the Chainguard image `cgr.dev/chainguard/minio:latest`, digest `sha256:db3f583fa49121942168bd6383a773b2cde3384c9a99bf7193560b9a0f3a28fd`, run as the unprivileged ssh uid, single-drive (bind-mounted directory), container `lunix-p0-minio`, published on `127.0.0.1:19000` only |
| why that image | the upstream Docker Hub repository `minio/minio` no longer exists and `quay.io/minio/minio` answers 401 (checked 2026-11-11); this is the first pullable MinIO build that worked |
| probe | `spikes/s3probe` (uncommitted), `object_store` 0.14.1 (the version Constellation's `Cargo.lock` resolves), tokio 1.54.1, futures 0.3.34, bytes 1.12.1, rustc 1.96.0 |
| credentials | generated at run time (`/dev/urandom`), passed through a mode-0600 env file deleted right after container start and through process environment variables; never written to the repo, never printed |
| cost | zero (local); the run used 3,771-3,807 requests and 1,979-1,981 MB transferred per provider entry, inside the plan's cap of about 5,000 requests and 2 GB |

Reproduce on the host: `cd ~/lunix-p0/s3probe && cargo build --release && OUT=out ./run-minio.sh`.
Raw: `raw/run1/{minio,miniover}.{csv,txt}`, `raw/run2/…`, `raw/run{1,2}.log`, `raw/run1/not-run.txt`.

## Provider matrix

| provider | create-if-absent (412) | If-Match swap + stale 412 | 8-way create ×20 | 8-way swap ×100, no torn read | read-your-write GET / LIST | role | note |
|---|---|---|---|---|---|---|---|
| AWS S3 | not run: no credentials | | | | | | |
| Cloudflare R2 | not run: no credentials | | | | | | |
| OVH Object Storage | not run: no credentials | | | | | | |
| MinIO RELEASE.2026-11-10, unversioned | PASS (412) | PASS / PASS (412) | PASS 20/20 | PASS 100/100, 0 torn of ~1,130 reads | PASS / PASS | authority-capable | see below |
| MinIO, versioning enabled on the bucket | PASS (412) | PASS / PASS (412) | PASS 20/20 | PASS 100/100, 0 torn of ~1,140 reads | PASS / PASS | authority-capable | PUT returns a version id; CAS still atomic |
| Google Cloud Storage | not run: no credentials | | | | | | needs a generation-precondition adapter (fact-check §A); the S3 probes would not apply on the interop endpoint |
| Backblaze B2 | not run: no credentials | | | | | | fact-check §A: 501 for `If-None-Match: *`, mirror-only (secondary evidence) |
| Scaleway, Wasabi, Hetzner, Tigris, DigitalOcean Spaces | not run: no credentials | | | | | | probe before use (fact-check §A) |

Gate rule (plan 00, ADR-2, DESIGN §9): a provider is an *authority* backend iff create-if-absent
and If-Match swap behave as above, the 8-way create has exactly one winner in 20/20 rounds, the
8-way swap exactly one winner in 100/100 rounds with no torn reads, and PUT-then-GET is
read-your-write. Otherwise it is mirror-only.

## Semantics, MinIO (identical on both runs; details from `raw/run1/minio.txt`)

| probe | result | observed |
|---|---|---|
| create-if-absent, first | PASS | ETag returned quoted; no version id (unversioned) |
| create-if-absent, second | PASS | HTTP 412 (surfaced by `object_store` as `AlreadyExists` wrapping the 412) |
| If-Match swap with the current ETag | PASS | new ETag |
| If-Match with the stale ETag | PASS | HTTP 412 (`Precondition`) |
| If-Match on a deleted key | PASS | HTTP 404 (`object_store` reports it as `Precondition`; both 404 and 412 are acceptable to the classifier) |
| 8-way concurrent create, 20 rounds | PASS | exactly one winner every round; 140 losers all 412 (7 per round); no 409 seen |
| 8-way concurrent swap from one ETag, 100 rounds | PASS | exactly one winner every round; 700 losers all 412; final object always one writer's complete 64 KiB body; ~1,130 concurrent GETs, 0 torn |
| ranged GET: bounded, suffix, offset | PASS ×3 | byte-exact |
| read-your-write, PUT,PUT,GET ×50 | PASS | 0 stale |
| read-your-write, PUT then LIST ×50; DELETE then LIST ×20 | PASS | 0 missing, 0 lingering |
| multipart create + abort | PASS | abort accepted, object absent |
| bucket versioning state (from the PUT's version id) | INFO | off (unversioned bucket); `enabled` on the second bucket, where the probe PUT returned a version id |
| clock skew | PASS | 0 s (Last-Modified against the local clock window, 1 s resolution) |

Quirks versus Constellation's `cas.rs`/`classify.rs` expectations: **none found**. MinIO
answers create-over-existing with 412, stale If-Match with 412 and If-Match on a missing key
with 404, all of which that classifier handles (`Lost`, `Lost`, `Missing`). No 409
(`ConditionalRequestConflict`) was ever returned under 8-way contention, so the "busy, retry the
same attempt" path was not exercised against a real server in this sweep. No bug to file
against `replicix-objcas`/`classify.rs`.

Relevance to ADR-2's versioning rule ("an authority bucket must have versioning off unless
`doctor`'s concurrent-CAS probe passes on the versioned bucket"): the probe was run on a
versioned MinIO bucket and passed 20/20 and 100/100. That is evidence about MinIO only; the
known problem case (Ceph RGW versioned buckets, tracker 80925) was not available and was not
run.

## Latency and throughput, MinIO on loopback (not representative of any cloud)

Median/p90/p99/max in ms; sequential, distinct keys, one connection; GET TTFB is the time to
response headers. Two runs; where they differ the range is given. `raw/run*/minio.csv`.

| operation | n | p50 | p90 | p99 | max | note |
|---|---|---|---|---|---|---|
| PUT 4 KiB | 200 | 3.78-5.49 | 3.91-5.77 | 4.01-7.70 | 4.05-11.0 | probably MinIO's per-PUT disk sync (not verified); run 2 was slower, host disk shared with other agents |
| GET 4 KiB | 200 | 0.23-0.24 | 0.28-0.29 | 0.32-0.36 | 0.46-0.47 | TTFB p50 0.22-0.23, p99 0.31-0.34 |
| PUT 64 KiB | 200 | 6.19-6.20 | 6.51-6.55 | 8.24-8.36 | 8.45-8.76 | |
| GET 64 KiB | 200 | 0.27-0.30 | 0.33-0.35 | 0.47 | 3.0-3.7 | TTFB p50 0.25-0.28, p99 0.43-0.44 |
| PUT 16 MiB | 40 | 54.1-54.5 | 55.7-56.6 | 59.6-62.4 | 59.6-62.4 | ~300 MB/s single stream |
| GET 16 MiB | 16 | 4.5-4.8 | 4.9-5.1 | 5.3-5.6 | 5.3-5.6 | page-cache hits; TTFB p50 0.74-0.80 |
| HEAD | 100 | 0.19 | 0.23-0.24 | 0.29-0.30 | 0.34 | |
| LIST, 10 keys | 50 | 0.37-0.39 | 0.42-0.43 | 0.66-0.67 | 0.66-0.67 | |

Parallel 16 MiB objects (aggregate MB/s = total bytes / wall time of the whole batch):

| concurrency | objects | PUT MB/s | GET MB/s |
|---|---|---|---|
| 1 | 2 | 302-310 | 3,475-3,670 |
| 8 | 8 | 996-1,027 | 3,152-3,492 |
| 32 | 16 | 1,259-1,329 | 3,359-3,471 |

The versioned bucket's numbers are close (PUT 4 KiB 5.45 ms, 64 KiB 9.21 ms: the extra metadata
fsync); full tables in `raw/run*/miniover.csv`. Network: TCP connect p50 0.01 ms; plain HTTP, no
TLS; cold HEAD 0.4-0.5 ms against warm HEAD 0.4-0.5 ms.

These numbers are the **floor of the local software stack**, not an S3 PUT RTT. The
sync-mode fsync estimate for DESIGN §20 needs the AWS/R2/OVH runs (the fact-check note only
has secondary figures: Standard 500 KB PUT p50 about 70 ms, Express same-AZ 4 KB PUT about
6.4 ms). The 4 KiB PUT p50 of 3.8-5.5 ms here is MinIO's own durable-write cost on this
disk.

## Deviations from the plan

- **Output path.** Written to `bench/s3probe/` as the task asked; the plan text says
  `bench/s3-probe/`. Rename if the plan's spelling is wanted.
- **Matrix trimmed to the 2 GB cap.** The plan's full matrix (16 MiB: sequential n = 40 each way,
  parallel at 1/8/32) would transfer about 3 GB. Run: sequential 16 MiB PUT n = 40 but GET
  n = 16 (not 40), parallel PUT and GET with 2 / 8 / 16 objects at concurrency 1 / 8 / 32 (the
  concurrency-32 cell therefore keeps only 16 transfers in flight). Phases that would exceed the
  cap are skipped and reported by the tool. Override with `LUNIX_PROBE_*_N`.
- **No reuse of Constellation code.** The plan prefers `replicix-objcas` or a path dependency on
  `constellation-store-s3`; the latter pulls Constellation's whole workspace into the build and
  its toolchain pin (1.98) is newer than the host's 1.96. The status classification
  (`http_status`, create/swap/missing mapping) is re-implemented in about 60 lines in
  `spikes/s3probe/src/main.rs`; `object_store` is pinned to the same 0.14.1.
- **Clock skew** uses the Last-Modified of a fresh object because `object_store` does not
  expose the `Date` header; resolution is 1 s, enough for the 2 s flag.
- **TLS RTT** is approximated as cold-HEAD minus warm-HEAD minus TCP connect; not applicable
  here (plain HTTP).
- **Time of day.** Both runs were within four minutes of each other; no second time of day.
- **AWS shared-config profiles** (`_PROFILE`) are not resolved by the spike; export `AWS_*` or
  the provider-specific key variables.
- **MinIO image** is a Chainguard build of upstream MinIO, not the upstream-published image
  (no longer available). The container ran single-drive on a bind mount, not in a
  multi-drive erasure set.
- **Extra:** a second bucket with versioning enabled (hand-signed `PutBucketVersioning` in
  `tools/enable_versioning.py`), because ADR-2 has an explicit versioning rule.
- Torn-read check uses 64 KiB uniform-byte bodies with two concurrent readers per swap round;
  a provider that serves partial objects would show non-uniform bytes. 0 found.

## Not run

AWS S3, Cloudflare R2, OVH (the three launch providers), Google Cloud Storage, Backblaze B2,
Scaleway, Wasabi, Hetzner, Tigris, DigitalOcean Spaces, S3 Express One Zone, Ceph RGW: **not
run: no credentials**. Nothing about them was measured; any statement about them is from the
fact-check note, which is secondary evidence for several of them.

## Cleanup and leftover check (remote)

- Container `lunix-p0-minio`: removed (`docker rm -f`) at the end of each run; none remain.
- Image `cgr.dev/chainguard/minio:latest` (236 MB, pulled by me): removed with `docker rmi`.
- Volumes: none created (bind mount); the data directory `~/lunix-p0/s3probe-minio-data` was
  deleted by the script. The docker image, container and volume lists equal the lists
  recorded before the run (diff empty); pre-existing containers/images/volumes were not touched.
- Objects: the probe lists and deletes only `lunix-p0-probe/<run-id>/`; "leftover objects under
  the probe prefix: 0" on all four runs (the versioned bucket additionally held delete markers
  and non-current versions, which vanished with the data directory).
- Port 19000: free. No `s3probe`, `run-minio.sh` or MinIO process running.
- Credentials: generated per run, in memory and a deleted env file only; the outputs and
  sources were grepped for host identifiers (none found).

## Open questions for the maintainer

1. Credentials (even short-lived and restricted to a single bucket) for AWS S3, R2 and OVH to
   complete the matrix and produce the "1 PUT RTT" figure; run
   `LUNIX_PROBE_<NAME>_BUCKET=… ./target/release/s3probe --providers AWS,R2,OVH`. Expected
   cost: about 3,800 requests and 2 GB of transfer per provider (AWS: under $0.20 at the
   fact-check list prices, mostly egress for about 0.9 GB of GETs; R2 egress free; OVH no
   request or egress charges per the fact-check, unverified for this account).
2. A Ceph RGW target on a *versioned* bucket would exercise the one provider where ADR-2's
   versioning rule matters.
3. Should `lunix doctor` run this whole sweep or a reduced "quick" form (suggested: 5 create
   rounds, 20 swap rounds, no 16 MiB transfers, about 300 requests)? The gate rule above was
   written for the full form.

## Proposed decision record (for the lead to merge; do not apply here)

Append under ADR-2 in `docs/explanation/DECISIONS.md`:

> **Gate outcome, partial (2026-11-11, P0.10):** the S3 probe harness (`spikes/s3probe`) is
> built and verified against MinIO `RELEASE.2026-11-10T21-18-11Z`, which meets the authority
> rule on an unversioned and on a versioned bucket (20/20 create races, 100/100 swap races,
> 0 torn reads, 412 on lost races, 404 on If-Match for a deleted key, read-your-write for GET
> and LIST). AWS S3, Cloudflare R2 and OVH were **not run: no credentials**; the claim that
> the three launch providers can be authorities remains documentation-based (fact-check
> §A) until those runs exist. Status of ADR-2 unchanged (proposed). Evidence:
> `bench/s3probe/RESULTS.md`.
