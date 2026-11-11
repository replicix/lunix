# s3probe (P0.10)

Provider-agnostic probe of the S3 conditional-write protocol that Lunix's commit chain and
lease need (ADR-2, DESIGN §9 `doctor`), plus PUT/GET latency and throughput. Spike code.

Reproduce (test host only): `cargo build --release && ./run-minio.sh` (throwaway local MinIO
in docker, bound to 127.0.0.1, container `lunix-p0-minio`, everything removed at exit).

Any other endpoint, from the environment only (nothing is hardcoded or printed):
`LUNIX_PROBE_<NAME>_BUCKET`, `_ENDPOINT` (omit for AWS), `_REGION`, `_PATH_STYLE=1`,
`_ALLOW_HTTP=1`, `_LABEL`, and `_ACCESS_KEY_ID`/`_SECRET_ACCESS_KEY` (else `AWS_*`), then
`./target/release/s3probe --out DIR [--providers AWS,R2]`. Unset providers print
"not run: no credentials". AWS profiles are not resolved: export the credentials.

Probes: create-if-absent and If-Match (412, 404 on a deleted key), 8-way concurrent create
(20 rounds) and swap (100 rounds, with torn-read check), ranged GET, read-your-write by
GET and LIST, multipart create/abort, versioning state, clock skew, 4 KiB/64 KiB/16 MiB
sequential PUT/GET with TTFB, 16 MiB parallel at 1/8/32, HEAD, 10-key LIST.
Everything lives under `lunix-p0-probe/<run-id>/` and only that prefix is deleted.
Defaults stay under ~5,000 requests and ~2 GB per provider; tune with `LUNIX_PROBE_*_N`.

Deviations: Constellation's `cas.rs`/`probe.rs` classification is re-implemented (about 60
lines) instead of a path dependency, which would pull its whole workspace into the build.
Clock skew uses the object's Last-Modified (object_store does not expose `Date`).
`cargo test` runs the harness against an in-memory store (no network).
