#!/bin/bash
# c5t6 driver (root, on the test host; never run locally): the blocks of the real-daemon T6 of active C5, one after the other, each a fresh
# daemon and volume (tests/perf/c5t6-cells.sh), each started only when the host is quiet (1-minute load below 1, 5-minute load below 2).
# After each block the result directory is copied to $RESDIR/<tag> (hostinfo files left out) and the tmpfs copy removed.
#
# Every arm: throttle_pace_store_bytes = true, gc_fast_recheck = true, log_level debug. No arm C (5 s pressure grace).
# Block tag  bNN-<arm>-<chunk bytes>-<kind>,  words "tag:kind:chunk:mode":
#   arm  off | A (shadow) | B (on, normal 30 s grace floor)         (gc_condemn_on_cut = off | shadow | on)
#   kind w4k | w4kz (zipf:1.2) | restart (w4k, one clean daemon restart in rep 3) | pin (w4k on a plain volume, snapshot pin held)
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR (a tmpfs) RESDIR (disk); BLOCKS overrides the list.
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIXD:?}" "${LUNIX:?}" "${TESTD:?}" "${LUNIX_SPIKE_DIR:?}" "${RESDIR:?}"
export LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR LUNIX_ALLOW_TMPFS_FOR_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_ROOT_TESTS=1
export LUNIX_MIN_FREE_GIB=2 DF_GIB=1
mkdir -p "$RESDIR"
: "${BLOCKS:=b01-A-65536-w4k:w4k:65536:shadow b02-B-65536-w4k:w4k:65536:on b03-B-65536-w4k:w4k:65536:on b04-A-65536-w4k:w4k:65536:shadow b05-off-65536-w4k:w4k:65536:off b06-B-65536-w4k:w4k:65536:on b07-A-65536-w4k:w4k:65536:shadow b08-A-65536-w4k:w4k:65536:shadow b09-B-65536-w4k:w4k:65536:on b10-A-16384-w4k:w4k:16384:shadow b11-B-16384-w4k:w4k:16384:on b12-off-16384-w4k:w4k:16384:off b13-B-16384-w4k:w4k:16384:on b14-A-16384-w4k:w4k:16384:shadow b15-A-65536-w4kz:w4kz:65536:shadow b16-B-65536-w4kz:w4kz:65536:on b17-B-65536-w4kz:w4kz:65536:on b18-A-65536-w4kz:w4kz:65536:shadow b19-B-65536-restart:restart:65536:on b20-B-65536-pin:pin:65536:on}"

loadwait() { # up to $1 s until the 1-minute load is below $2 and the 5-minute load below 2
  local i l1 l5
  for i in $(seq 1 "$1"); do
    read -r l1 l5 _ < /proc/loadavg
    if awk -v a="$l1" -v b="$l5" -v m="$2" 'BEGIN{exit !(a < m && b < 2.0)}'; then return 0; fi
    sleep 1
  done
  return 1
}

for b in $BLOCKS; do
  IFS=: read -r tag kind chunk mode <<< "$b"
  [ "${C5_SKIP_LOADWAIT:-0}" = 1 ] || loadwait 5400 1.0 || { echo "$tag: load did not fall below 1 / 2 within 90 min; stopping"; exit 4; }
  read -r l1 l5 l15 _ < /proc/loadavg
  OUT="$LUNIX_SPIKE_DIR/c5-$tag"
  echo "== $tag kind=$kind chunk=$chunk mode=$mode load=$l1/$l5/$l15 at $(date -u +%FT%TZ)"
  rm -rf "${RESDIR:?}/$tag"
  common="throttle_pace_store_bytes = true
gc_fast_recheck = true
gc_condemn_on_cut = \"$mode\""
  cell=$kind; rr=0; vm=integrity
  [ "$kind" = restart ] && { cell=w4k; rr="${RESTART_REP_OVERRIDE:-3}"; }
  [ "$kind" = pin ] && vm=plain
  env OUT="$OUT" CELL="$cell" CHUNK="$chunk" REPS="${REPS:-5}" W_RUNTIME="${W_RUNTIME:-60}" LOG_LEVEL=debug CFG_EXTRA="$common" RESTART_REP="$rr" VOLMODE="$vm" \
      bash "$here/c5t6-cells.sh"
  rc=$?
  [ -d "$OUT" ] || { echo "$tag: no result directory (rc=$rc); stopping"; exit 3; }
  mkdir -p "$RESDIR/$tag"
  rsync -a --exclude 'hostinfo*' "$OUT/" "$RESDIR/$tag/"
  printf '%s %s %s\n' "$l1" "$l5" "$l15" > "$RESDIR/$tag/loadavg-block-start.txt"
  echo "$rc" > "$RESDIR/$tag/driver-rc.txt"
  echo "== $tag done rc=$rc"
  [ -s "$RESDIR/$tag/result.txt" ] && rm -rf "$OUT"
done
echo "== all blocks done $(date -u +%FT%TZ)"
