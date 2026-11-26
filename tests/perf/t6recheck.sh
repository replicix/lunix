#!/bin/bash
# T6 fastrecheck A/B driver (root, on the test host, through tools/remote.sh root; never run locally).
#
# Runs the blocks of the interleaved design one after the other, each a fresh daemon and a fresh volume
# (tests/perf/partA.sh with CHUNKS set to one chunk size and QUEUES_LIST=4), with `throttle_pace_store_bytes = true` in
# both arms and `gc_fast_recheck` the only difference. After each block the result directory is copied to
# $RESDIR/<tag> (hostinfo files left out) and the tmpfs copy removed.
#
# Block tag: b<N>-<arm>-<chunk bytes>-<set>; set main = w4k + seqw, ctl = w4k only, zipf = the skewed cell (w4kz) only.
#
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR (a tmpfs, LUNIX_ALLOW_TMPFS_FOR_TESTS is set here) RESDIR (disk).
#   BLOCKS   override of the block list (words "tag:arm:chunk:cells"), for a partial re-run
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIXD:?}" "${LUNIX:?}" "${TESTD:?}" "${LUNIX_SPIKE_DIR:?}" "${RESDIR:?}"
export LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR LUNIX_ALLOW_TMPFS_FOR_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_ROOT_TESTS=1
export LUNIX_MIN_FREE_GIB=2 DF_GIB=1
mkdir -p "$RESDIR"

: "${BLOCKS:=b1-off-16384-ctl:off:16384:w4k b2-on-16384-ctl:on:16384:w4k b3-on-16384-ctl:on:16384:w4k b4-off-16384-ctl:off:16384:w4k b5-off-65536-main:off:65536:w4k,seqw b6-on-65536-main:on:65536:w4k,seqw}"

loadwait() { # up to $1 s until the 1-minute load is below $2 (and, with $3 = 5, the 5-minute load below 2)
  local i l1 l5
  for i in $(seq 1 "$1"); do
    read -r l1 l5 _ < /proc/loadavg
    if awk -v a="$l1" -v b="$l5" -v m="$2" -v w="${3:-1}" 'BEGIN{exit !(a < m && (w != 5 || b < 2.0))}'; then return 0; fi
    sleep 1
  done
  return 1
}

for b in $BLOCKS; do
  IFS=: read -r tag arm chunk cells <<< "$b"
  case "$arm" in on) fr=true ;; off) fr=false ;; *) echo "bad arm in $b" >&2; exit 2 ;; esac
  # every block starts with the 1-minute load below 1 and the 5-minute load below 2 (the load of the previous block decays slowly)
  loadwait 5400 1.0 5 || { echo "$tag: load did not fall below 1 / 2 within 90 min; stopping"; exit 4; }
  read -r l1 l5 l15 _ < /proc/loadavg
  OUT="$LUNIX_SPIKE_DIR/t6r-$tag"
  echo "== $tag arm=$arm gc_fast_recheck=$fr chunk=$chunk cells=$cells load=$l1/$l5/$l15 at $(date -u +%FT%TZ)"
  rm -rf "${RESDIR:?}/$tag"
  env OUT="$OUT" CHUNKS="$chunk" QUEUES_LIST=4 REPS="${REPS:-5}" WS_GIB=2 VOL_GIB=16 W_RUNTIME="${W_RUNTIME:-60}" RUNTIME="${RUNTIME:-30}" \
      SKIP_READ_CELLS=1 VERIFY_READS=0 SAMPLE=1 STATUS_PROBE=1 NULLBLK=0 PERF=0 CELLS="${cells//,/ }" \
      DESTAGE_WORKERS=8 LOG_LEVEL=debug \
      CFG_EXTRA="throttle_pace_store_bytes = true
gc_fast_recheck = $fr" \
      bash "$here/partA.sh"
  rc=$?
  [ -d "$OUT" ] || { echo "$tag: partA.sh left no result directory (rc=$rc); stopping"; exit 3; }
  mkdir -p "$RESDIR/$tag"
  rsync -a --exclude 'hostinfo*' "$OUT/" "$RESDIR/$tag/"
  printf '%s %s %s\n' "$l1" "$l5" "$l15" > "$RESDIR/$tag/loadavg-block-start.txt"
  echo "$rc" > "$RESDIR/$tag/driver-rc.txt"
  echo "== $tag done rc=$rc"
  [ -s "$RESDIR/$tag/result.txt" ] && rm -rf "$OUT"
done
echo "== all blocks done $(date -u +%FT%TZ)"
