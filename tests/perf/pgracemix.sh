#!/bin/bash
# pgracemix driver (root, on the test host; never run locally): blocks of the mixed 70 % write / 30 % read cell (w4kmix.fio) under the short segment grace,
# arms C (gc_pressure_grace_ms = 5000) and C2 (2000), each otherwise as arm C of pgracet6 (gc_condemn_on_cut on, gc_fast_recheck on,
# throttle_pace_store_bytes on, log_level debug). One fresh daemon and volume per block (tests/perf/c5t6-cells.sh, CELL=w4kmix); the store is a tmpfs
# mounted here and unmounted at the end. A block starts when the host is quiet (cpu and io pressure, see loadwait).
# Block tag  bNN-<arm>-<chunk bytes>-w4kmix ; BLOCKS words "tag:chunk:grace_ms". Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR RESDIR; BLOCKS, DIST, TMPFS_GIB.
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIXD:?}" "${LUNIX:?}" "${TESTD:?}" "${LUNIX_SPIKE_DIR:?}" "${RESDIR:?}"
: "${TMPFS_GIB:=13}" "${LOADWAIT_S:=3600}"
STORE="$LUNIX_SPIKE_DIR/store"
mkdir -p "$RESDIR" "$STORE"
mountpoint -q "$STORE" || mount -t tmpfs -o "size=${TMPFS_GIB}g" tmpfs "$STORE" || exit 5
trap 'umount "$STORE" 2>/dev/null' EXIT
export LUNIX_SPIKE_DIR="$STORE"
export LUNIXD LUNIX TESTD LUNIX_ALLOW_TMPFS_FOR_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_ROOT_TESTS=1 LUNIX_MIN_FREE_GIB=2 DF_GIB=1
: "${BLOCKS:=b01-C-65536:5000 b02-C2-65536:2000 b03-C2-65536:2000 b04-C-65536:5000}"

# The load average of this host stays high for tens of minutes after a block (the io_uring workers of fio count as load while they run), so the host is judged
# quiet by pressure-stall information instead: 10 s averages of cpu and io "some" below 1 %, twice 10 s apart, and nothing but this script running fio or lunixd.
psi10() { awk '/^some/{split($2,a,"="); print a[2]}' "/proc/pressure/$1"; }
loadwait() {
  local i ok=0
  for i in $(seq 1 "$LOADWAIT_S"); do
    if awk -v c="$(psi10 cpu)" -v d="$(psi10 io)" 'BEGIN{exit !(c < 1.0 && d < 1.0)}' && ! pgrep -x 'fio|lunixd' > /dev/null; then
      ok=$((ok + 1)); [ "$ok" -ge 10 ] && return 0
    else ok=0; fi
    sleep 1
  done
  return 1
}

for b in $BLOCKS; do
  IFS=: read -r pre grace <<< "$b"
  tag="${pre}-w4kmix"; chunk="${pre##*-}"
  loadwait || { echo "$tag: load did not fall in time; stopping"; exit 4; }
  read -r l1 l5 l15 _ < /proc/loadavg
  OUT="$LUNIX_SPIKE_DIR/c5-$tag"
  echo "== $tag grace=$grace chunk=$chunk load=$l1/$l5/$l15 at $(date -u +%FT%TZ)"
  rm -rf "${RESDIR:?}/$tag"
  common="throttle_pace_store_bytes = true
gc_fast_recheck = true
gc_condemn_on_cut = \"on\"
gc_pressure_grace_ms = $grace"
  env OUT="$OUT" CELL=w4kmix CHUNK="$chunk" REPS="${REPS:-5}" W_RUNTIME="${W_RUNTIME:-60}" LOG_LEVEL=debug CFG_EXTRA="$common" RESTART_REP=0 VOLMODE=integrity \
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
