# P0.10: S3 probe sweep (AWS S3 Standard, OVH Object Storage, MinIO)

Status: **done for the two real providers.** `spikes/s3probe` ran against AWS S3 Standard (from a
client in the same region and from Hungary), OVH Object Storage Milan (from Hungary) and, earlier,
a throwaway local MinIO. Cloudflare R2, GCS, B2 and the rest are still **not run: no credentials**.

Outcome per provider, ADR-2's authority gate (create-if-absent 412, If-Match swap and stale 412,
8-way create 20/20, 8-way swap 100/100 with no torn read, read-your-write by GET and LIST):

| provider | gate | notes |
|---|---|---|
| AWS S3 Standard (unversioned bucket) | **authority-capable (met), 3 of 3 runs** | also `If-None-Match: *` on `CompleteMultipartUpload` works (create-only, 412 on an existing key) |
| OVH Object Storage, Milan (PUT returns a version id) | **authority-capable (met), 1 full sweep** | same conditional-Complete result; passes whatever the bucket's versioning state is |
| MinIO RELEASE.2026-11-10 (unversioned and versioned bucket) | authority-capable (met) | earlier sweep, loopback |
| R2, GCS, B2, Scaleway, Wasabi, Hetzner, Tigris, Spaces | not run: no credentials | |

Headline numbers (create-only or plain PUT, concurrency 1, medians; details below):

| | 4 KiB PUT p50 / p99 | 1 MiB PUT p50 | 16 MiB PUT p50 | fixed cost + slope (fit of p50) | two serial 4 KiB create-only PUTs p50 / p99 |
|---|---|---|---|---|---|
| AWS, same region (us-west-2) | 29.7 / 45 ms (2 runs pooled, n=80) | 73 ms | 210 ms | 42 ms + 10.6 ms/MiB (about 95 MiB/s) | 54 / 94 ms (n=200) |
| OVH, Hungary → Milan | 39.6 / 46-54 ms (runs 1 and 3) | 130-138 ms | 886-911 ms | 71 ms + 52.9 ms/MiB (about 19 MiB/s, Wi-Fi-bound) | 97 / 133-212 ms |
| AWS, Hungary → us-west-2 | 218 / 302 ms | 630 ms | 5.5 s | 291 ms + 328 ms/MiB (3 MiB/s) | 444 / 1,059 ms |

## Header

| | |
|---|---|
| dates | 2026-11-12 (MinIO: 2026-11-11). All times below are offsets from the start of the first AWS sweep, T+0 |
| AWS client ("same region (us-west-2)") | test host: AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root; kernel 7.0.0 (Ubuntu), Ubuntu 26.04.1 LTS; probe pinned `taskset -c 8-15`, build `-j 4`; glibc build |
| OVH and cross-continent clients ("Hungary → Milan", "Hungary → us-west-2") | the local workstation, running a static musl binary built on the test host (the workstation never compiled anything). the single-stream rate (19 MiB/s to Milan) and the aggregate upload rate (31 MB/s) were bound by that machine's Wi-Fi link (5 GHz, about 270 Mbit/s link rate, -61 dBm; the wired uplink behind it is 1 Gbit/s), so large-object numbers from Hungary describe that path, not the provider |
| probe | `spikes/s3probe` (uncommitted), `object_store` 0.14.1, reqwest 0.13.5, sha2 0.10.9, hmac 0.12.1, tokio 1.54.1, futures 0.3.34, bytes 1.12.1, rustc 1.96.0; `musl-tools` installed on the host with `apt-get` for the static build |
| credentials | short-lived SSO credentials for AWS and the OVH profile's keys, each read with `aws configure export-credentials`, held only in the environment of the probe process (AWS: piped over ssh stdin); never in a file, log or command line |
| scope | only the key prefix `lunix-p0-probe/<run-id>/` of each bucket. No bucket setting was changed (versioning was only read), no lifecycle/policy/CORS/ACL/tags; every multipart upload the probe started was completed or aborted |

Runs (offsets are minutes after T+0; "sweep" = semantics + latency suites):

| run | client → target | start | suites | requests (probe counter) | transferred |
|---|---|---|---|---|---|
| AWS host smoke | same region | T-1 | cond, net | 808 | 136 MB |
| AWS host 1 | same region | T+0 | all (sem, cond, seq, sizes, pairs, lane, net) | 6,722 | 2,958 MB |
| OVH local 1 | Hungary → Milan | T+0.5 | all | 7,069 | 2,946 MB |
| OVH smoke | Hungary → Milan | T-1 | cond, net | 808 | 136 MB |
| AWS local 1 | Hungary → us-west-2 | T+7 | sem, cond, sizes, pairs, lane (small lane), net | 4,584 | 1,245 MB, of which about 40 MB were GETs (swap-race reads) |
| AWS host 2 | same region | T+31 | sem, cond, sizes, pairs, lane, net | 5,376 | 2,397 MB |
| AWS host 3 | same region | T+35 | cond (40 race rounds), lane (64 x 16 MiB per phase), net | 2,638 | 2,557 MB |
| OVH local 2 | Hungary → Milan | T+37 | cond (40 race rounds), sizes, pairs, net | 3,016 | 612 MB |
| OVH local 3 | Hungary → Milan | T+42 | sizes, pairs, net | 994 | 475 MB |

Totals: AWS from the host 15,544 requests / 8,048 MB over four runs (largest run 6,722 requests,
2,958 MB); AWS from Hungary 4,584 requests; OVH 11,887 requests / 4,169 MB over four runs (largest run 7,069,
2,946 MB). Each run stayed under the 10,000 requests and about 3 GB caps. The request counter is the
probe's own and omits a few helper calls (it is a lower bound by a few percent).

Reproduce on the host: `cd ~/lunix-p0/s3probe && cargo build --release`, export the
`LUNIX_PROBE_<NAME>_*` variables (README), `./target/release/s3probe --out out --providers AWS`.
Raw: `raw/aws-host-run{1,2,3}`, `raw/aws-local-run1`, `raw/ovh-local-run{1,2,3}` (each: `<p>.txt`,
`<p>.csv`, `<p>-samples.csv`, one row per timed request); MinIO: `raw/run{1,2}`.
`spikes/s3probe/tools/pool.py` recomputes every percentile and fit from the sample CSVs.

