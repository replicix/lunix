#!/bin/bash
# P0.2 driver (root, scratch only). For each mode x work: a 16-queue daemon for the
# "main" matrix and a 1-queue daemon for the QD1 rows; then the headline cells with
# --workers 0 and --workers 4. Env: MODES, WORKS, OUT, DAEMON_CPUS, FIO_CPUS,
# RUNTIME, RAMP, REPS, PHASE (main|workers|both).
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"
require_root
: "${OUT:=$here/out}" "${DAEMON_CPUS:=0-3}" "${FIO_CPUS:=4-7}" "${MODES:=copy user-copy batch}"
: "${WORKS:=none hash hash+lz4}" "${RUNTIME:=4}" "${RAMP:=1}" "${REPS:=3}" "${PHASE:=both}"
export FIO_CPUS RUNTIME RAMP REPS
# 50 % compressible write payload (about 2:1 under LZ4; the daemon reports the measured ratio)
export FIO_EXTRA="--buffer_compress_percentage=50 --buffer_compress_chunk=4096"
BIN="$here/target/release/ublk-datapath"; M="$here/../common/matrices"
mkdir -p "$OUT"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-start.txt" || { echo "not clean at start"; exit 2; }
loaded=0; lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && loaded=1 && add_cleanup 'rmmod ublk_drv'; }
DPID=""
start_dp() { # logfile, add args...
  local log=$1; shift
  taskset -c "$DAEMON_CPUS" "$BIN" add "$@" --cpus "$DAEMON_CPUS" > "$log" 2>&1 &
  DPID=$!
  for i in $(seq 1 100); do grep -q '^READY' "$log" && break; sleep 0.1; done
  grep -q '^READY' "$log" || { echo "daemon not ready"; cat "$log"; return 1; }
  DEV=$(sed -n 's/^READY id=[0-9]* bdev=\([^ ]*\).*/\1/p' "$log")
  register_device "$DEV"; register_device "${DEV/ublkb/ublkc}"
}
stop_dp() {
  [ -n "$DPID" ] || return 0
  grep -E 'Vm(RSS|HWM)' /proc/$DPID/status > "$CUR_OUT/rss-before-stop.txt" 2>/dev/null
  kill -TERM "$DPID" 2>/dev/null
  for i in $(seq 1 100); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && { echo "daemon did not exit"; kill -KILL "$DPID"; }
  DPID=""
}
add_cleanup 'stop_dp'
if [ "$PHASE" != workers ]; then
for mode in $MODES; do for work in $WORKS; do
  d="$OUT/$mode/$work"; CUR_OUT=$d; mkdir -p "$d"
  start_dp "$d/daemon-main.log" --mode "$mode" --work "$work" --workers 0 --queues 16 --depth 256 || continue
  echo "== $mode $work main $(date +%T)"
  bash "$here/../common/fio-matrix.sh" "$DEV" "$d/main" "$M/dp-$work-main.txt" --daemon-pid "$DPID"; stop_dp
  start_dp "$d/daemon-qd1.log" --mode "$mode" --work "$work" --workers 0 --queues 1 --depth 128 || continue
  echo "== $mode $work qd1 $(date +%T)"
  bash "$here/../common/fio-matrix.sh" "$DEV" "$d/qd1" "$M/dp-$work-qd1.txt" --daemon-pid "$DPID"; stop_dp
done; done
fi
if [ "$PHASE" != main ]; then
for mode in $MODES; do for w in 0 4; do
  d="$OUT/$mode/hash-workers$w"; CUR_OUT=$d; mkdir -p "$d"
  start_dp "$d/daemon-main.log" --mode "$mode" --work hash --workers "$w" --queues 16 --depth 256 || continue
  echo "== $mode workers=$w main $(date +%T)"
  bash "$here/../common/fio-matrix.sh" "$DEV" "$d/main" "$M/dp-workers-main.txt" --daemon-pid "$DPID"; stop_dp
  start_dp "$d/daemon-qd1.log" --mode "$mode" --work hash --workers "$w" --queues 1 --depth 128 || continue
  echo "== $mode workers=$w qd1 $(date +%T)"
  bash "$here/../common/fio-matrix.sh" "$DEV" "$d/qd1" "$M/dp-workers-qd1.txt" --daemon-pid "$DPID"; stop_dp
done; done
fi
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
