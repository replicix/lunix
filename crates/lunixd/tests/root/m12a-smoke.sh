#!/bin/bash
# M12a device smoke (root; plan M12a gate "Device smoke" and the idle CPU and memory gate).
#
#   create, attach, mkfs.ext4, mount, write 1 GiB with fio verify, detach while mounted is Busy,
#   umount, fsck.ext4, detach, re-attach, verify, umount, detach, `lunixd check` clean (the store
#   self-check standing in for `lunix fsck` until M14); then one kill -9 of lunixd under a fio
#   verify load and recovery by a new lunixd (the device is recovered, fio finishes and verifies);
#   then the idle daemon's CPU and resident memory with one idle 1 TiB volume at 4 queues (and 8
#   for information).
#
# Binaries are built beforehand as the normal user (never compile as root):
#   tools/remote.sh cargo build --release -p lunixd -p lunix-ublk
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> \
#       LUNIXD=<abs target>/release/lunixd TESTD=<abs target>/release/lunix-ublk-testd \
#       bash crates/lunixd/tests/root/m12a-smoke.sh
#
# Safety (plan W1-W9): lunixd never sends a stop command (detach is TRY_STOP_DEV, exit is QUIESCE);
# a delete is only `lunix-ublk-testd del --force` of a device recorded by this run whose daemon is
# gone (W1a); every failure path stops fio, makes sure a daemon serves the device before the
# unmount, unmounts, detaches, and only then lets the daemon exit; a stopping daemon is never killed;
# the kill -9 reaches only the daemon of the crash cell (no LUNIX_CRASH_AT is used at all). A wedged
# step is reported and the run stops (W9). Disk: about 3.5 GiB peak, inside the plan's 8 GiB (no GC yet,
# so the crash cell's random writes are rate-limited); df-guard asks for SMOKE_GIB (4) before mkfs.
here="$(cd "$(dirname "$0")" && pwd)"
ublk_root="$here/../../../lunix-ublk/tests/root"
. "$ublk_root/lib.sh"
require_root; need_root_tests
: "${LUNIXD:?LUNIXD is not set}" "${TESTD:?TESTD is not set}"
: "${OUT:=$LUNIX_SPIKE_DIR/m12a-smoke-out}" "${DAEMON_CPUS:=2-5}" "${FIO_CPUS:=6-7}"
ST="$LUNIX_SPIKE_DIR/m12a-smoke-state"; VOLS="$LUNIX_SPIKE_DIR/m12a-smoke-vols"; MNT="$LUNIX_SPIKE_DIR/m12a-smoke-mnt"
need_safe "$OUT"; need_safe "$ST"; need_safe "$VOLS"; need_safe "$MNT"
rm -rf "$OUT"; mkdir -p "$OUT"
bash "$here/../../../../spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; echo "not clean at start"; exit 2; }
: > "$CREATED_DEVICES"
script_cap "${SCRIPT_CAP:-1800}"
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
rm -rf "$ST" "$VOLS"; mkdir -p "$ST" "$VOLS" "$MNT"
SOCK="$ST/ctl.sock"
cat > "$ST/lunixd.toml" <<CFG
state_dir = "$ST/state"
socket = "$SOCK"
dev_dir = "$ST/dev"
device = "ublk"
harden = true
cpus = "$DAEMON_CPUS"
journal_max = 536870912
commit_interval_ms = 5000
log_level = "info"
CFG

R="$OUT/results.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
FAIL=0
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi; }
DPID=""; FIO_PID=""; _dn=0
wedged() { echo "WEDGED/STOP: $*" | tee -a "$OUT/WEDGED"; exit 9; }

start_lunixd() {
  _dn=$((_dn + 1)); DLOG="$OUT/lunixd.$_dn.log"
  env -u LUNIX_CRASH_AT taskset -c "$DAEMON_CPUS" "$LUNIXD" --config "$ST/lunixd.toml" > "$DLOG" 2>&1 &
  DPID=$!
  local i
  for i in $(seq 1 1200); do
    "$LUNIXD" ctl --socket "$SOCK" '{"method":"ping"}' >/dev/null 2>&1 && break
    kill -0 "$DPID" 2>/dev/null || { echo "lunixd died at start:"; tail -20 "$DLOG"; register_all; return 9; }
    sleep 0.05
  done
  register_all
  "$LUNIXD" ctl --socket "$SOCK" '{"method":"ping"}' >/dev/null 2>&1 || wedged "lunixd $DPID not ready after 60 s"
}
ctl() { timeout -k 5 300 "$LUNIXD" ctl --socket "$SOCK" "$1"; }
# Records every device the daemon announced (attach replies, recoveries in its logs, kernel ids it
# names in errors), so that nothing this run made goes unrecorded.
register_id() { grep -qxF "/dev/ublkb$1" "$CREATED_DEVICES" || { register_device "/dev/ublkb$1"; register_device "/dev/ublkc$1"; }; }
register_all() {
  local id
  for id in $(cat "$OUT"/lunixd.*.log "$OUT"/ctl.*.json 2>/dev/null | grep -o -E '(dev_id[=":]+ *)[0-9]+' | grep -o -E '[0-9]+$' | sort -u); do register_id "$id"; done
}
_cn=0
ctlj() { # request -> prints the reply; keeps a copy
  _cn=$((_cn + 1)); local f="$OUT/ctl.$_cn.json"
  ctl "$1" > "$f"; local rc=$?
  register_all; cat "$f"; return $rc
}
attached_dev() { # name -> /dev/ublkbN of the attached volume (from volume_status)
  ctl "{\"method\":\"volume_status\",\"params\":{\"name\":\"$1\"}}" | jq -r '.ok.device // empty' | xargs -r readlink -f
}