## Provider matrix

| provider | create-if-absent (412) | If-Match swap + stale 412 | 8-way create ×20 | 8-way swap ×100, no torn read | read-your-write GET / LIST | conditional Complete (If-None-Match) | role |
|---|---|---|---|---|---|---|---|
| AWS S3 Standard, same region | PASS (412), 2/2 sweeps | PASS / PASS (412), 2/2 | PASS 20/20, 2/2 | PASS 100/100, 0 torn of 1,166 and 1,178 reads | PASS / PASS | PASS: absent 30/30, existing 412 10/10, 8-way 70/70 rounds one winner | **authority-capable** |
| AWS S3 Standard, from Hungary | PASS | PASS / PASS | PASS 20/20 | PASS 100/100, 0 torn of 590 | PASS / PASS | same | authority-capable |
| OVH Object Storage Milan | PASS (412) | PASS / PASS (412) | PASS 20/20 | PASS 100/100, 0 torn of 1,030 reads | PASS / PASS | PASS: absent 30/30 (twice), existing 412 10/10 (twice), 8-way 50/50 rounds, one winner each | **authority-capable** |
| MinIO RELEASE.2026-11-10, unversioned | PASS (412) | PASS / PASS (412) | PASS 20/20 | PASS 100/100, 0 torn of ~1,130 | PASS / PASS | not run (probe added later; the MinIO loopback smoke of the new suite passed 30/30, 10/10, 10/10 rounds) | authority-capable |
| MinIO, versioning enabled | PASS (412) | PASS / PASS (412) | PASS 20/20 | PASS 100/100, 0 torn of ~1,140 | PASS / PASS | not run | authority-capable |
| Cloudflare R2 | not run: no credentials | | | | | | |
| Google Cloud Storage | not run: no credentials | | | | | | needs a generation-precondition adapter (fact-check §A) |
| Backblaze B2 | not run: no credentials | | | | | | fact-check §A: 501 for `If-None-Match: *`, mirror-only |
| Scaleway, Wasabi, Hetzner, Tigris, DigitalOcean Spaces | not run: no credentials | | | | | | probe before use |

Gate rule (plan 00, ADR-2, DESIGN §9): a provider is an *authority* backend iff create-if-absent
and If-Match swap behave as above, the 8-way create has exactly one winner in 20/20 rounds, the
8-way swap exactly one winner in 100/100 rounds with no torn reads, and PUT-then-GET is
read-your-write. Otherwise it is mirror-only. The conditional-Complete result is not part of the
gate; it matters only for staging variant C.

## Semantics, AWS S3 and OVH (identical across sweeps unless noted)

| probe | AWS S3 Standard | OVH Milan |
|---|---|---|
| create-if-absent, second | 412 (`AlreadyExists`) | 412 |
| If-Match swap with current ETag / stale ETag | new ETag / 412 | new ETag / 412 |
| If-Match on a deleted key | 404 (`object_store` reports `Precondition`) | 404 |
| 8-way create x20 | exactly one winner every round; losers 412 (140) | same; no 409 |
| 8-way swap x100 | exactly one winner every round; losers 412 (700) and **409 retries: 369, 408 and 310 in the three sweeps** | same; 128 409 retries; 0 torn reads |
| ranged GET (bounded, suffix, offset) | byte-exact | byte-exact |
| read-your-write, PUT,PUT,GET x50; PUT-then-LIST x50; DELETE-then-LIST x20 | 0 stale, 0 missing, 0 lingering | same |
| multipart create + abort | abort accepted, object absent | same |
| bucket versioning | PUT returned no version id; `GetBucketVersioning` (read-only) has no `Status`: never enabled | **PUT returns a version id** (an integer-like timestamp string), yet `GetBucketVersioning` returns no `Status`; a read-only `list-object-versions` of the probe prefix after cleanup showed 0 versions and 0 delete markers |
| ETag | quoted | quoted |
| clock skew (Last-Modified against local clock, 1 s resolution) | 1 s (same region, T+0), 1 s (Hungary) | 0 s |

Quirks against Constellation's `cas.rs`/`classify.rs`: both providers answer create-over-existing
with 412 and stale If-Match with 412, which that classifier handles. **AWS and OVH both return 409
(`ConditionalRequestConflict`) for concurrent If-Match swaps** (not for concurrent creates), so the
"busy, retry the same attempt" path is exercised by real servers for the first time; the swap race
counted every 409 as a retry and still ended with exactly one winner per round, so the 409 handling
is a liveness concern only. MinIO never returned a 409. No bug to file against `replicix-objcas`.
OVH's versioning state is ambiguous: object versions are minted but the bucket-level read reports
nothing. ADR-2's rule ("versioning off unless the concurrent-CAS probe passes on the versioned
bucket") is satisfied either way, because the probe passed on the bucket as OVH serves it.

## Conditional `CompleteMultipartUpload`

Hand-signed requests (`src/raw.rs`) because `object_store` cannot send the header on Complete.
Create, UploadPart and Complete with `If-None-Match: *`.

| check | AWS S3 | OVH |
|---|---|---|
| Complete on an absent key | 30/30 succeed, in every run | 30/30, in every run |
| Complete on a key that already exists (10 rounds) | **412 x10** (not silently overwritten) | **412 x10** |
| 8 uploads of one key complete together | 70/70 rounds with exactly one winner (10, 10, 10, 40 rounds across runs: 200 once, others 412 `PreconditionFailed`; two AWS runs each had one loser answered with a 5xx whose code was not captured, none in the 320 attempts of the run that did capture codes) | 50/50 rounds (10 + 40), 200 once, 412 for all 7 others, no 409 |

Latency in milliseconds (p50 / p90; n=30 for 1 part, 8 for 2 parts, 6 for 4 parts; range of the runs
given where there were several). The 2-part case is 5 MiB + 4 KiB and the 4-part case 3 x 5 MiB + 4 KiB.

