#!/bin/bash
# Observer for a fio-verify cell: kernel timings of the ublk device (bpftrace) and a 1 Hz sample of the daemon (status, threads, disk).
# usage (root): diag-sidecar.sh OUTDIR LUNIX_BIN SOCKET VOLUME [LONG_MS]
# It starts when a ublk device appears and stops 30 s after the daemon is gone. It changes nothing in the run.
out=$1; lunix=$2; sock=$3; vol=$4; long=${5:-3000}; here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
for i in $(seq 1 7200); do ls /dev/ublkb[0-9]* >/dev/null 2>&1 && pgrep -x lunixd >/dev/null && break; sleep 1; done
dev=$(ls /dev/ublkb[0-9]* | head -1); b=$(basename "$dev")
mm=$(stat -c '%t:%T' "$dev"); maj=$((16#${mm%%:*})); min=$((16#${mm##*:})); devno=$(( (maj << 20) | min ))
echo "device $dev devno $devno long_ms $long" > "$out/sidecar.txt"
sed "s/DEVNO/$devno/g; s/LONG_MS/$long/g" "$here/ublk-lat.bt" > "$out/ublk-lat.bt"
bpftrace "$out/ublk-lat.bt" > "$out/bpf.txt" 2>"$out/bpf.err" &
BP=$!
root=$(lsblk -no PKNAME "$(findmnt -no SOURCE /)" 2>/dev/null | head -1)
while pgrep -x lunixd >/dev/null; do
  pid=$(pgrep -x lunixd | head -1)
  now=$(date +%s.%N)
  {
    echo "T $now"
    timeout 5 "$lunix" --socket "$sock" status "$vol" --json 2>/dev/null | jq -c '{state:.throttle.state,rate:.throttle.rate,dw:.throttle.delayed_writes,dmax:.throttle.delay_max_ms,sw:.throttle.space_waiters,pw:.throttle.paced_waiters,wr:.stats.ops_write,bw:.stats.bytes_written,fl:.stats.ops_flush,commits:.stats.commits,ju:.journal_used,jc:.journal_capacity,cut_ms:.write_path.last_cut_ms,cut_b:.write_path.last_cut_bytes,rw:.write_path.ring_waiters,rp:.write_path.ring_parked_writes,rwmax60:.write_path.ring_wait_max_60s_ms,pace:.write_path.ring_pace_rate,pmax:.write_path.ring_pace_max_ms,win:.write_path.window_resident_bytes,gcp:.throttle.gc_pressure,used:.space.used,lat:.write_path.write_latency_log2_us}' | sed 's/^/S /'
    # threads: name, state, cpu ticks
    for t in /proc/$pid/task/*; do
      comm=$(cat "$t/comm" 2>/dev/null) || continue
      rest=$(sed 's/.*) //' "$t/stat" 2>/dev/null)
      set -- $rest
      echo "H $(basename "$t") ${comm// /_} $1 $(( ${12:-0} + ${13:-0} )) $(cat "$t/wchan" 2>/dev/null)"
    done
    echo "I $b $(cat /sys/block/$b/inflight 2>/dev/null)"
    grep -E " ($b|$root) " /proc/diskstats | sed 's/^/D /'
  } >> "$out/samples.txt"
  sleep 1
done
sleep 30; kill "$BP" 2>/dev/null; wait "$BP" 2>/dev/null
echo done >> "$out/sidecar.txt"
