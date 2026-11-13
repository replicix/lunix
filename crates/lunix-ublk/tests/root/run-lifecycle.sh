#!/bin/bash
# M10 review-fix cells on raw (unmounted) devices:
#   A  DEL_DEV of a LIVE device is refused unless forced (it is a STOP_DEV);
#   B  a crash between UPDATE_SIZE and the record write: the daemon recovers its own device;
#   C  START_DEV fails: the layer releases the queues and deletes the never-started device;
#   D  two volumes of equal geometry: a record pointed at the other volume's device is refused, nothing touched.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/out-lifecycle}"
rm -rf "$OUT"; mkdir -p "$OUT"
MAX_GIB=4; QUEUES=2
STATE="$LUNIX_SPIKE_DIR/state/life"; IMG="$LUNIX_SPIKE_DIR/life.img"; need_safe "$IMG"; need_safe "$STATE"
need_clean_start "$OUT"
script_cap "${SCRIPT_CAP:-1200}"
load_ublk
rm -rf "$STATE"; mkdir -p "$STATE"; rm -f "$IMG"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
R="$OUT/results.md"; : > "$R"
say() { echo "$*" | tee -a "$R"; }
FAIL=0
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi; }
dev_bytes() { blockdev --getsize64 "$DEV"; }
info_line() { td info --id "$1" | tr '\n' ' '; }
df_guard "$LUNIX_SPIKE_DIR" 6 || exit 2

say "# M10 lifecycle cells ($(date -u +%FT%TZ))"

say ""; say "## A: DEL_DEV of a live device is refused"
start_daemon run --create-gib 2 || exit 1
out=$(td del --id "$DEVID" 2>&1); rc=$?
say "- del without --force on a live device: rc $rc, '$out'"
[ "$rc" != 0 ] && echo "$out" | grep -q "LIVE"; expect "refused as a stop" $?
kill -0 "$DPID" 2>/dev/null && [ -b "$DEV" ]; expect "daemon and device untouched" $?
dd if="$DEV" of=/dev/null bs=4k count=64 iflag=direct 2>/dev/null; expect "the device still serves" $?
tdc detach | grep -q '^OK'; expect "detach (TRY_STOP_DEV) works" $?
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
wait "$DPID" 2>/dev/null; DPID=""; DEVID=""

say ""; say "## B: crash between UPDATE_SIZE and the record write"
rm -f "$IMG"; rm -rf "$STATE"; mkdir -p "$STATE"
CRASH_AT="ublk.after_update_size:1" start_daemon run --create-gib 2 || exit 1
CRASH_AT=""
id=$DEVID; dev=$DEV
before=$(python3 -I -c 'import struct,sys;print(struct.unpack_from("<Q",open(sys.argv[1],"rb").read(),56)[0])' "$STATE/device")
tdc grow --bytes $((4 << 30)) > "$OUT/grow-kill.txt" 2>&1; grc=$?
for i in $(seq 1 100); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
kill -0 "$DPID" 2>/dev/null && wedged "the daemon did not die at the crash point"
wait "$DPID" 2>/dev/null; xrc=$?
after=$(python3 -I -c 'import struct,sys;print(struct.unpack_from("<Q",open(sys.argv[1],"rb").read(),56)[0])' "$STATE/device")
[ "$grc" != 0 ] && [ "$xrc" = 137 ]; expect "the daemon was killed at ublk.after_update_size (grow rc $grc, exit $xrc)" $?
[ "$before" = "$after" ] && [ "$after" = $(( (2 << 30) >> 9 )) ]; expect "the record on disk is still the old size ($after sectors)" $?
sleep 2
say "- $(info_line "$id")"
td info --id "$id" | grep -q "dev_sectors=$(( (4 << 30) >> 9 ))"; expect "the kernel already has the new size" $?
start_daemon recover; expect "recovery accepts record < device = engine" $?
[ "$(dev_bytes)" = $((4 << 30)) ]; expect "the device is 4 GiB" $?
python3 -I -c 'import struct,sys;sys.exit(0 if struct.unpack_from("<Q",open(sys.argv[1],"rb").read(),56)[0]==(4<<30)>>9 else 1)' "$STATE/device"; expect "the record now holds the new size" $?
timeout 300 fio --name=lifeb --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randwrite --bs=4k --iodepth=16 --offset=3g --size=64m --io_size=64m \
   --verify=crc32c --do_verify=1 --verify_fatal=1 --cpus_allowed="$FIO_CPUS" > "$OUT/lifeb.out" 2>&1; expect "write+verify in the grown region (rc $?)" $?