| step | AWS same region (3 runs) | OVH Hungary → Milan (2 runs) | AWS Hungary → us-west-2 |
|---|---|---|---|
| CreateMultipartUpload | 24.4-29.9 | 38.3-39.1 | 216-218 |
| UploadPart 4 KiB | 33.2-38.4 | 47.4-48.5 | 227 |
| Complete, 1 part, unconditional | 85.1-90.1 | 66.4-67.9 | 283 |
| Complete, 1 part, `If-None-Match: *` | **86.6, 97.0, 91.8** | **75.5, 77.6** | 285 |
| Complete, 2 parts (5 MiB + 4 KiB), conditional | 92.1-113.0 | 77.4-78.2 | 295 |
| Complete, 4 parts (15 MiB + 4 KiB), conditional | 96.5-100.7 | 80.3-84.9 | 286 |
| for comparison: plain 4 KiB PUT | 29-31 | 39-40 | 218 |

Reading: on both providers Complete costs one slow request independent of part count and size
(under 20 MiB): about 3 times a 4 KiB PUT on AWS (87 vs 30 ms) and about 2 times on OVH (76 vs 40 ms).
The `If-None-Match` header adds roughly 1-9 ms over the unconditional Complete. A multipart commit
(variant C: Create + parts + Complete) therefore costs far more than the single create-only PUT it
would replace: 25 + 34 + 87 = about 146 ms sequentially on AWS against 30 ms, unless Create and the
parts were done ahead of the flush. The fsync-staging note asked for exactly this number
(open question 2) and AWS, OVH and MinIO now have it; R2 does not.

## PUT latency by object size (concurrency 1, sequential, plain PUT, unique keys)

n = 40 per size up to 1 MiB and 12 above, sizes interleaved round by round, connection warmed.
Milliseconds; p99 over 40 or 12 samples is effectively the maximum. Two AWS same-region sweeps 31
minutes apart are shown as `run 1 / run 2`.

AWS S3 Standard, same region (us-west-2):

| size | p50 | p90 | p99 |
|---|---|---|---|
| 4 KiB | 30.6 / 29.2 | 37.7 / 32.7 | 45.3 / 67.1 |
| 64 KiB | 43.4 / 33.0 | 72.6 / 41.4 | 124.3 / 68.8 |
| 256 KiB | 48.9 / 45.1 | 87.0 / 73.9 | 113.1 / 119.6 |
| 1 MiB | 70.5 / 77.7 | 104.6 / 140.1 | 150.7 / 182.7 |
| 2 MiB | 67.9 / 59.3 | 85.0 / 97.5 | 210.9 / 100.7 |
| 4 MiB | 86.1 / 83.4 | 123.7 / 148.7 | 150.1 / 155.3 |
| 8 MiB | 128.5 / 130.6 | 165.9 / 181.9 | 169.0 / 216.7 |
| 16 MiB | 227.0 / 205.6 | 288.9 / 215.5 | 319.0 / 291.5 |

Fit (least squares on the eight per-size percentiles, `t = fixed + slope x MiB`):

| fit | fixed cost | slope | implied single-stream rate |
|---|---|---|---|
| AWS same region, p50, run 1 / run 2 / pooled samples | 43.5 / 42.1 / **42.3 ms** | 11.3 / 10.4 / **10.6 ms/MiB** | about 95 MiB/s |
| same, 1-16 MiB points only (pooled) | 50.7 ms | 9.9 ms/MiB | about 100 MiB/s |
| AWS same region, p99 (pooled) | 113 ms | 13.3 ms/MiB | |
| OVH Hungary → Milan, p50, run 1 / run 3 | 70.2 / 72.2 ms | 53.7 / 52.0 ms/MiB | about 19 MiB/s (Wi-Fi-bound) |
| OVH, p50, run 2 (degraded, see below) | 125.9 ms | 51.1 ms/MiB | |
| OVH, p99, run 1 / run 3 | 202 / 160 ms | 74 / 56 ms/MiB | |
| AWS Hungary → us-west-2, p50 | 291 ms | 328 ms/MiB | about 3 MiB/s |

The AWS curve is not a straight line: 2 MiB (60-68 ms) is faster than 1 MiB (70-78 ms) in both
sweeps, and 256 KiB (45-49) is already close to 1 MiB; the first 1 MiB is dominated by the fixed
cost and what looks like a per-request cost step between 64 KiB and 1 MiB. A two-point fit on the
4 KiB and 16 MiB medians gives 30 ms + 11.3 ms/MiB. The fact-check figure "Standard 500 KB PUT p50
about 70 ms" sits at the top of what is measured here for 256 KiB-1 MiB (45-78 ms).

OVH Hungary → Milan, run 1 / run 3 (run 2 in parentheses):

| size | p50 | p90 | p99 |
|---|---|---|---|
| 4 KiB | 39.8 / 39.4 (104.6) | 41.5 / 40.7 (160.5) | 53.8 / 46.1 (235.4) |
| 64 KiB | 82.5 / 79.3 (131.7) | 102.0 / 136.6 (195.2) | 157.3 / 205.8 (248.4) |
| 256 KiB | 74.8 / 78.2 (140.1) | 112.4 / 107.8 (190.3) | 404.3 / 169.7 (252.5) |
| 1 MiB | 130.0 / 137.5 (188.3) | 187.8 / 192.7 (289.8) | 346.6 / 357.4 (444.7) |
| 2 MiB | 192.9 / 167.6 (236.7) | 267.1 / 223.2 (268.5) | 269.5 / 253.5 (288.8) |
| 4 MiB | 276.4 / 304.2 (339.5) | 393.5 / 328.1 (352.7) | 483.7 / 330.7 (355.4) |
| 8 MiB | 534.6 / 514.7 (519.9) | 701.7 / 564.1 (630.1) | 856.2 / 603.3 (644.7) |
| 16 MiB | 911.2 / 886.1 (947.5) | 1119.5 / 947.9 (1000.2) | 1365.0 / 1060.6 (1020.3) |

