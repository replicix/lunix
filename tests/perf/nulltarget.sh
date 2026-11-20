#!/bin/bash
# ADR-33 T6, W9: the ublk framework ceiling for cache-hit reads: the r4k and seqr patterns of Part A on the ublk null target
# of spikes/ublk-baseline (completes every request at once), same queues (QUEUES, 4), depth 128, queue CPUs and fio CPUs as
# the Lunix cells. Root, through tools/remote.sh root:
#   tools/remote.sh root env LUNIX_SPIKE_DIR=<abs scratch> NULLSPIKE=<ublk-spike binary> bash tests/perf/nulltarget.sh
# Writes $OUT/nulltarget.txt ("cell iops bw_bytes", one line per cell: the median of REPS runs) and the fio JSON under raw/.
here="$(cd "$(dirname "$0")" && pwd)"
: "${NULLSPIKE:?NULLSPIKE is not set}" "${QUEUES:=4}" "${QCPUS:=0-3}" "${FIO_CPUS:=8-15}" "${JOBS:=8}" "${REPS:=3}" "${RUNTIME:=30}" "${RAMP:=5}" "${WS_GIB:=4}"
: "${LUNIX_SPIKE_DIR:?LUNIX_SPIKE_DIR is not set}"
. "$here/../../crates/lunix-ublk/tests/root/lib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/t6-null-out}"; need_safe "$OUT"; mkdir -p "$OUT/raw"
bash "$here/../../spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { echo "not clean at start"; exit 2; }
: > "$CREATED_DEVICES"
script_cap 1800
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
DPID=""
stop_null() { [ -n "$DPID" ] && kill -TERM "$DPID" 2>/dev/null; for _i in $(seq 1 100); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done; DPID=""; }
add_cleanup stop_null
taskset -c "$QCPUS" "$NULLSPIKE" add --target null --queues "$QUEUES" --depth 128 --cpus "$QCPUS" --size-gib 32 > "$OUT/daemon.log" 2>&1 &
DPID=$!
for _i in $(seq 1 100); do grep -q '^READY' "$OUT/daemon.log" && break; sleep 0.1; done
DEV=$(sed -n 's/^READY id=[0-9]* bdev=\([^ ]*\).*/\1/p' "$OUT/daemon.log"); [ -n "$DEV" ] || { echo "null target did not start"; cat "$OUT/daemon.log"; exit 1; }
register_device "$DEV"; register_device "${DEV/ublkb/ublkc}"
WS=$((WS_GIB << 30)); SLICE=$((WS / JOBS))
export DEV WS SLICE RUNTIME RAMP CPUS="$FIO_CPUS" JOBS
: > "$OUT/nulltarget.txt"
for c in r4k seqr; do
  qd=128; [ "$c" = seqr ] && qd=32
  vals=()
  for rep in $(seq 1 "$REPS"); do
    export QD=$qd SEED=$((200 + rep)) LATLOG="$OUT/raw/$c-rep$rep.lat"
    timeout -k 30 $((RUNTIME + RAMP + 120)) fio "$here/../../bench/phase1/perf/fio/$c.fio" --output-format=json --output="$OUT/raw/fio-$c-rep$rep.json" > "$OUT/raw/fio-$c-rep$rep.stdout" 2>&1
    vals+=("$(sed -n '/^{/,$p' "$OUT/raw/fio-$c-rep$rep.json" | jq -r '.jobs[0] | [(.read.iops + .write.iops), (.read.bw_bytes + .write.bw_bytes)] | @tsv')")
  done
  printf '%s\n' "${vals[@]}" | sort -k1 -n | sed -n "$(((REPS + 1) / 2))p" | awk -v c="$c" '{print c, $1, $2}' >> "$OUT/nulltarget.txt"
done
stop_null
[ -b "$DEV" ] && [ -n "${TESTD:-}" ] && timeout -k 5 75 "$TESTD" del --id "${DEV#/dev/ublkb}" --force >/dev/null 2>&1; true
cat "$OUT/nulltarget.txt"