tdc detach | grep -q '^OK'; expect "detach" $?
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
wait "$DPID" 2>/dev/null; DPID=""; DEVID=""

say ""; say "## C: START_DEV fails -> queues released, the never-started device deleted"
rm -f "$IMG"; rm -rf "$STATE"; mkdir -p "$STATE"
start_daemon run --create-gib 2 --fail-start; src=$?
added=$(sed -n 's/^ADDED id=\([0-9]*\).*/\1/p' "$DLOG" | head -1)
say "- start rc $src; ADDED id=$added; log: $(tr '\n' ' ' < "$DLOG" | cut -c1-300)"
[ "$src" = 9 ] && [ -n "$added" ]; expect "the daemon exited with the device id announced" $?
for i in $(seq 1 100); do [ -e "/dev/ublkb$added" ] || break; sleep 0.1; done
[ ! -e "/dev/ublkb$added" ] && [ ! -e "/sys/class/ublk-char/ublkc$added" ]; expect "the device was deleted by the layer" $?
grep -q "^ERRDEV id=$added" "$DLOG"; expect "the error carries the device id" $?
DEVID=""; DPID=""

say ""; say "## D: equal geometry, other volume: the record of A pointed at B's device is refused"
SA="$LUNIX_SPIKE_DIR/state/lifeA"; SB="$LUNIX_SPIKE_DIR/state/lifeB"; IA="$LUNIX_SPIKE_DIR/lifeA.img"; IB="$LUNIX_SPIKE_DIR/lifeB.img"
rm -rf "$SA" "$SB"; mkdir -p "$SA" "$SB"; rm -f "$IA" "$IB"
STATE=$SA IMG=$IA start_daemon run --create-gib 1 --volume-seed 1 || exit 1
pidA=$DPID; idA=$DEVID
STATE=$SB IMG=$IB start_daemon run --create-gib 1 --volume-seed 2 || exit 1
pidB=$DPID; idB=$DEVID
track_extra "$pidA" "$idA"; track_extra "$pidB" "$idB"
say "- A is device $idA (seed 1), B is device $idB (seed 2)"
[ "$(td info --id "$idA" | head -1 | sed 's/.*tag=//')" != "$(td info --id "$idB" | head -1 | sed 's/.*tag=//')" ]; expect "the two devices carry different volume tags" $?
kill -9 "$pidA" "$pidB"; wait "$pidA" "$pidB" 2>/dev/null
sleep 2
baseA=$(info_line "$idA"); baseB=$(info_line "$idB")
cp "$SA/device" "$SA/device.good"
python3 -I "$here/mkrec.py" "$SA/device.good" "$SA/device" dev_id="$idB"
STATE=$SA IMG=$IA start_daemon recover --volume-seed 1; rc=$?
[ "$rc" = 3 ] && grep -q '^REFUSED identity' "$DLOG" && grep -q 'volume tag' "$DLOG"; expect "A's record pointed at B's device is refused (rc $rc): $(grep '^REFUSED' "$DLOG")" $?
[ "$(info_line "$idA")" = "$baseA" ] && [ "$(info_line "$idB")" = "$baseB" ]; expect "neither device changed" $?
cp "$SA/device.good" "$SA/device"
untrack_extra "$pidA" "$idA"
STATE=$SA IMG=$IA start_daemon recover --volume-seed 1; expect "A recovers with its own record" $?
pidA=$DPID; track_extra "$pidA" "$idA"
untrack_extra "$pidB" "$idB"
STATE=$SB IMG=$IB start_daemon recover --volume-seed 2; expect "B recovers with its own record" $?
pidB=$DPID; track_extra "$pidB" "$idB"
# Detach both through their sockets (no mount, nothing open).
for st in "$SA" "$SB"; do td detach --state "$st" | grep -q '^OK'; expect "detach $st" $?; done
for p in "$pidA" "$pidB"; do for i in $(seq 1 300); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done; done
wait "$pidA" "$pidB" 2>/dev/null
EXTRAS=""; DEVID=""; DPID=""; STATE="$LUNIX_SPIKE_DIR/state/life"; IMG=""
rm -f "$IA" "$IB"

say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