The TCP connect time to the endpoint was 20.1-20.5 ms in every run, so a 4 KiB PUT is about 2
RTT (40 ms) on a warm connection. **Run 2 (T+37) was degraded**: the small-object medians were 2-2.6 times
those of runs 1 and 3 while the TCP RTT was unchanged, so the cause is server-side latency or
queueing on the path (not determined). Run 3 five minutes later matched run 1. Use runs 1 and 3 as
the OVH figures and keep run 2 as the evidence that OVH latency varies in time.

AWS Hungary → us-west-2 (one run): 4 KiB 217.9 / 222.9 / 301.7 ms; 64 KiB 418 / 453 / 604; 256 KiB 430 / 465 / 1,836; 1 MiB 630 / 1,004 / 2,197;
2 MiB 844; 4 MiB 1,418; 8 MiB 3,177; 16 MiB 5,466 / 8,479 / 11,457 (p50 / p90 / p99). TCP connect 188 ms.

Not run: the "8 parallel, idle" cell of the review's table per size (only the 16 MiB parallel
aggregate was run, below), S3 Express One Zone, R2.

## Two serial PUTs (the FLUSH-then-FUA pattern)

A create-only PUT, then a second create-only PUT once the first returned; the pair time in
milliseconds (p50 / p90 / p99), n=100 for 4 KiB + 4 KiB and n=40 for 1 MiB + 4 KiB (data object, then commit record).

| pair | AWS same region run 1 | run 2 | OVH run 1 | OVH run 3 | AWS Hungary |
|---|---|---|---|---|---|
| 4 KiB + 4 KiB | 56.7 / 63.4 / 87.4 | 48.0 / 59.4 / 98.6 | 97.5 / 104.4 / 132.9 | 96.5 / 174.9 / 211.7 | 443.8 / 457.2 / 1,059 |
| 1 MiB + 4 KiB | 81.8 / 116.0 / 133.4 | 81.4 / 113.7 / 188.8 | 192.5 / 250.7 / 362.3 | 192.0 / 270.0 / 377.5 | 695 / 1,035 / 2,281 |

AWS 4 KiB pair pooled over both runs (n=200): p50 53.8, p90 62.3, p99 93.9 ms, which is 1.8
times a single 4 KiB PUT (29.7 ms p50) and 2.1 times its p99 (45). OVH run 2 (degraded): 268 / 323 / 416 ms.
A sync-mode flush that must write a 1 MiB data object and then the commit costs about 81 ms p50,
133-189 ms p99 on AWS, with the second PUT about 27 ms p50 (the commit PUT after a data PUT is no
slower than a stand-alone one).

## Lane isolation: small create-only PUTs during a bulk upload

Bulk: 16 MiB objects at concurrency 8, unthrottled, on one client; small PUTs: alternating 4 KiB and
64 KiB create-only, sequential, either on a **separate client** (own connection pool) or on the
**same client** as the bulk, counted only when the whole PUT lay inside the bulk window (the window
ends when the first bulk worker finds the queue empty). "Idle" is the same small PUTs with no bulk.
p50 / p90 / p99 in ms. Note that the `object_store`/reqwest pool is unbounded, so "same client" shares
the pool object and idle connections but does not limit the connection count: a small PUT during bulk
opens its own connection either way.

AWS same region, samples pooled over three runs (idle n=90 per size; separate n=44 / 42; shared n=45 / 42):

| small PUT | idle | bulk running, separate client | bulk running, same client |
|---|---|---|---|
| 4 KiB | 28.2 / 33.6 / 58.6 | 30.0 / 34.5 / 51.2 | 28.7 / 41.8 / 197.9 |
| 64 KiB | 34.4 / 55.8 / 95.8 | 38.2 / 62.4 / 368 | 37.6 / 64.3 / 125.1 |

Bulk goodput in MB/s (decimal) per run, runs 1 / 2 / 3: bulk alone 461 / 532 / 520; with the
separate-client sampler 451 / 576 / 586; with the same-client sampler 447 / 529 / 580. The
sampler cost the bulk nothing measurable, and the bulk (about 0.5 GB/s, 8 connections) cost the
small PUTs about 2 ms at p50 (4 KiB, +6%) to 4 ms (64 KiB, +11%), with no difference between
the two pool arrangements at p50. The tails (p99 of 44 samples is the maximum) are too thin to separate
the arrangements; the largest outliers (198 ms shared 4 KiB, 368 ms separate 64 KiB) appeared in both.
The bulk rate was limited by the 8 connections, not shown to be limited by the instance's network,
so this does not test a saturated NIC.

OVH Hungary → Milan (run 1; the client's Wi-Fi link *was* saturated: bulk goodput 31 MB/s;
idle n=30, separate n=97, shared n=101):

| small PUT | idle | bulk running, separate client | bulk running, same client |
|---|---|---|---|
| 4 KiB | 47.7 / 49.2 / 58.8 | 52.7 / 60.3 / 66.6 | 52.3 / 57.8 / 69.5 |
| 64 KiB | 81.4 / 132.9 / 238.1 | 87.7 / 130.8 / 196.8 | 86.7 / 115.3 / 157.1 |

Bulk goodput alone / separate / shared: 31.2 / 31.4 / 30.7 MB/s. On a saturated Wi-Fi link the small PUT
p50 rose only 5 ms (+10%, 4 KiB) and 6 ms (+8%, 64 KiB), p99 +8 to +11 ms for 4 KiB, again with
no difference between separate and shared clients. 

AWS Hungary → us-west-2 (n=11-19 per cell, bulk goodput 6-7 MB/s): 4 KiB idle 217 / 245 / 335,
separate 224 / 243 / 832, shared 218 / 226 / 229; 64 KiB idle 415 / 440 / 600, separate 421 / 449 /
664, **shared 603 / 800 / 3,019**. The 64 KiB shared-client result (+45% p50) is the only sign of
interference in the whole matrix; it is one run with 18 samples and a slow, lossy long-RTT path, so
treat it as a hint that a separate pool matters when the path is slow, not as a result.

