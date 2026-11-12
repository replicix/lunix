#!/bin/bash
# P0.3 step 4 (optional watchdog check): behaviour of I/O while the server is away, for a device created
# with USER_RECOVERY_FAIL_IO (expect EIO within a moment) versus REISSUE (expect I/O to queue, no error).
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-failio}" "${IMG_GIB:=2}"
mkdir -p "$OUT"
IMG="$LUNIX_SPIKE_DIR/rec.img"; need_safe "$IMG"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
add_cleanup "rm -f $IMG"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'
R="$OUT/result.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
say "# I/O while the server is away"
for variant in fail-io reissue; do
  DEVID=""; rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"
  A=(); [ $variant = fail-io ] && A=(--fail-io)
  start_daemon "${A[@]}" || { say "- $variant: start failed"; continue; }
  say ""; say "## $variant (dev_info flags: $(grep -o 'DEVINFO after-add.*' "$DLOG" | head -1 | grep -o 'flags=.*'))"
  ( timeout -k 5 60 fio --name=f --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randwrite --bs=4k --iodepth=8 --time_based --runtime=25 \
      --cpus_allowed="$FIO_CPUS" --output-format=json --output="$OUT/$variant.json" > "$OUT/$variant.stdout" 2>&1; echo $? > "$OUT/$variant.rc"; date +%s%N > "$OUT/$variant.end" ) &
  FPID=$!
  sleep 3; T0=$(now_ns); kill -9 "$DPID"; wait "$DPID" 2>/dev/null
  sleep 8
  if [ -s "$OUT/$variant.rc" ]; then
    say "- fio ended $(( ($(cat $OUT/$variant.end) - T0) / 1000000 )) ms after the server was killed, exit code $(cat $OUT/$variant.rc); $(grep -iE 'error|Input/output' $OUT/$variant.stdout | head -2 | tr '\n' ';')"
  else
    say "- 8 s after kill -9 fio is still running with no error (I/O queued while the server is away)"
  fi
  start_daemon --recover "${A[@]}" || say "- recovery failed"
  say "- recovered after $(daemon_ms) ms (exec to END_USER_RECOVERY)"
  wait "$FPID"
  say "- final fio exit code: $(cat $OUT/$variant.rc); json errors: $(jq -c '[.jobs[].total_err]' $OUT/$variant.json 2>/dev/null || echo n/a)"
  rm -f "$OUT/$variant.rc"
  stop_daemon_clean; del_dev; DEVID=""
done
rm -f "$IMG"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
