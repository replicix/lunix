#!/bin/bash
# refsoak driver (root, on the test host, through tools/remote.sh root; never run locally): the six soak cells and the cost blocks,
# one after the other, each a fresh daemon and volume, each started only when the host is quiet (1-minute load below 1, 5-minute
# load below 2). After each block the result directory is copied to $RESDIR/<tag> (hostinfo files left out) and the tmpfs copy removed.
#
# Every arm: throttle_pace_store_bytes = true, gc_fast_recheck = true, log_level debug, no pins or snapshots.
# Block word: tag:kind:chunk:mode
#   kind c1..c6: a soak cell (tests/perf/refsoak-cells.sh), mode = shadow
#   kind cost:   the cost check (tests/perf/partA.sh, w4k, 3 repetitions, 4 queues), mode = shadow | off  (gc_condemn_on_cut)
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR (a tmpfs) RESDIR (disk); BLOCKS overrides the list.
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIXD:?}" "${LUNIX:?}" "${TESTD:?}" "${LUNIX_SPIKE_DIR:?}" "${RESDIR:?}"
export LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR LUNIX_ALLOW_TMPFS_FOR_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_ROOT_TESTS=1
export LUNIX_MIN_FREE_GIB=2 DF_GIB=1
mkdir -p "$RESDIR"
: "${BLOCKS:=s1-c1-65536:c1:65536:shadow s2-c2-16384:c2:16384:shadow s3-c3-65536:c3:65536:shadow s4-c4-65536:c4:65536:shadow s5-c5-65536:c5:65536:shadow s6-c6-65536:c6:65536:shadow}"

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
  loadwait 5400 1.0 || { echo "$tag: load did not fall below 1 / 2 within 90 min; stopping"; exit 4; }
  read -r l1 l5 l15 _ < /proc/loadavg
  OUT="$LUNIX_SPIKE_DIR/rs-$tag"
  echo "== $tag kind=$kind chunk=$chunk mode=$mode load=$l1/$l5/$l15 at $(date -u +%FT%TZ)"
  rm -rf "${RESDIR:?}/$tag"
  common="throttle_pace_store_bytes = true
gc_fast_recheck = true
gc_condemn_on_cut = \"$mode\""
  case "$kind" in
    cost)
      env OUT="$OUT" CHUNKS="$chunk" QUEUES_LIST=4 REPS=3 WS_GIB=2 VOL_GIB=16 W_RUNTIME=60 RUNTIME=30 SAMPLE_S=2 \
          SKIP_READ_CELLS=1 VERIFY_READS=0 SAMPLE=1 STATUS_PROBE=1 NULLBLK=0 PERF=0 CELLS="w4k" \
          DESTAGE_WORKERS=8 LOG_LEVEL=debug CFG_EXTRA="$common" bash "$here/partA.sh"
      rc=$? ;;
    c*)
      env OUT="$OUT" CELL="$kind" CHUNK="$chunk" REPS=5 LOG_LEVEL=debug CFG_EXTRA="$common" \
          RESTART_REP="$([ "$kind" = c1 ] && echo 3 || echo 0)" bash "$here/refsoak-cells.sh"
      rc=$? ;;
    *) echo "bad kind in $b" >&2; exit 2 ;;
  esac
  [ -d "$OUT" ] || { echo "$tag: no result directory (rc=$rc); stopping"; exit 3; }
  mkdir -p "$RESDIR/$tag"
  rsync -a --exclude 'hostinfo*' "$OUT/" "$RESDIR/$tag/"
  printf '%s %s %s\n' "$l1" "$l5" "$l15" > "$RESDIR/$tag/loadavg-block-start.txt"
  echo "$rc" > "$RESDIR/$tag/driver-rc.txt"
  echo "== $tag done rc=$rc"
  [ -s "$RESDIR/$tag/result.txt" ] && rm -rf "$OUT"
done
echo "== all blocks done $(date -u +%FT%TZ)"