Conclusion for the review's question ("does lane isolation keep the commit PUT near its idle
latency?"): with 8 concurrent 16 MiB uploads the commit-sized PUT stays within about 10% of idle at
p50 on AWS same-region and on a saturated Wi-Fi link to Milan, whether or not it has its own client. A
separate pool was not needed for p50 in these tests; the p99 effect is unresolved and needs more
samples. A shaped 200 MB/s upload on a bandwidth-constrained NIC was not tested.

## Other latency and throughput (suite `seq`; AWS same region run 1, OVH run 1)

Median / p90 / p99 in ms; sequential, one connection. The OVH 4 KiB PUT and GET were measured at a
slow stretch of run 1 (the 4 KiB PUT of the later `sizes` suite was 40 ms).

| operation | AWS same region | OVH Hungary → Milan |
|---|---|---|
| HEAD 1 KiB (n=100) | 11.7 / 13.4 / 21.7 | 29.1 / 30.7 / 33.9 |
| LIST, 10 keys (n=50) | 14.4 / 16.2 / 19.9 | 31.8 / 36.2 / 42.0 |
| GET 4 KiB (n=200) | 23.9 / 29.7 / 42.8 | 78.1 / 95.8 / 112.6 |
| PUT 4 KiB (n=200) | 31.1 / 39.3 / 59.8 | 90.4 / 126.6 / 160.6 |
| GET 64 KiB (n=200) | 26.0 / 41.4 / 86.4 | 78.9 / 107.7 / 132.0 |
| PUT 64 KiB (n=200) | 39.9 / 79.4 / 161.8 | 128.4 / 193.4 / 288.5 |
| PUT 16 MiB, one stream (n=3) | 214 (78 MB/s) | 861 (19.5 MB/s) |
| GET 16 MiB, one stream (n=3) | 175 (96 MB/s), TTFB 27 | 887 (18.9 MB/s), TTFB 134 |

Parallel 16 MiB (aggregate MB/s; few objects, so rough): AWS same region PUT 72 / 332 / 196 and
GET 78 / 788 / 180 at concurrency 1 / 8 / 32 (the concurrency-32 cell had only 4 objects in flight);
OVH Hungary → Milan PUT 19 / 26 / 30 and GET 18 / 28 / 26 MB/s, bounded by the client's Wi-Fi link.

- **Multipart section (deviations).** Goodput cells are 403 MB on AWS (not 1 GB) because the required
  latency cells (2.35 GB) plus three 1 GB cells would pass the 4 GB cap; the 8 x 4 MiB and 8 x 2 MiB
  splits are not allowed by the 5 MiB minimum (replaced by 3 and 6 parts); the sequential control
  covers one split per size; the 64 MiB optional cell was not run.

## Cleanup and safety

- After every run the probe listed and deleted only its own prefix and re-listed it: **leftover
  objects 0 in all 9 real-provider runs**; it also listed open multipart uploads under its prefix: **0 open at the end of
  every run**. Uploads created / aborted / completed per run: 164 / 80 / 84 (smoke, sweep) and
  404 / 290 / 114 (the 40-round race runs); every created upload was either completed (and then
  deleted as an object) or aborted.
- An independent check afterwards with the AWS CLI (list objects, list multipart uploads, and for OVH
  list object versions, all with the prefix `lunix-p0-probe/`): **empty** in both buckets, 0 versions and 0 delete markers on OVH.
- No bucket configuration was read other than `GetBucketVersioning`, and none was written; the
  `enable_versioning` tool was not run; nothing outside `lunix-p0-probe/` was touched.
- The probe runs on the test host left no process, container or output directory behind.

## Semantics, MinIO (earlier sweep) (identical on both runs; details from `raw/run1/minio.txt`)

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

## Multipart vs single PUT (16 MiB and 32 MiB objects)

Question: is a multipart upload with parts sent in parallel worth it compared with one PUT? Measured
with `s3probe mpu` (new subcommand, `spikes/s3probe/src/mpu.rs`; all requests through the hand-signed
client in `raw.rs`, one connection pool, bodies sent unsigned over https, so the single PUT and the
parts use the same stack). Raw: `raw/aws-host-mpu`, `raw/ovh-local-mpu`, `raw/ovh-local-mpu2` (plus the
`-check` directories with the 5 MiB check); `tools/mpu_summary.py <p> <dirs>` regenerates the tables.

**Allowed splits.** Both providers refuse a Complete whose non-final part is under 5 MiB
(`400 EntityTooSmall`, tested with 1 MiB + 1 MiB on AWS and OVH). So 16 MiB in 8 x 2 MiB or 4 x 4
MiB, and 32 MiB in 8 x 4 MiB, were **not run: not allowed**. Used instead: 16 MiB as 2 x 8 MiB and 3
x 5.33 MiB (5,592,406 B); 32 MiB as 2 x 16, 4 x 8 and 6 x 5.33 MiB. "parN" = N parts in parallel,
"seqN" = N parts one after the other (control; one split per size: seq2 for 16 MiB, seq4 for 32 MiB,
to stay within the byte cap). Times in ms, one object at a time on a warmed pool, cells interleaved
round by round with rotating order; "total" is end to end (Create + slowest part + Complete),
"parts wall" is from the first part's launch to the last part's answer.

### Per-object latency

AWS S3 Standard, same region (us-west-2), n = 10 per cell, no failed sample:

| size | form | total p50 | p90 | min | max | Create p50 | parts wall p50 | Complete p50 |
|---|---|---|---|---|---|---|---|---|
| 16 MiB | single PUT | **194** | 216 | 178 | 229 | - | - | - |
| 16 MiB | par2 (8 MiB) | 249 | 278 | 233 | 316 | 24 | 127 | 91 |
| 16 MiB | par3 (5.33 MiB) | 252 | 295 | 198 | 316 | 24 | 126 | 88 |
| 16 MiB | seq2 (control) | 394 | 435 | 325 | 435 | 22 | 274 | 92 |
| 32 MiB | single PUT | 336 | 359 | 328 | 399 | - | - | - |
| 32 MiB | par2 (16 MiB) | 343 | 366 | 283 | 370 | 22 | 210 | 94 |
| 32 MiB | par4 (8 MiB) | **265** | 279 | 247 | 285 | 23 | 139 | 96 |
| 32 MiB | par6 (5.33 MiB) | 271 | 302 | 217 | 318 | 25 | 149 | 99 |
| 32 MiB | seq4 (control) | 584 | 621 | 529 | 644 | 22 | 460 | 89 |

