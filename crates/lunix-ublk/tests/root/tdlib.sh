#!/bin/bash
# Helpers for the lunix-ublk-testd harness (source after lib.sh). Root only.
# Ported from spikes/ublk-recovery/rec-lib.sh for the M10 test daemon.
#
# Safety rules kept here (plan W1-W9):
#  * a daemon that is stopping is never signalled with KILL, we wait;
#  * teardown makes sure a daemon is serving BEFORE it unmounts (an unmount with
#    a dead daemon and dirty data blocks), then detaches (TRY_STOP_DEV), then
#    deletes only a device this run created and recorded;
#  * a stuck step is reported and the run stops; nothing is retried.
: "${TESTD:?TESTD is not set}"
: "${DAEMON_CPUS:=4-5}" "${FIO_CPUS:=6-7}" "${QUEUES:=4}" "${DEPTH:=128}"
DPID=""; DEV=""; DEVID=""; DLOG=""; _dn=0; STATE=""; IMG=""; MNT=""; FIO_PID=""; HB_PID=""; S1_PID=""; S2_PID=""
now_ns() { date +%s%N; }

wedged() { # message: report, stop the whole run, no retry (W9)
  echo "WEDGED/STOP: $*" | tee -a "${OUT:-/tmp}/WEDGED"
  touch "${OUT:-/tmp}/WEDGED"
  exit 9
}

# register_from_log LOG: record a device the daemon announced (ADDED / ERRDEV) before it was READY, so that a
# failed start never leaves an unrecorded device.
register_from_log() {
  local id
  for id in $(sed -n 's/^\(ADDED\|ERRDEV\) id=\([0-9]*\).*/\2/p' "$1" | sort -u); do
    grep -qxF "/dev/ublkb$id" "$CREATED_DEVICES" || { register_device "/dev/ublkb$id"; register_device "/dev/ublkc$id"; }
  done
}

# start_daemon MODE [testd args...]: MODE is run or recover; sets DPID DEV DEVID DLOG
start_daemon() {
  local mode=$1; shift
  _dn=$((_dn + 1)); DLOG="$OUT/daemon.$_dn.log"
  env -u LUNIX_CRASH_AT ${CRASH_AT:+LUNIX_CRASH_AT=$CRASH_AT} taskset -c "$DAEMON_CPUS" "$TESTD" "$mode" --state "$STATE" --backing "$IMG" --queues "$QUEUES" --depth "$DEPTH" \
      --cpus "$DAEMON_CPUS" ${MAX_GIB:+--max-gib $MAX_GIB} "$@" > "$DLOG" 2>&1 &
  DPID=$!
  local i
  for i in $(seq 1 1200); do
    grep -q '^READY' "$DLOG" && break
    grep -q '^REFUSED' "$DLOG" && { echo "daemon refused:"; cat "$DLOG"; return 3; }
    kill -0 "$DPID" 2>/dev/null || { register_from_log "$DLOG"; echo "daemon died during start:"; tail -5 "$DLOG"; return 9; }
    sleep 0.05
  done
  register_from_log "$DLOG"
  grep -q '^READY' "$DLOG" || wedged "daemon $DPID not ready after 60 s"
  DEVID=$(sed -n 's/^READY id=\([0-9]*\) .*/\1/p' "$DLOG")
  DEV=/dev/ublkb$DEVID
  grep -qxF "$DEV" "$CREATED_DEVICES" || { register_device "$DEV"; register_device "/dev/ublkc$DEVID"; }
  return 0
}
daemon_ms() { sed -n 's/^READY .* ms=\([0-9]*\)$/\1/p' "$DLOG" | head -1; }

