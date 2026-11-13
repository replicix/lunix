#!/bin/bash
# Brings a run that was stopped with a dead daemon back to a clean state: starts a recovering daemon (no crash
# point), lets the blocked load finish, unmounts, detaches and deletes the recorded device. Only for devices
# this harness created (the state dir and image of the named run).  NAME=loop|raw|mounted|rz
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${NAME:?}" "${OUT:=$LUNIX_SPIKE_DIR/out-rescue}"
mkdir -p "$OUT"
STATE="$LUNIX_SPIKE_DIR/state/$NAME"; IMG="$LUNIX_SPIKE_DIR/${IMGNAME:?}"; MNT="${MNTDIR:?}"
need_safe "$STATE"; need_safe "$IMG"; need_safe "$MNT"
MAX_GIB="${MAX_GIB:-16}"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
DEVID=$(python3 -I -c 'import struct,sys;print(struct.unpack_from("<I",open(sys.argv[1],"rb").read(),16)[0])' "$STATE/device")
DEV=/dev/ublkb$DEVID
grep -qxF "$DEV" "$CREATED_DEVICES" || { echo "refusing: $DEV is not recorded as created by this harness"; exit 2; }
start_daemon recover || exit 1
# Stop the load (time_based fio would run to its runtime): only processes whose cwd is the mount of this run.
for p in /proc/[0-9]*; do
  c=$(readlink "$p/cwd" 2>/dev/null) || continue
  case "$c" in "$MNT"|"$MNT"/*) kill -TERM "${p#/proc/}" 2>/dev/null;; esac
done
for i in $(seq 1 120); do
  busy=0
  for p in /proc/[0-9]*; do c=$(readlink "$p/cwd" 2>/dev/null) || continue; case "$c" in "$MNT"|"$MNT"/*) busy=1;; esac; done
  [ $busy = 0 ] && break; sleep 1
done
[ $busy = 0 ] || wedged "load processes still use $MNT 2 min after SIGTERM"
exit 0