Reading: multipart costs a fixed Create (about 23 ms) plus Complete (about 90 ms, the same as the
earlier conditional-Complete figure), about 115 ms, before any part moves. Parallel parts saved 67 ms
at 16 MiB (194 -> 127 for the data phase) and 197 ms at 32 MiB (336 -> 139), so the net is **+55 ms
worse at 16 MiB and 71 ms (21%) better at 32 MiB**. Interpolating the two sizes, break-even is near
25 MiB (not measured between). More parts than 4 buy nothing (par6 = par4). The slowest part is the
whole parts phase; sequential parts are slower than one PUT, as expected.

OVH Milan from the Wi-Fi client ("Hungary → Milan (Wi-Fi client)"), n = 12 per cell (two runs of 6, 3
minutes apart, pooled), no failed sample. The single-stream rate on this link was 12 MB/s in these
runs (19 MB/s in the earlier size sweep), and it wanders in time (hence min far below p50):

| size | form | total p50 | p90 | min | max | Create p50 | parts wall p50 | Complete p50 |
|---|---|---|---|---|---|---|---|---|
| 16 MiB | single PUT | 1,424 | 1,509 | 750 | 1,553 | - | - | - |
| 16 MiB | par2 | 1,597 | 1,693 | 816 | 1,731 | 41 | 1,491 | 66 |
| 16 MiB | par3 | **1,164** | 1,282 | 876 | 2,130 | 39 | 1,044 | 70 |
| 16 MiB | seq2 (control) | 2,224 | 2,457 | 1,010 | 2,766 | 39 | 2,119 | 67 |
| 32 MiB | single PUT | 2,770 | 3,271 | 1,442 | 3,587 | - | - | - |
| 32 MiB | par2 | 2,088 | 2,233 | 1,435 | 2,271 | 41 | 1,963 | 68 |
| 32 MiB | par4 | 1,840 | 2,066 | 1,454 | 2,164 | 39 | 1,625 | 75 |
| 32 MiB | par6 | **1,598** | 2,279 | 1,309 | 2,555 | 39 | 1,482 | 80 |
| 32 MiB | seq4 (control) | 4,600 | 4,810 | 4,018 | 6,411 | 39 | 4,487 | 72 |

Reading: here the path is the bottleneck and one TCP stream does not fill it, so several parallel
connections win at 32 MiB (par4 34% and par6 42% below single) and par3 wins 18% at 16 MiB, while par2
at 16 MiB loses (the 16 MiB single-PUT medians sit on a bimodal distribution, 750 vs 1,400 ms). This
measures "more connections on a Wi-Fi path", not the provider; the same gain could come from sending
two segments as two parallel PUTs. Create is 39 ms (one RTT pair) and Complete 66-80 ms.

### Aggregate goodput, 8 objects in flight

One run per cell (AWS 403 MB per cell, OVH about 320-335 MB per cell). Requests per GB use 1 GB =
10^9 B and include the Create and Complete requests.

| provider | size | form | objects | wall (ms) | MB/s | requests | requests per GB |
|---|---|---|---|---|---|---|---|
| same region (us-west-2) | 16 MiB | single PUT | 24 | 753 | 535 | 24 | 60 |
| same region (us-west-2) | 32 MiB | single PUT | 12 | 723 | 557 | 12 | 30 |
| same region (us-west-2) | 32 MiB | par4 (32 parts in flight) | 12 | 774 | 520 | 72 | 179 |
| Hungary → Milan (Wi-Fi client) | 16 MiB | single PUT | 19 | 11,200 | 28.5 | 19 | 60 |
| Hungary → Milan (Wi-Fi client) | 32 MiB | single PUT | 10 | 12,121 | 27.7 | 10 | 30 |
| Hungary → Milan (Wi-Fi client) | 32 MiB | par4 (32 parts in flight) | 10 | 10,142 | 33.1 | 60 | 179 |

On AWS, 8 objects in flight already saturate what the client can push (about 0.53 GB/s, CPU and NIC
of the test host): multipart gives no gain (520 vs 557 MB/s, within the spread of a 0.75 s cell; one
run each, so differences under about 10% are not resolved). Per-object time with 8 in flight (p50):
16 MiB single 209 ms, 32 MiB single 365 ms, 32 MiB par4 370 ms. On the Wi-Fi path the link caps at
about 31-33 MB/s: multipart 33.1 vs 27.7-28.5 MB/s for single PUTs (+16-19%, one run each), i.e. the
single-PUT cells did not quite fill the link.

### Request cost per GB uploaded

S3 Standard list prices: PUT, COPY, POST and LIST requests $0.005 per 1,000; GET $0.0004 per 1,000;
DELETE and abort free ([AWS S3 pricing](https://aws.amazon.com/s3/pricing/): the fetched page showed
the LIST-as-PUT and free-DELETE statements but not the table, so the $0.005 figure is confirmed from
secondary pages that quote it for us-east-1; check the live page for us-west-2). CreateMultipartUpload
and CompleteMultipartUpload are POSTs and each UploadPart is a PUT, all in that tier. OVH states "no
hidden fees for ingress/egress or API calls" on its Object Storage product page (storage capacity and
restored data are billed; the price table itself was not read), so request count costs nothing there.

| form | requests per object | requests per GB | request cost per GB | per TB |
|---|---|---|---|---|
| 16 MiB single | 1 | 60 | $0.00030 | $0.30 |
| 16 MiB par2 | 4 | 238 | $0.00119 | $1.19 |
| 16 MiB par3 | 5 | 298 | $0.00149 | $1.49 |
| 32 MiB single | 1 | 30 | $0.00015 | $0.15 |
| 32 MiB par2 | 4 | 119 | $0.00060 | $0.60 |
| 32 MiB par4 | 6 | 179 | $0.00089 | $0.89 |
| 32 MiB par6 | 8 | 238 | $0.00119 | $1.19 |

