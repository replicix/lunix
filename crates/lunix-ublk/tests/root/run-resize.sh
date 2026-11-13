#!/bin/bash
# M10 resize cells: UPDATE_SIZE 16 -> 64 GiB under fio on ext4, xfs and btrfs (sparse backing file, bounded
# fio: --io_size 2 GiB per run, well under the 8 GiB cap), grow right after a kill -9 recovery, and
# rejected sizes on a mounted device.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/out-resize}" "${FSES:=ext4 xfs btrfs}" "${IO_SIZE:=2g}"
rm -rf "$OUT"; mkdir -p "$OUT"
MAX_GIB=64
IMG="$LUNIX_SPIKE_DIR/rz.img"; MNT="$LUNIX_SPIKE_DIR/mnt/rz"; STATE="$LUNIX_SPIKE_DIR/state/rz"
need_safe "$IMG"; need_safe "$MNT"; need_safe "$STATE"
need_clean_start "$OUT"
load_ublk
mkdir -p "$MNT"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
R="$OUT/results.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
FAIL=0
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi; }
dev_bytes() { blockdev --getsize64 "$DEV"; }
fs_size() { df -B1 --output=size "$MNT" | tail -1 | tr -d ' '; }
MKFS_GIB=16

fresh() { # fs
  DEVID=""; rm -rf "$STATE"; mkdir -p "$STATE"; rm -f "$IMG"
  df_guard "$LUNIX_SPIKE_DIR" 16 || return 1
  start_daemon run --create-gib 16 || return 1
  make_fs "$1" "$DEV" && mount_fs "$1" "$DEV" "$MNT"
}
finish() { # fs
  timeout -k 5 120 umount "$MNT" || wedged "umount timed out"
  local rc; rc=$(check_fs "$1" "$DEV"); [ "$rc" = 0 ]; expect "$1 check after unmount: rc $rc" $?
  tdc detach | grep -q '^OK'; expect "detach" $?
  for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && wedged "daemon did not exit after the detach"
  wait "$DPID" 2>/dev/null; DPID=""; DEVID=""; rm -f "$IMG"
}
fio_load() { # name
  (cd "$MNT" && exec timeout -k 10 900 fio --name=g --directory="$MNT" --rw=randwrite --bs=4k --iodepth=16 --numjobs=2 \
     --size=1g --io_size="$IO_SIZE" --ioengine=io_uring --direct=1 --verify=crc32c --verify_backlog=64 --verify_fatal=1 --norandommap \
     --runtime=600 --cpus_allowed="$FIO_CPUS" --output-format=json --output="$OUT/$1.json" > "$OUT/$1.out" 2>&1) &
  FIO_PID=$!
}
fio_done() { wait "$FIO_PID"; FIO_RC=$?; FIO_PID=""; FIO_ERR=$(jq -c '[.jobs[].error]' "$OUT/$1.json" 2>/dev/null || echo n/a); }
grow_fs() {
  case "$1" in
    ext4) resize2fs "$DEV" 2>&1 | tail -1;;
    xfs) xfs_growfs "$MNT" 2>&1 | grep -E "data +=" | head -1;;
    btrfs) btrfs filesystem resize max "$MNT" 2>&1;;
  esac
}

say "# M10 resize cells ($(date -u +%FT%TZ))"
for fs in $FSES; do
  say ""; say "## online grow $fs: 16 -> 64 GiB under fio randwrite+verify (io_size $IO_SIZE per job)"
  fresh "$fs" || { fail "$fs: fresh failed"; continue; }
  say "- before: device $(dev_bytes) bytes, fs $(fs_size)"
  fio_load "grow-$fs"; sleep 8
  tdc grow --bytes $((64 << 30)) | tee -a "$R" | grep -q '^OK'; expect "$fs: UPDATE_SIZE accepted" $?
  [ "$(dev_bytes)" = $((64 << 30)) ]; expect "$fs: block device is 64 GiB without a remount" $?
  say "- fs grow: $(grow_fs "$fs" | tr '\n' ' ')"
  say "- after: fs $(fs_size)"
  [ "$(fs_size)" -gt $((60 << 30)) ]; expect "$fs: file system grew online" $?
  fio_done "grow-$fs"; [ "$FIO_RC" = 0 ] && [ "$FIO_ERR" = "[0,0]" ]; expect "$fs: fio rc $FIO_RC, errors $FIO_ERR" $?
  for bad in "$(( (64 << 30) + 512 ))" "$(( 8 << 30 ))" "$(( 65 << 30 ))"; do
    res=$(tdc grow --bytes "$bad" 2>&1); rc=$?
    [ "$rc" != 0 ] && [ "$(dev_bytes)" = $((64 << 30)) ]; expect "$fs: grow $bad refused while mounted (rc $rc: $res)" $?
  done
  finish "$fs"
done

say ""; say "## grow right after kill -9 + recovery (ext4)"
fresh ext4 || { fail "fresh failed"; exit 1; }
fio_load grow-after-kill; sleep 6
kill -9 "$DPID"; wait "$DPID" 2>/dev/null
start_daemon recover || fail "recovery failed"
tdc grow --bytes $((64 << 30)) | tee -a "$R" | grep -q '^OK'; expect "grow after recovery accepted" $?
[ "$(dev_bytes)" = $((64 << 30)) ]; expect "device is 64 GiB" $?
say "- fs grow: $(grow_fs ext4 | tr '\n' ' ')"
fio_done grow-after-kill; [ "$FIO_RC" = 0 ] && [ "$FIO_ERR" = "[0,0]" ]; expect "fio rc $FIO_RC, errors $FIO_ERR" $?
finish ext4

say ""; say "## the recorded size follows a grow (state/device) and a recovery keeps it"
fresh ext4 || { fail "fresh failed"; exit 1; }
tdc grow --bytes $((32 << 30)) | grep -q '^OK'; expect "grow to 32 GiB" $?
kill -9 "$DPID"; wait "$DPID" 2>/dev/null
start_daemon recover; expect "recovery with the grown record" $?
[ "$(dev_bytes)" = $((32 << 30)) ]; expect "device is 32 GiB after the recovery" $?
finish ext4

say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
