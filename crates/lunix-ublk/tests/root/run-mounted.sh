#!/bin/bash
# M10 mounted cells: TRY_STOP_DEV while mounted is Busy and device + mount survive; after umount it
# succeeds and DEL_DEV completes; a stuck control call while mounted does not disturb the file system.
# Order rule: run run-raw.sh first (unmounted cells), this one second.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/out-mounted}" "${FS:=ext4}"
rm -rf "$OUT"; mkdir -p "$OUT"
MAX_GIB=4
IMG="$LUNIX_SPIKE_DIR/mnt.img"; MNT="$LUNIX_SPIKE_DIR/mnt/mounted"; STATE="$LUNIX_SPIKE_DIR/state/mounted"
need_safe "$IMG"; need_safe "$MNT"; need_safe "$STATE"
need_clean_start "$OUT"
load_ublk
rm -rf "$STATE"; mkdir -p "$STATE" "$MNT"; rm -f "$IMG"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
R="$OUT/results.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
FAIL=0
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi; }

say "# M10 mounted cells ($(date -u +%FT%TZ), $FS)"
MKFS_GIB=2; df_guard "$LUNIX_SPIKE_DIR" 2 || exit 2
start_daemon run --create-gib 2 || exit 1
id=$DEVID
make_fs "$FS" "$DEV" || exit 1
mount_fs "$FS" "$DEV" "$MNT" || exit 1
echo data > "$MNT/f"; sync -f "$MNT"; expect "mkfs, mount and a synced write" $?

say ""; say "## TRY_STOP_DEV while mounted"
out=$(tdc detach 2>&1); rc=$?
say "- detach while mounted: rc $rc, '$out'"
[ "$rc" = 16 ] && [ "$out" = BUSY ]; expect "Busy, exit code 16" $?
kill -0 "$DPID" 2>/dev/null; expect "the daemon is still serving" $?
[ -b "/dev/ublkb$id" ]; expect "the device survives" $?
[ "$(cat "$MNT/f")" = data ]; expect "the file is readable" $?
(echo more >> "$MNT/f" && sync -f "$MNT"); expect "writes still work" $?
mountpoint -q "$MNT"; expect "the mount survives" $?
c=$(tdc cmd counters); echo "$c" | grep -q 'in_flight=0'; expect "nothing in flight when idle ($c)" $?

say ""; say "## a stuck control call does not disturb the mounted fs"
out=$(tdc cmd stuck-ctl 1500); echo "$out" | grep -q '^TIMEOUT'; expect "reported at the timeout ($out)" $?
(echo again >> "$MNT/f" && sync -f "$MNT"); expect "writes still work afterwards" $?

say ""; say "## umount, then detach succeeds and DEL_DEV completes"
timeout -k 5 90 umount "$MNT" || wedged "umount timed out"
fr=$(check_fs "$FS" "$DEV"); [ "$fr" = 0 ]; expect "$FS check clean (rc $fr)" $?
tdc detach | tee -a "$R" | grep -q '^OK'; expect "detach after umount answers OK" $?
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
kill -0 "$DPID" 2>/dev/null && wedged "daemon did not exit after the detach; stopped, nothing killed"
wait "$DPID"; xrc=$?; DPID=""
[ "$xrc" = 0 ]; expect "daemon exit code $xrc" $?
[ ! -e "/dev/ublkb$id" ] && [ ! -e "/sys/class/ublk-char/ublkc$id" ]; expect "device and char device are gone" $?
DEVID=""
say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