Multipart is 4-5 times the request cost, but the absolute amounts are tiny (cents per 10 TB). The
cost to watch is not the request fee but an abandoned upload: parts of an unfinished upload stay
stored (and billed) until aborted; a lifecycle abort rule is a bucket setting this probe did not and
may not touch, so a `flush_segment` that used multipart would have to abort on every failure path.

### Verdict

- **16 MiB segments: single PUT.** On AWS it is the fastest (194 vs 249-252 ms for 2-3 parts) and the
  cheapest in requests; multipart's fixed 115 ms exceeds what parallel parts save at this size. The
  5 MiB part minimum also caps useful parallelism at 3 parts. On the Wi-Fi/OVH path 3 parallel parts
  were 18% faster at p50 but 2 parts were not, and the gain comes from using more connections, which
  concurrent segment PUTs already provide.
- **Larger objects: multipart starts to pay from about 25-32 MiB** (AWS: par4 21% faster at 32 MiB;
  OVH from this client: 34-42% faster). Use parts of 8 MiB or more, 4 parts; more parts do not help.
  Not measured: 64 MiB (the optional cell was **not run: it needs 0.64 GB and the required cells left 0.44 GB under the 4 GB cap**), part counts above 6, multipart under a shaped link.
- **Aggregate throughput at concurrency 8: no reason for multipart** on AWS (both about 0.53 GB/s);
  on a link-bound client it adds up to 19% by filling the pipe, which more concurrent PUTs would also do.
- Multipart is still a valid fencing commit (conditional Complete, see above), but it is not cheaper on
  the flush path (Create + Complete = 3 more round trips).
- Caveats: AWS n = 10, OVH n = 12 per cell; one goodput run per cell (0.7-0.8 s on AWS, so noisy); the OVH
  path varies in time (single 16 MiB PUT 750 to 1,550 ms) and its numbers describe this Wi-Fi client.

Runs for this section: AWS same region 3,559 MB uploaded in 751 requests (probe counter; check run
2 MB, 24 requests, plus 727 for the main run); OVH 3,810 MB in 838 requests over three runs (check,
main, second latency run). Multipart uploads created / completed / aborted: AWS 83 / 82 / 1, OVH
95 / 94 / 1 (the one abort in each is the deliberate 5 MiB check). Leftover objects 0 and open uploads
0 in every run, and an independent `--verify` pass at the end listed 0 objects and 0 open uploads
under `lunix-p0-probe/` in both buckets.

## Deviations from the plan and from the request

- **Output path.** Written to `bench/s3probe/` as the task asked; the plan text says
  `bench/s3-probe/`. Rename if the plan's spelling is wanted.
- **Matrix trimmed to the byte cap.** To keep each run under about 3 GB the `seq` suite ran 16 MiB
  PUT/GET n = 3 (not 40/16) and parallel 16 MiB with 1 / 8 / 4 objects at concurrency 1 / 8 / 32; the
  16 MiB latency is carried by the `sizes` suite (n = 12) instead. MinIO (earlier sweep) used the
  larger numbers, so its 16 MiB rows are not comparable in n.
- **Lane bulk is unthrottled.** The review text names "a 200 MB/s upload"; the probe ran 8 concurrent
  16 MiB uploads at whatever the path gave (about 0.5 GB/s on AWS same region, 31 MB/s from
  Hungary), because shaping needs a rate limiter that the byte cap made hard to use. The AWS lane
  samples are few (n about 45 per cell pooled over three runs), so only p50 is resolved.
- **"Idle and 8 parallel" per size** (review table) was not run; only 16 MiB parallel aggregate.
  **S3 Express One Zone and R2** were not available.
- **Extra runs.** AWS same region ran three times (T+0, T+31, T+35), the last two without the
  semantic sweep, and OVH three times; the OVH second run was degraded (see above) and a third was
  added. AWS from Hungary ran once with a smaller lane (12 objects per phase) and without the
  `seq` suite so that GET volume stayed near 40 MB.
- **Per-sample cap.** `CAP_REQS` and `CAP_BYTES` were raised from 5,000 / 2 GB to 9,000 / 3 GB as the
  request allowed.
- **Conditional Complete** uses a hand-signed SigV4 client (`src/raw.rs`, `sha2` + `hmac` + `reqwest`,
  pinned) because `object_store` 0.14.1 cannot send `If-None-Match` on Complete except inside its
  copy-if-not-exists path. Complete responses are checked for an `<Error>` inside a 200 body.
- **No reuse of Constellation code.** The status classification is re-implemented in about 60 lines in
  `spikes/s3probe/src/main.rs`; `object_store` is pinned to the same 0.14.1.
- **Clock skew** uses the Last-Modified of a fresh object (1 s resolution); `object_store` does not
  expose `Date`.
- **TLS set-up** is approximated as cold-HEAD minus warm-HEAD minus TCP connect from a single sample
  each; it is noisy and negative in one run. Not a result.
- **MinIO image** is a Chainguard build of upstream MinIO, single-drive on a bind mount.
- **Extra (MinIO):** a second bucket with versioning enabled (`tools/enable_versioning.py`, run
  against the throwaway MinIO only, never against AWS or OVH).
- Torn-read check uses 64 KiB uniform-byte bodies with two concurrent readers per swap round; 0 found.
- The two AWS conditional-Complete races that each had one 5xx loser did not record the error code;
  the third did and saw none. Not explained.
- OVH run 1's `seq` phase (4 KiB PUT 90 ms, GET 78 ms against HEAD 29 ms) was slower than its later
  `sizes` phase (4 KiB PUT 40 ms); the fluctuation is of the same kind as the degraded run 2.

## Not run

