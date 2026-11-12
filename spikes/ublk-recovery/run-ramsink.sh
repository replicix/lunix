#!/bin/bash
# P0.12 step 5: RAM-sink acknowledgement latency (4k randwrite, QD1 and QD32, raw device) against the
# plain file-backed target, and the replay time of a 256 MiB buffer (64k and 4k records).
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-ramsink}" "${IMG_GIB:=4}" "${SECS:=10}"
mkdir -p "$OUT"; QUEUES=1
IMG="$LUNIX_SPIKE_DIR/rec.img"; KSOCK="$LUNIX_SPIKE_DIR/run/keeper.sock"; need_safe "$IMG"; need_safe "$KSOCK"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
add_cleanup "rm -f $IMG $KSOCK"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'; add_cleanup 'pkill -f keeper.py'
python3 "$here/keeper/keeper.py" "$KSOCK" > "$OUT/keeper.log" 2>&1 &
for i in $(seq 1 50); do grep -q ready "$OUT/keeper.log" && break; sleep 0.1; done
R="$OUT/result.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
runfio() { # label bs qd -> json file
  taskset -c "$FIO_CPUS" timeout -k 5 $((SECS + 30)) fio --name=l --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randwrite --bs="$2" \
    --iodepth="$3" --numjobs=1 --time_based --runtime="$SECS" --ramp_time=1 --lat_percentiles=1 --norandommap --randrepeat=0 \
    --cpus_allowed=4 --output-format=json --output="$OUT/$1-$2-qd$3.json" >/dev/null 2>&1
  jq -r '.jobs[0].write | "\(.iops|floor) IOPS, lat p50 \(.lat_ns.percentile["50.000000"]/1000) us, p99 \(.lat_ns.percentile["99.000000"]/1000) us, p99.9 \(.lat_ns.percentile["99.900000"]/1000) us, mean \(.lat_ns.mean/1000|floor) us"' "$OUT/$1-$2-qd$3.json"
}
say "# P0.12 RAM-sink latency and replay"; say ""
say "## Acknowledgement latency (4k random write on the raw device, no FLUSH issued)"
for sink in file mem; do
  DEVID=""; rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"
  A=(); [ $sink = mem ] && A=(--sink mem --keeper "$KSOCK" --mem-mib 256)
  start_daemon "${A[@]}" || continue
  for qd in 1 32; do say "- sink=$sink QD$qd: $(runfio $sink 4k $qd)"; done
  stop_daemon_clean; del_dev; DEVID=""
done
say ""; say "## Replay of a 256 MiB buffer after kill -9 (no FLUSH before the kill)"
for bs in 64k 4k; do
  DEVID=""; rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"
  start_daemon --sink mem --keeper "$KSOCK" --mem-mib 512 || continue
  taskset -c "$FIO_CPUS" fio --name=fill --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randwrite --bs=$bs --iodepth=32 --numjobs=1 \
     --size=3g --io_size=250m --norandommap --randrepeat=0 --cpus_allowed=4 >/dev/null 2>&1
  kill -9 "$DPID"; wait "$DPID" 2>/dev/null
  start_daemon --recover --sink mem --keeper "$KSOCK" --mem-mib 512 || { say "- bs=$bs: recovery failed"; continue; }
  say "- bs=$bs: $(grep REPLAY "$DLOG") ; exec->recovered $(daemon_ms) ms"
  stop_daemon_clean; del_dev; DEVID=""
done
rm -f "$IMG"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
