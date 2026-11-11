#!/bin/sh
# P0.10: run the probe against a throwaway local MinIO (docker), then remove everything this
# script created. Runs on the test host only. Credentials are generated here, passed through an
# env file that is deleted immediately, and never written to the repo or printed.
#
#   MINIO_IMAGE   image to use (default: a Chainguard MinIO build; the upstream Docker Hub and
#                 quay.io images are no longer pullable)
#   OUT           output directory for the CSV/TXT reports (default ./out)
#   CPUS          CPU set for the container and the probe (default 8-15)
set -eu
cd "$(dirname "$0")"
IMAGE=${MINIO_IMAGE:-cgr.dev/chainguard/minio:latest}
OUT=${OUT:-out}
CPUS=${CPUS:-8-15}
NAME=lunix-p0-minio
PORT=19000
DATA="$HOME/lunix-p0/s3probe-minio-data"
BUCKET=lunix-p0
VBUCKET=lunix-p0-ver   # second bucket, versioning enabled after start

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$DATA"
  [ -n "${ENVF:-}" ] && rm -f "$ENVF"
}
trap cleanup EXIT INT TERM

docker ps -a --format '{{.Names}}' | grep -qx "$NAME" && { echo "container $NAME already exists; refusing" >&2; exit 1; }
mkdir -p "$DATA/$BUCKET" "$DATA/$VBUCKET"   # a top-level directory is a bucket for a single-drive MinIO

umask 077
ENVF=$(mktemp)
AK=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
SK=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
printf 'MINIO_ROOT_USER=%s\nMINIO_ROOT_PASSWORD=%s\n' "$AK" "$SK" > "$ENVF"
docker run -d --name "$NAME" --user "$(id -u):$(id -g)" --cpuset-cpus "$CPUS" --memory 2g \
  -p "127.0.0.1:$PORT:9000" --env-file "$ENVF" -v "$DATA:/data" "$IMAGE" server /data --address :9000 >/dev/null
rm -f "$ENVF"; ENVF=

i=0
until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/minio/health/live" || true)" = 200 ]; do
  i=$((i + 1)); [ "$i" -gt 60 ] && { echo "MinIO did not become healthy" >&2; exit 1; }
  sleep 1
done
docker exec "$NAME" /usr/bin/minio --version 2>/dev/null | head -1 || docker run --rm "$IMAGE" --version | head -1

ENDPOINT="http://127.0.0.1:$PORT" BUCKET="$VBUCKET" AK="$AK" SK="$SK" python3 -I tools/enable_versioning.py

export LUNIX_PROBE_MINIO_ENDPOINT="http://127.0.0.1:$PORT"
export LUNIX_PROBE_MINIO_BUCKET="$BUCKET"
export LUNIX_PROBE_MINIO_REGION=us-east-1
export LUNIX_PROBE_MINIO_PATH_STYLE=1
export LUNIX_PROBE_MINIO_ALLOW_HTTP=1
export LUNIX_PROBE_MINIO_LABEL="local docker, loopback"
export LUNIX_PROBE_MINIO_ACCESS_KEY_ID="$AK"
export LUNIX_PROBE_MINIO_SECRET_ACCESS_KEY="$SK"
# the same server, the bucket with versioning enabled (provider name MINIOVER)
export LUNIX_PROBE_MINIOVER_ENDPOINT="$LUNIX_PROBE_MINIO_ENDPOINT" LUNIX_PROBE_MINIOVER_BUCKET="$VBUCKET"
export LUNIX_PROBE_MINIOVER_REGION=us-east-1 LUNIX_PROBE_MINIOVER_PATH_STYLE=1 LUNIX_PROBE_MINIOVER_ALLOW_HTTP=1
export LUNIX_PROBE_MINIOVER_LABEL="local docker, loopback, versioned bucket"
export LUNIX_PROBE_MINIOVER_ACCESS_KEY_ID="$AK" LUNIX_PROBE_MINIOVER_SECRET_ACCESS_KEY="$SK"
# other providers are skipped unless the caller exported their variables
taskset -c "$CPUS" ./target/release/s3probe --out "$OUT"
