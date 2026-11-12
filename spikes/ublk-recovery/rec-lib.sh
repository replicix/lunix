#!/bin/bash
# Helpers shared by the recovery / resize / flush drivers (source after common/lib.sh).
REC_BIN="${REC_BIN:-$here/target/release/ublk-recovery}"
: "${DAEMON_CPUS:=0-3}" "${FIO_CPUS:=4-7}" "${QUEUES:=4}"
DPID=""; DEV=""; DEVID=""; DLOG=""; _dn=0
now_ns() { date +%s%N; }
# start_daemon <extra args...>: starts `run`, waits for READY, sets DPID/DEV/DEVID/DLOG.
start_daemon() {
  _dn=$((_dn + 1)); DLOG="$OUT/daemon.$_dn.log"
  local id_arg=()
  [ -n "$DEVID" ] && id_arg=(--dev-id "$DEVID")
  taskset -c "$DAEMON_CPUS" "$REC_BIN" run --backing "$IMG" --queues "$QUEUES" --cpus "$DAEMON_CPUS" \
      "${id_arg[@]}" "$@" > "$DLOG" 2>&1 &
  DPID=$!
  local i
  for i in $(seq 1 1200); do
    grep -q '^READY' "$DLOG" && break
    kill -0 "$DPID" 2>/dev/null || { echo "daemon died during start:"; tail -5 "$DLOG"; exit 9; }
    sleep 0.05
  done
  grep -q '^READY' "$DLOG" || { echo "daemon not ready"; tail -5 "$DLOG"; exit 9; }
  DEVID=$(sed -n 's/^READY id=\([0-9]*\) .*/\1/p' "$DLOG")
  DEV=/dev/ublkb$DEVID
  grep -qxF "$DEV" "$CREATED_DEVICES" || { register_device "$DEV"; register_device "/dev/ublkc$DEVID"; }
}
daemon_ms() { sed -n 's/^READY .* ms=\([0-9]*\)$/\1/p' "$DLOG" | head -1; }
stop_daemon_clean() { # SIGTERM -> kill_dev; then delete
  [ -n "$DPID" ] && kill -TERM "$DPID" 2>/dev/null
  # No SIGKILL fallback: killing a daemon whose STOP_DEV is in flight wedges the device (seen twice); report instead.
  local i; for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && echo "WARNING: daemon $DPID did not stop within 30 s (device may be wedged); not killing it" >&2
  DPID=""
}
del_dev() { [ -n "$DEVID" ] && timeout -k 3 20 "$REC_BIN" del --id "$DEVID" >/dev/null 2>&1; }
counters() { # snapshot counters of the running daemon: prints "reads writes fua flushes"
  local before; before=$(grep -c '^COUNTERS' "$DLOG")
  kill -USR1 "$DPID"
  local i; for i in $(seq 1 50); do [ "$(grep -c '^COUNTERS' "$DLOG")" -gt "$before" ] && break; sleep 0.05; done
  grep '^COUNTERS' "$DLOG" | tail -1 | sed 's/COUNTERS reads=\([0-9]*\) writes=\([0-9]*\) fua_writes=\([0-9]*\) flushes=\([0-9]*\).*/\1 \2 \3 \4/'
}
mount_fs() { # fs dev mountpoint
  mkdir -p "$3"; safe_path "$3" || return 1
  mount -o nosuid,nodev "$2" "$3"
}
make_fs() { # fs dev
  safe_path "$2" || return 1
  case "$1" in
    ext4) mkfs.ext4 -q -F "$2";;
    xfs) mkfs.xfs -q -f "$2";;
    btrfs) mkfs.btrfs -q -f "$2" >/dev/null;;
  esac
}
check_fs() { # fs dev  (unmounted)
  case "$1" in
    ext4) fsck.ext4 -fn "$2" >/dev/null 2>&1; echo $?;;
    xfs) xfs_repair -n "$2" >/dev/null 2>&1; echo $?;;
    btrfs) btrfs check --readonly "$2" >/dev/null 2>&1; echo $?;;
  esac
}
