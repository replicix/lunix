#!/bin/bash
# c5def driver (root, on the test host; never run locally): the real-daemon gate of making gc_condemn_on_cut = "on" the default.
# shipcheck arms (log_level debug; destage_workers is the only other key):
#   SHIP  no GC or throttle override at all: the shipped defaults (gc_pressure_grace_ms 2000, gc_fast_recheck on, gc_condemn_on_cut on, throttle_pace_store_bytes off)
#   OLD   gc_pressure_grace_ms = 0, gc_fast_recheck = false, gc_condemn_on_cut = "off" (throttle_pace_store_bytes at its default, off)
# One fresh daemon and volume per block (tests/perf/c5t6-cells.sh); the store is a tmpfs mounted here. A block starts when the host is quiet
# (cpu and io PSI some avg10 below 1 % for 10 s in a row, no fio or lunixd alive).
# BLOCKS words "NN-arm:kind:chunk" (tag bNN-<arm>-<chunk>-<kind>; arm SHIP | OLD); kind w4k | w4kz | pin (plain volume, pin held through rep 3, removed before rep 4) | restart (w4k, clean restart in rep 3).
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR RESDIR; BLOCKS, TMPFS_GIB.
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIXD:?}" "${LUNIX:?}" "${TESTD:?}" "${LUNIX_SPIKE_DIR:?}" "${RESDIR:?}"
: "${TMPFS_GIB:=20}" "${LOADWAIT_S:=3600}"
STORE="$LUNIX_SPIKE_DIR/store"
mkdir -p "$RESDIR" "$STORE"
mountpoint -q "$STORE" || mount -t tmpfs -o "size=${TMPFS_GIB}g" tmpfs "$STORE" || exit 5
trap 'umount "$STORE" 2>/dev/null' EXIT
export LUNIX_SPIKE_DIR="$STORE"
export LUNIXD LUNIX TESTD LUNIX_ALLOW_TMPFS_FOR_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_ROOT_TESTS=1 LUNIX_MIN_FREE_GIB=2 DF_GIB=1
: "${BLOCKS:=01-SHIP:w4k:65536 02-OLD:w4k:65536 03-OLD:w4k:65536 04-SHIP:w4k:65536 05-OLD:w4k:16384 06-SHIP:w4k:16384 07-SHIP:w4k:16384 08-OLD:w4k:16384 09-SHIP:w4kz:65536 10-OLD:w4kz:65536 11-SHIP:pin:65536}"

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
  IFS=: read -r pre kind chunk <<< "$b"
  nn="${pre%%-*}"; arm="${pre#*-}"
  tag="b${nn}-${arm}-${chunk}-${kind}"
  loadwait || { echo "$tag: load did not fall in time; stopping"; exit 4; }
  read -r l1 l5 l15 _ < /proc/loadavg
  OUT="$LUNIX_SPIKE_DIR/c5-$tag"
  echo "== $tag arm=$arm kind=$kind chunk=$chunk load=$l1/$l5/$l15 at $(date -u +%FT%TZ)"
  rm -rf "${RESDIR:?}/$tag"
  case "$arm" in
    SHIP) common="" ;;   # pure defaults: no GC or throttle key at all
    OLD) common="gc_pressure_grace_ms = 0
gc_fast_recheck = false
gc_condemn_on_cut = \"off\"" ;;
    *) echo "unknown arm $arm"; exit 2 ;;
  esac
  cell=$kind; rr=0; vm=integrity; pra=0
  [ "$kind" = restart ] && { cell=w4k; rr=3; }
  [ "$kind" = pin ] && { vm=plain; pra=3; }
  env OUT="$OUT" CELL="$cell" CHUNK="$chunk" REPS="${REPS:-5}" W_RUNTIME="${W_RUNTIME:-60}" LOG_LEVEL=debug CFG_EXTRA="$common" RESTART_REP="$rr" VOLMODE="$vm" PIN_RELEASE_AFTER="$pra" \
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
