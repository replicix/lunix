# s3probe (P0.10)

Provider-agnostic probe of the S3 conditional-write protocol that Lunix's commit chain and
lease need (ADR-2, DESIGN §9 `doctor`), plus PUT latency and throughput. Spike code.

Reproduce (test host only): `cargo build --release && ./run-minio.sh [--suites ...]` (throwaway
local MinIO in docker, 127.0.0.1 only). Any endpoint, from the environment only (nothing
hardcoded or printed): `LUNIX_PROBE_<NAME>_BUCKET`, `_ENDPOINT` (omit for AWS), `_REGION`,
`_PATH_STYLE=1`, `_ALLOW_HTTP=1`, `_LABEL`, `_ACCESS_KEY_ID`/`_SECRET_ACCESS_KEY`/`_SESSION_TOKEN`
(else `AWS_*`), then `s3probe --out DIR [--providers AWS,OVH] [--suites sem,seq,cond,sizes,pairs,lane,net]`.
Profiles are not resolved: pipe `aws configure export-credentials --format env-no-export` into the
process environment. Static binary for another machine: `--target x86_64-unknown-linux-musl`.

Suites (all by default). `sem`: create-if-absent and If-Match (412, 404), 8-way create and swap
races, ranged GET, read-your-write, multipart create/abort, versioning state (read only), clock
skew. `cond`: hand-signed (`src/raw.rs`) `CompleteMultipartUpload` with `If-None-Match: *`
(absent, existing, 8-way race) and Complete latency for 1/2/4 parts. `seq`: 4 KiB/64 KiB/16 MiB
GET/PUT, parallel 16 MiB, HEAD, LIST. `sizes`: PUT latency 4 KiB..16 MiB with a fitted fixed cost
and slope. `pairs`: two serial create-only PUTs. `lane`: small PUTs during a 16 MiB x 8 bulk
upload, separate client versus the same client. `net`: TCP connect and TLS estimate.

Everything lives under `lunix-p0-probe/<run-id>/`; only that prefix is deleted, open multipart
uploads under it are aborted at the end. Caps: 9,000 requests and 3 GB per run; tune with
`LUNIX_PROBE_*_N`, `_LANE_OBJS`, `_LANE_ALONE_OBJS`, `_CCRACE_ROUNDS`. `<provider>-samples.csv`
holds every sample; `tools/pool.py` recomputes percentiles and fits. `cargo test` runs the harness
on an in-memory store. Constellation's classification is re-implemented (about 60 lines).
`s3probe mpu --out DIR --providers AWS [--phases check,lat,gp,opt64] [--verify|--sweep]`: multipart vs
single PUT, 16/32 MiB (`src/mpu.rs`): `check` 5 MiB part minimum, `lat` per-object latency
(`LUNIX_PROBE_MPU_N`), `gp` goodput at 8 in flight (`_GP16`, `_GP32`), `opt64`. `--verify` counts
objects and open uploads under `lunix-p0-probe/`, `--sweep` aborts them; `tools/mpu_summary.py`.