# stop_daemon_clean: SIGTERM = detach (TRY_STOP_DEV). BUSY keeps the daemon serving; we only report.
# No KILL fallback: a stopping daemon is never killed (W4); after 30 s we report.
stop_daemon_clean() {
  [ -n "$DPID" ] || return 0
  kill -0 "$DPID" 2>/dev/null || { DPID=""; return 0; }
  kill -TERM "$DPID" 2>/dev/null
  local i; for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$DPID" 2>/dev/null; then
    echo "WARNING: daemon $DPID did not exit within 30 s of SIGTERM (BUSY or draining); not killing it" >&2
    return 1
  fi
  DPID=""
}
# del_dev: DEL_DEV of a LIVE device is a STOP_DEV, so it is only sent for a device whose daemon is gone (--force),
# never while the daemon of this run is alive.
del_dev() {
  [ -n "$DEVID" ] && [ -e "/dev/ublkb$DEVID" ] || return 0
  if [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null; then
    echo "del_dev: the daemon $DPID is alive; not deleting $DEV (DEL_DEV of a live device is a stop)" >&2
    return 1
  fi
  timeout -k 5 75 "$TESTD" del --id "$DEVID" --force >/dev/null 2>&1
}
td() { timeout -k 5 100 "$TESTD" "$@"; }       # client commands against the running daemon
tdc() { td "$1" --state "$STATE" "${@:2}"; }

mount_fs() { # fs dev mountpoint
  mkdir -p "$3"; safe_path "$3" || return 1
  mount -o nosuid,nodev "$2" "$3"
}
make_fs() { # fs dev  (df-guard first, W: every mkfs)
  safe_path "$2" || return 1
  df_guard "$LUNIX_SPIKE_DIR" "${MKFS_GIB:-8}" || return 1
  case "$1" in
    ext4) mkfs.ext4 -q -F "$2";;
    xfs) mkfs.xfs -q -f "$2";;
    btrfs) mkfs.btrfs -q -f "$2" >/dev/null;;
  esac
}
check_fs() { # fs dev  (unmounted); prints the exit code
  case "$1" in
    ext4) fsck.ext4 -fn "$2" >/dev/null 2>&1; echo $?;;
    xfs) xfs_repair -n "$2" >/dev/null 2>&1; echo $?;;
    btrfs) btrfs check --readonly "$2" >/dev/null 2>&1; echo $?;;
  esac
}

# Extra daemons of a multi-device cell: "pid:id" pairs.
EXTRAS=""
track_extra() { EXTRAS="$EXTRAS $1:$2"; }
untrack_extra() { EXTRAS=" $(echo "$EXTRAS" | sed "s/ $1:$2//")"; }
extras_teardown() {
  local e pid id i
  for e in $EXTRAS; do
    pid=${e%%:*}; id=${e##*:}
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null
      for i in $(seq 1 300); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
      kill -0 "$pid" 2>/dev/null && { echo "teardown: extra daemon $pid did not exit; device $id left" >&2; continue; }
    fi
    [ -e "/dev/ublkb$id" ] && timeout -k 5 75 "$TESTD" del --id "$id" --force >/dev/null 2>&1
  done
}

# teardown: the only exit path. Order: load, serving daemon, unmount, detach, delete.
teardown() {
  trap - EXIT INT TERM
  CRASH_AT=""   # a daemon started for the teardown must not carry a crash point
  local p
  for p in $FIO_PID $HB_PID $S1_PID $S2_PID; do kill -TERM "$p" 2>/dev/null; done
  for p in $FIO_PID $HB_PID $S1_PID $S2_PID; do
    local i; for i in $(seq 1 600); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
    kill -0 "$p" 2>/dev/null && echo "teardown: load process $p did not stop within 60 s (not killed)" >&2
  done
  if [ -e "${OUT:-/nonexistent}/WEDGED" ]; then
    echo "teardown: the run is marked WEDGED; only the load processes were stopped, nothing else touched" >&2
    return 1
  fi
  if [ -n "$DEVID" ] && [ -b "/dev/ublkb$DEVID" ] && { [ -z "$DPID" ] || ! kill -0 "$DPID" 2>/dev/null; }; then
    echo "teardown: no daemon serves $DEV; starting one to let the unmount finish"
    start_daemon recover || echo "teardown: recovery failed; device left for the leftovers check" >&2
  fi
  if [ -n "$MNT" ] && mountpoint -q "$MNT" 2>/dev/null; then
    timeout -k 5 90 umount "$MNT" || { echo "teardown: umount of $MNT failed or timed out; stopping here (nothing else touched)" >&2; return 1; }
  fi
  if stop_daemon_clean; then
    del_dev
  else
    echo "teardown: the daemon is still alive after the stop; the device is not deleted (stop of a live device)" >&2
  fi
  extras_teardown
  [ -n "$IMG" ] && rm -f "$IMG"
  return 0
}

# need_clean_start OUT: leftovers must be clean at the start
need_clean_start() {
  bash "$here/../../../../spikes/common/ublk-leftovers.sh" > "$1/leftovers-start.txt" || { cat "$1/leftovers-start.txt"; echo "not clean at start"; exit 2; }
  : > "$CREATED_DEVICES"   # ids are reused: only devices recorded by this run count
}
load_ublk() { lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }; }
