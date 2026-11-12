#!/bin/bash
# P0.1 driver (root, scratch only). Phases via PHASES (default: all):
#   nullblk   null_blk reference, full 72-cell matrix
#   ublknull  ublk-null, full 72-cell matrix
#   qd1       1-queue QD1 rows for null_blk and ublk-null
#   loop      kernel loop vs ublk-loop, reduced matrix
# CPU split: daemon queue threads on DAEMON_CPUS, fio on FIO_CPUS (disjoint).
# 16 queues so that each fio CPU maps to its own hardware queue (blk-mq maps
# CPUs to queues in contiguous groups; see RESULTS.md).
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"
require_root
: "${OUT:=$here/out}" "${DAEMON_CPUS:=0-3}" "${FIO_CPUS:=4-7}" "${PHASES:=nullblk ublknull qd1 loop}"
: "${RUNTIME:=5}" "${RAMP:=1}" "${REPS:=3}" "${LOOP_GIB:=16}" "${QUEUES:=16}" "${DEPTH:=256}"
export FIO_CPUS RUNTIME RAMP REPS
BIN="$here/target/release/ublk-spike"
M="$here/../common/matrices"
: "${BASE_MATRIX:=$M/baseline.txt}" "${LOOP_MATRIX:=$M/baseline-loop.txt}"
mkdir -p "$OUT"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-start.txt" || { echo "not clean at start"; exit 2; }

loaded_ublk=0; loaded_nb=0
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && loaded_ublk=1; }
[ $loaded_ublk = 1 ] && add_cleanup 'rmmod ublk_drv'
[ -c /dev/ublk-control ] || skip "no /dev/ublk-control"
DPID=""
start_ublk() { # args to `add`; sets DPID and DEV
  taskset -c "$DAEMON_CPUS" "$BIN" add "$@" > "$OUT/daemon.log" 2>&1 &
  DPID=$!
  for i in $(seq 1 100); do grep -q '^READY' "$OUT/daemon.log" && break; sleep 0.1; done
  grep -q '^READY' "$OUT/daemon.log" || { echo "daemon did not become ready"; cat "$OUT/daemon.log"; return 1; }
  DEV=$(sed -n 's/^READY id=[0-9]* bdev=\([^ ]*\).*/\1/p' "$OUT/daemon.log")
  register_device "$DEV"; register_device "${DEV/ublkb/ublkc}"
}
stop_ublk() {
  [ -n "$DPID" ] && kill -TERM "$DPID" 2>/dev/null
  for i in $(seq 1 100); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && { echo "daemon did not exit"; kill -KILL "$DPID"; }
  DPID=""
}
add_cleanup 'stop_ublk'
load_nb() { # submit_queues
  modprobe null_blk nr_devices=1 gb=64 bs=4096 submit_queues="$1" queue_mode=2 irqmode=0 completion_nsec=0 && loaded_nb=1
  register_device /dev/nullb0
}
unload_nb() { rmmod null_blk 2>/dev/null; }
run() { # name device matrix [daemon-pid]
  echo "== $1 $(date +%T)"; local extra=()
  [ -n "$4" ] && extra=(--daemon-pid "$4")
  bash "$here/../common/fio-matrix.sh" "$2" "$OUT/$1" "$3" "${extra[@]}"
}
for ph in $PHASES; do
  case "$ph" in
  nullblk)
    load_nb "$QUEUES"; run nullblk /dev/nullb0 "$BASE_MATRIX"; unload_nb;;
  ublknull)
    start_ublk --target null --queues "$QUEUES" --depth "$DEPTH" --cpus "$DAEMON_CPUS" --size-gib 64 || exit 1
    run ublknull "$DEV" "$BASE_MATRIX" "$DPID"; stop_ublk;;
  qd1)
    grep -E '^[0-9]+[kM] [a-z]+ 1 1$' "$BASE_MATRIX" > "$OUT/qd1.txt"
    load_nb 1; run nullblk-1q /dev/nullb0 "$OUT/qd1.txt"; unload_nb
    start_ublk --target null --queues 1 --depth 128 --cpus "$DAEMON_CPUS" --size-gib 64 || exit 1
    run ublknull-1q "$DEV" "$OUT/qd1.txt" "$DPID"; stop_ublk;;
  loop)
    img="$LUNIX_SPIKE_DIR/loop-backing.img"; need_safe "$img"
    df -h "$LUNIX_SPIKE_DIR" | tail -1
    fallocate -l "${LOOP_GIB}G" "$img"
    dd if=/dev/zero of="$img" bs=4M count=$((LOOP_GIB * 256)) oflag=direct conv=notrunc status=none
    add_cleanup "rm -f $img"
    export SIZE="${LOOP_GIB}g"
    lo=$(losetup --direct-io=on -f --show "$img"); register_device "$lo"
    add_cleanup "losetup -d $lo"
    run kloop "$lo" "$LOOP_MATRIX"
    losetup -d "$lo"
    start_ublk --target loop --backing "$img" --queues "$QUEUES" --depth "$DEPTH" --cpus "$DAEMON_CPUS" || exit 1
    run ublkloop "$DEV" "$LOOP_MATRIX" "$DPID"; stop_ublk
    rm -f "$img"; unset SIZE;;
  esac
done
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