stop_fio() {
  [ -n "$FIO_PID" ] || return 0
  kill -TERM "$FIO_PID" 2>/dev/null
  local i; for i in $(seq 1 600); do kill -0 "$FIO_PID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$FIO_PID" 2>/dev/null && echo "teardown: fio $FIO_PID did not stop within 60 s (not killed)" >&2
  FIO_PID=""
}
stop_lunixd() { # SIGTERM = QuiesceAll then exit; a stopping daemon is never killed (W4)
  [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null || { DPID=""; return 0; }
  kill -TERM "$DPID"
  local i; for i in $(seq 1 600); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$DPID" 2>/dev/null; then echo "lunixd $DPID did not exit within 60 s of SIGTERM; not killing it" >&2; return 1; fi
  wait "$DPID" 2>/dev/null; DPID=""
}
detach_all() {
  local n
  for n in $(ctl '{"method":"volume_list"}' | jq -r '.ok.volumes[] | select(.device != null) | .name'); do
    ctl "{\"method\":\"detach\",\"params\":{\"name\":\"$n\"}}" >> "$OUT/teardown.log" 2>&1
  done
}
teardown() {
  trap - EXIT INT TERM
  stop_fio
  if [ -e "$OUT/WEDGED" ]; then echo "teardown: run marked WEDGED; nothing else touched" >&2; cap_stop; return 1; fi
  local d left=""
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do [ -b "$d" ] && left="$left $d"; done
  if [ -n "$left" ] && { [ -z "$DPID" ] || ! kill -0 "$DPID" 2>/dev/null; }; then
    echo "teardown: devices$left have no daemon; starting lunixd to recover them"
    start_lunixd || echo "teardown: lunixd did not start" >&2
  fi
  if mountpoint -q "$MNT" 2>/dev/null; then
    timeout -k 5 90 umount "$MNT" || { echo "teardown: umount timed out; stopping here" >&2; cap_stop; return 1; }
  fi
  if [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null; then detach_all; stop_lunixd || { cap_stop; return 1; }; fi
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do
    if [ -b "$d" ]; then
      echo "teardown: deleting $d (its daemon is gone)"
      timeout -k 5 75 "$TESTD" del --id "${d#/dev/ublkb}" --force || echo "teardown: delete of $d failed" >&2
    fi
  done
  rm -rf "$VOLS"
  cap_stop
  return 0
}
trap 'teardown >> "$OUT/teardown.log" 2>&1; cleanup' EXIT INT TERM

FIO_COMMON=(--filename="$MNT/f" --size=1G --bs=128k --ioengine=libaio --iodepth=16 --direct=1 --verify=crc32c --verify_fatal=1)

say "# M12a device smoke ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus)"
if [ -n "${ONLY_IDLE:-}" ]; then
  # Re-run of the idle cells alone (512 MiB of journal files; 1 GiB more for the 8-queue cell).
  df_guard "$LUNIX_SPIKE_DIR" "$([ -n "${SKIP_8Q:-}" ] && echo 1 || echo 2)" || exit 2
else
  df_guard "$LUNIX_SPIKE_DIR" "${SMOKE_GIB:-4}" || exit 2
fi
start_lunixd || exit 1
if [ -z "${ONLY_IDLE:-}" ]; then
ctlj "{\"method\":\"volume_create\",\"params\":{\"name\":\"smoke\",\"dir\":\"$VOLS\",\"size_bytes\":$((4 << 30)),\"chunk_size\":65536}}" | grep -q '"volume_created"'
expect "volume_create (integrity default, 4 GiB, 64 KiB chunks)" $?
VOLDIR=$(ctl '{"method":"volume_list"}' | jq -r '.ok.volumes[] | select(.name=="smoke") | .dir')
ctlj '{"method":"attach","params":{"name":"smoke","queues":4}}' | tee "$OUT/attach1.json" | grep -q '"attached"'
expect "attach (no credential: the key file beside the volume)" $?
DEV=$(attached_dev smoke); [ -b "$DEV" ]; expect "device $DEV exists, symlink $ST/dev/smoke resolves to it" $?
[ "$(readlink -f "$ST/dev/smoke")" = "$DEV" ]; expect "symlink matches" $?
[ -n "$DEV" ] || exit 1
df_guard "$LUNIX_SPIKE_DIR" "${MKFS_GIB:-3}" || exit 2   # what is still to come: the fs and its data
mkfs.ext4 -q -F "$DEV"; expect "mkfs.ext4" $?
mount -o nosuid,nodev "$DEV" "$MNT"; expect "mount" $?

say ""; say "## write 1 GiB with fio verify"
taskset -c "$FIO_CPUS" fio --name=w --rw=write --do_verify=1 "${FIO_COMMON[@]}" --output="$OUT/fio-write.txt" >/dev/null 2>&1
expect "fio write + verify of 1 GiB (rc $?)" $?
grep -E 'WRITE:|READ:' "$OUT/fio-write.txt" | sed 's/^/    /' | tee -a "$R"
out=$(ctl '{"method":"detach","params":{"name":"smoke"}}'); rc=$?
echo "$out" | grep -q '"busy"' && [ "$rc" = 3 ]; expect "detach while mounted is Busy, exit code 3 (rc $rc)" $?
[ -b "$DEV" ] && mountpoint -q "$MNT"; expect "the device and the mount survive the busy detach" $?
timeout -k 5 90 umount "$MNT" || wedged "umount timed out"
fsck.ext4 -fn "$DEV" > "$OUT/fsck1.txt" 2>&1; expect "fsck.ext4 -fn clean" $?
ctlj '{"method":"detach","params":{"name":"smoke"}}' | grep -q '"detached"'; expect "detach after umount" $?
for i in $(seq 1 100); do [ -e "$DEV" ] || break; sleep 0.1; done
[ ! -e "$DEV" ]; expect "the device is gone" $?

say ""; say "## re-attach and verify"
ctlj '{"method":"attach","params":{"name":"smoke"}}' | grep -q '"attached"'; expect "re-attach" $?
DEV=$(attached_dev smoke)
mount -o nosuid,nodev "$DEV" "$MNT"; expect "mount" $?
taskset -c "$FIO_CPUS" fio --name=w --rw=write --verify_only=1 "${FIO_COMMON[@]}" --output="$OUT/fio-verify.txt" >/dev/null 2>&1
expect "fio verify-only of the 1 GiB after re-attach (rc $?)" $?
timeout -k 5 90 umount "$MNT" || wedged "umount timed out"
ctlj '{"method":"detach","params":{"name":"smoke"}}' | grep -q '"detached"'; expect "detach" $?
"$LUNIXD" check --dir "$VOLDIR" > "$OUT/check1.txt" 2>&1; expect "lunixd check clean ($(head -1 "$OUT/check1.txt"))" $?

say ""; say "## kill -9 under a fio verify load, then recovery"
ctlj '{"method":"attach","params":{"name":"smoke"}}' | grep -q '"attached"'; expect "attach" $?
DEV=$(attached_dev smoke); ID1=${DEV#/dev/ublkb}
mount -o nosuid,nodev "$DEV" "$MNT"; expect "mount" $?
taskset -c "$FIO_CPUS" fio --name=crash --rw=randwrite --do_verify=1 --verify_backlog=1024 --time_based --runtime=40 --rate=16m "${FIO_COMMON[@]}" --output="$OUT/fio-crash.txt" >/dev/null 2>&1 &
FIO_PID=$!
sleep 15
kill -0 "$FIO_PID" 2>/dev/null; expect "fio running before the kill" $?
OLD=$DPID
kill -KILL "$OLD"; wait "$OLD" 2>/dev/null; DPID=""
say "- kill -9 sent to lunixd $OLD (fio keeps its I/O in the kernel queue)"
sleep 2
t0=$(date +%s%N)
start_lunixd || wedged "lunixd did not restart after the kill"
grep -q 'volume recovered at start.*recovered=true' "$DLOG"; expect "the new lunixd recovered the device at start ($(grep -o 'stall_ms=[0-9]*' "$DLOG" | head -1))" $?
say "- restart to ready: $(( ($(date +%s%N) - t0) / 1000000 )) ms"
[ "$(attached_dev smoke)" = "/dev/ublkb$ID1" ]; expect "same device id after recovery" $?
for i in $(seq 1 1200); do kill -0 "$FIO_PID" 2>/dev/null || break; sleep 0.1; done
kill -0 "$FIO_PID" 2>/dev/null && wedged "fio did not finish 120 s after the recovery"
wait "$FIO_PID"; frc=$?; FIO_PID=""
expect "fio finished with verify passing across the kill (rc $frc)" "$frc"
grep -E 'WRITE:|READ:|err=' "$OUT/fio-crash.txt" | sed 's/^/    /' | tee -a "$R"
timeout -k 5 90 umount "$MNT" || wedged "umount timed out"
fsck.ext4 -fn "$DEV" > "$OUT/fsck2.txt" 2>&1; expect "fsck.ext4 -fn clean after the crash" $?
ctlj '{"method":"detach","params":{"name":"smoke"}}' | grep -q '"detached"'; expect "detach" $?
"$LUNIXD" check --dir "$VOLDIR" > "$OUT/check2.txt" 2>&1; expect "lunixd check clean after the crash ($(head -1 "$OUT/check2.txt"))" $?

# The smoke volume is done: free its disk before the idle cells (there is no GC yet).
rm -rf "$VOLDIR"
fi

say ""; say "## idle daemon: CPU and resident memory with one idle 1 TiB volume"
create_big() { # name
  ctlj "{\"method\":\"volume_create\",\"params\":{\"name\":\"$1\",\"dir\":\"$VOLS\",\"size_bytes\":$((1 << 40)),\"chunk_size\":65536}}" | grep -q '"volume_created"'
  expect "create the 1 TiB volume $1" $?
}
measure() { # name queues
  local n=$1; shift
  ctlj "{\"method\":\"attach\",\"params\":{\"name\":\"$n\",\"queues\":$1}}" > "$OUT/attach-big-$1.json" || { fail "attach $n at $1 queues: $(cat "$OUT/attach-big-$1.json")"; return; }
  sleep 10
  local tck s1 s2 rss
  tck=$(getconf CLK_TCK)
  s1=$(awk '{print $14 + $15}' "/proc/$DPID/stat"); sleep 30; s2=$(awk '{print $14 + $15}' "/proc/$DPID/stat")
  rss=$(awk '/^VmRSS/ {print $2}' "/proc/$DPID/status")
  local q d b bufs cpu
  q=$(jq -r '.ok.queues' "$OUT/attach-big-$1.json"); d=$(jq -r '.ok.depth' "$OUT/attach-big-$1.json"); b=$((1 << 20))
  bufs=$((q * d * b / 1048576))
  cpu=$(awk -v a="$s1" -v b="$s2" -v t="$tck" 'BEGIN { printf "%.3f", (b - a) / t / 30 * 100 }')
  say "- $q queues x depth $d x 1 MiB: buffers $bufs MiB; VmRSS $((rss / 1024)) MiB; idle CPU $cpu % of one core over 30 s"
  echo "$q $bufs $((rss / 1024)) $cpu" >> "$OUT/idle.txt"
  ctl "{\"method\":\"detach\",\"params\":{\"name\":\"$n\"}}" >/dev/null || fail "detach $n"
}
# The idle figures are taken on a fresh daemon: the one that served the crash cell keeps the heap
# its destage and replay grew (allocator retention), which is not the idle volume's cost. That
# figure is printed first, for information.
create_big big
measure big 4
say "- (4 queues on the daemon that ran the cells before, for information: $(tail -1 "$OUT/idle.txt"))"
stop_lunixd || wedged "lunixd did not exit before the idle cells"
start_lunixd || exit 1
measure big 4
read -r q bufs rss cpu < <(tail -1 "$OUT/idle.txt")
[ "$rss" -le $((bufs + 150)) ]; expect "memory at 4 queues: $rss MiB <= buffers $bufs + 150 MiB" $?
awk -v c="$cpu" 'BEGIN { exit !(c <= 1.0) }'; expect "idle CPU $cpu % <= 1 %" $?
if [ -z "${SKIP_8Q:-}" ]; then
# 8 lanes need a larger journal (two 64 MiB files per lane): a new volume made by a daemon
# restarted with journal_max = 1 GiB.
stop_lunixd || wedged "lunixd did not exit before the 8-queue cell"
sed -i 's/^journal_max = .*/journal_max = 1073741824/' "$ST/lunixd.toml"
start_lunixd || exit 1
create_big big8
measure big8 8
say "- (8 queues for information: $(tail -1 "$OUT/idle.txt"))"
fi

say ""; say "## end"
stop_lunixd; expect "lunixd exits on SIGTERM (quiesce of no device)" $?
du -sh "$VOLS" | tee -a "$R"
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
