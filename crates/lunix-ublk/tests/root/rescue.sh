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
for i in $(seq 1 600); do pgrep -f "fio jobs.fio" >/dev/null || break; sleep 1; done
pgrep -f "fio jobs.fio" >/dev/null && wedged "fio still running 10 min after the daemon came back"
exit 0