Cloudflare R2, Google Cloud Storage, Backblaze B2, Scaleway, Wasabi, Hetzner, Tigris, DigitalOcean
Spaces, S3 Express One Zone, Ceph RGW: **not run: no credentials**. Nothing about them was measured;
any statement about them is from the fact-check note, which is secondary evidence for several of
them. Also not run: MinIO conditional Complete in the full sweep (smoke only), the 8-parallel-per-size
cell, a throttled 200 MB/s lane.

## Cleanup and leftover check (remote)

- MinIO (earlier sweep): container `lunix-p0-minio` removed at the end of each run and the image
  removed; a later loopback smoke of the new suites re-pulled the image and the script removed the
  container and data directory again (no `lunix-p0` container remains).
- Objects: the probe lists and deletes only `lunix-p0-probe/<run-id>/`; leftover objects 0 on every run
  (see "Cleanup and safety" above for AWS and OVH).
- Host: no `s3probe` process, no output directory; build artefacts under `~/lunix-p0/s3probe/target`
  (about 700 MB, includes the musl target) remain until the lead removes them. Disk use well inside the
  2 GiB budget apart from that build cache.
- Credentials: environment only; the outputs and sources were grepped for the host's addresses,
  user name, hostnames, bucket names, account ids and key prefixes before copying (none found).

## Open questions for the maintainer

1. R2 credentials to complete the launch-provider matrix (and S3 Express One Zone).
2. A Ceph RGW target on a *versioned* bucket would exercise the one provider where ADR-2's
   versioning rule matters; OVH's mixed signal (version ids returned, no bucket-level status) is
   worth asking OVH about.
3. Should `lunix doctor` run this whole sweep or a reduced "quick" form (suggested: 5 create
   rounds, 20 swap rounds, no 16 MiB transfers, about 300 requests)? The gate rule was written
   for the full form.
4. The lane test should be repeated with a shaped 200 MB/s bulk on a bandwidth-limited NIC and
   with enough samples for p99 before the `flush_segment` default depends on lane isolation.

## Proposed decision record (for the lead to merge; do not apply here)

Append under ADR-2 in `docs/explanation/DECISIONS.md`:

> **Gate outcome (2026-11-12, P0.10):** `spikes/s3probe` ran the authority probes against AWS S3
> Standard (three full sweeps, from the same region and from another continent), OVH Object Storage
> Milan (one full sweep) and MinIO `RELEASE.2026-11-10T21-18-11Z`. All three meet the authority rule:
> create-only PUT answers 412 on an existing key; If-Match swap and stale If-Match (412); 20/20
> 8-way create races and 100/100 8-way swap races with exactly one winner and no torn read;
> read-your-write by GET and LIST. **AWS S3 and OVH are confirmed authority backends.** Both also
> accept `If-None-Match: *` on `CompleteMultipartUpload` (create-only; 412 when the key exists;
> exactly one winner of an 8-way race, 70/70 rounds on AWS, 50/50 on OVH), so a multipart commit
> is a valid fencing primitive. Both return 409 `ConditionalRequestConflict` for concurrent
> If-Match swaps (hundreds per 100-round race), so a client must retry on 409. OVH returns version
> ids on PUT while reporting no bucket versioning status; the probe passes on it as served.
> Cloudflare R2 remains unverified (no credentials). Evidence: `bench/s3probe/RESULTS.md`.

Numbers for DESIGN §20 / the fsync-scope note (concurrency 1, create-only or plain PUT):

> AWS S3 Standard, same region: 4 KiB PUT p50 29.7 ms, p99 about 45-67 ms; 256 KiB 45-49 ms; 1 MiB
> 70-78 ms; 16 MiB 205-227 ms; p50 fit 42 ms + 10.6 ms/MiB. Two serial 4 KiB create-only PUTs: p50
> 54 ms, p99 94 ms (n=200); a 1 MiB data PUT then a 4 KiB commit PUT: p50 81 ms, p99 133-189 ms.
> OVH Milan from Hungary (RTT 20 ms): 4 KiB 39.5 ms, 1 MiB 130-138 ms, two serial 4 KiB PUTs 97 ms,
> 1 MiB + 4 KiB 192 ms; fit 71 ms + 53 ms/MiB (Wi-Fi-bound); OVH latency was 2-2.6 times worse in
> one run 37 minutes after the first. Conditional or unconditional `CompleteMultipartUpload` of 1-4
> parts under 20 MiB is one request of about 87 ms (AWS) or 76 ms (OVH), about 3 and 2 times a 4 KiB PUT,
> so the multipart route is not cheaper on the flush path. Lane isolation: with 8 concurrent 16 MiB
> uploads, a 4 or 64 KiB PUT stays within about 10% of idle at p50 on both AWS (0.5 GB/s) and a
> saturated 31 MB/s Wi-Fi link, with or without a separate client; p99 unresolved.

Multipart vs single PUT (P0.10 addendum, measured 2026-11-12; ADR-7 and ADR-28):

> **Segment upload form:** keep one PUT per 16 MiB segment in `flush_segment`. Same region (us-west-2),
> n = 10: 16 MiB single PUT p50 194 ms (p90 216) versus 249-252 ms for 2-3 parallel parts, because
> CreateMultipartUpload (about 23 ms) and CompleteMultipartUpload (about 90 ms) cost more than the
> parallel parts save; S3 and OVH both reject non-final parts under 5 MiB, so 16 MiB can use at most 3
> parts. At 32 MiB, 4 parallel 8 MiB parts win by 21% (265 vs 336 ms; break-even about 25 MiB); at 8
> objects in flight there is no aggregate gain (about 0.53 GB/s either way). Multipart costs 4-6
> requests per object instead of 1 ($0.0009 vs $0.00015 per GB for 32 MiB objects at $0.005 per 1,000
> PUT/POST), none on OVH. If segments grow beyond about 32 MiB, use 4 parts of at least 8 MiB, abort
> on every failure path, and note that parallel segment PUTs reach the same goal without the extra
> round trips. From the Wi-Fi client to OVH, parallel connections were 18-42% faster (the link, not the
> provider, is the limit). Evidence: `bench/s3probe/RESULTS.md`, "Multipart vs single PUT".
