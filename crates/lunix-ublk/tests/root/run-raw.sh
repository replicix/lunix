#!/bin/bash
# M10 root cells on an UNMOUNTED device (plan order: raw first, then mounted).
#   features table vs phase 0 | ADD_DEV + sysfs | raw fio verify | kill -9 under raw load |
#   QUIESCE handover under raw load | watchdog (stuck control call) | grow + rejected sizes |
#   identity refusals | USER_COPY | detach (TRY_STOP_DEV) and delete.
# Run through the wrapper (it holds the host lock and runs the leftovers check before and after):
#   tools/remote.sh root env LUNIX_SPIKE_DIR=... TESTD=... bash crates/lunix-ublk/tests/root/run-raw.sh
# Needs: root, LUNIX_ROOT_TESTS=1, the release build of lunix-ublk-testd (features crashpoints), fio, jq.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/out-raw}" "${KILL_CYCLES:=3}" "${QUIESCE_CYCLES:=3}"
rm -rf "$OUT"; mkdir -p "$OUT"
MAX_GIB=4
STATE="$LUNIX_SPIKE_DIR/state/raw"; IMG="$LUNIX_SPIKE_DIR/raw.img"; need_safe "$IMG"; need_safe "$STATE"
need_clean_start "$OUT"
script_cap "${SCRIPT_CAP:-3000}"   # the script's own cap (W8); the wrapper timeout is larger
load_ublk
rm -rf "$STATE"; mkdir -p "$STATE"; rm -f "$IMG"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
R="$OUT/results.md"; : > "$R"
say() { echo "$*" | tee -a "$R"; }
FAIL=0
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { # name condition-rc
  if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi
}
sysq() { cat "/sys/block/ublkb$DEVID/queue/$1"; }
dev_bytes() { blockdev --getsize64 "$DEV"; }
info_line() { td info --id "$DEVID" | tr '\n' ' '; }

fio_raw_bg() { # name runtime [extra fio args]  (time_based: bounded by --runtime and a fixed 1 GiB working set)
  local name=$1 rt=$2; shift 2
  timeout -k 10 $((rt + 180)) fio --name="$name" --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randrw --rwmixwrite=70 \
      --bssplit=4k/50:16k/30:64k/20 --iodepth=16 --numjobs=1 --size=1g --time_based --runtime="$rt" \
      --verify=crc32c --verify_backlog=64 --verify_fatal=1 --do_verify=1 --norandommap --cpus_allowed="$FIO_CPUS" \
      --output-format=json --output="$OUT/$name.json" "$@" > "$OUT/$name.out" 2>&1 &
  FIO_PID=$!
}
fio_wait() { # name -> sets FIO_RC, prints errors
  wait "$FIO_PID"; FIO_RC=$?; FIO_PID=""
  FIO_ERR=$(jq -c '[.jobs[].error]' "$OUT/$1.json" 2>/dev/null || echo n/a)
}
fio_verify_once() { # name [extra]: bounded write + verify (io_size, no time_based; 64 MiB because the shared host disk is slow)
  local name=$1; shift
  timeout -k 10 300 fio --name="$name" --filename="$DEV" --direct=1 --ioengine=io_uring --rw=randwrite --bs=4k --iodepth=16 \
      --size=64m --io_size=64m --verify=crc32c --do_verify=1 --verify_fatal=1 --cpus_allowed="$FIO_CPUS" \
      --output-format=json --output="$OUT/$name.json" "$@" > "$OUT/$name.out" 2>&1
}

say "# M10 raw cells ($(date -u +%FT%TZ), kernel $(uname -r))"; say ""

say "## features table"
td features > "$OUT/features.txt"; rc=$?
cat "$OUT/features.txt" >> "$R"
exp="$here/../../../../bench/ublk-recovery/RESULTS.md"
rows() { awk -F'|' '/^\| UBLK_F_/ { for (i = 2; i <= 4; i++) { gsub(/^ +| +$/, "", $i) } print $2 "|" $3 "|" $4 }' "$1" | head -19; }
diff <(rows "$exp") <(rows "$OUT/features.txt") > "$OUT/features.diff"; drc=$?
expect "feature table (flag, bit, set) equals the phase 0 table; mandatory flags present (rc $rc)" $((rc + drc))

say ""; say "## add (copy path), parameters, bounded raw verify"
df_guard "$LUNIX_SPIKE_DIR" 4 || exit 2
start_daemon run --create-gib 2 || { say "start failed"; exit 1; }
say "- device $DEV, READY after $(daemon_ms) ms"
td info --id "$DEVID" | tee -a "$R"
for kv in write_cache:"write back" fua:1 logical_block_size:4096 physical_block_size:4096 minimum_io_size:4096 \
          optimal_io_size:65536 max_sectors_kb:1024 discard_granularity:4096 rotational:0; do
  k=${kv%%:*}; v=${kv#*:}; got=$(sysq "$k")
  [ "$got" = "$v" ]; expect "sysfs $k = $v (got $got)" $?
done
[ "$(dev_bytes)" = $((2 << 30)) ]; expect "device size 2 GiB" $?
[ -s "$STATE/device" ]; expect "state/device written" $?
fio_verify_once rawverify; expect "raw fio write+verify 64 MiB (rc $?)" $?
counters=$(tdc cmd counters); say "- $counters"
echo "$counters" | grep -Eq 'writes=[1-9][0-9]*'; expect "counters show writes" $?
echo "$counters" | grep -q 'in_flight=0'; expect "no request in flight when idle" $?

say ""; say "## kill -9 under raw load x$KILL_CYCLES (REISSUE)"
fio_raw_bg rawkill $((KILL_CYCLES * 12 + 40))
sleep 5
for c in $(seq 1 "$KILL_CYCLES"); do
  kill -9 "$DPID"; wait "$DPID" 2>/dev/null
  start_daemon recover || { fail "recovery $c failed"; break; }
  say "- cycle $c: recovered, exec to END_USER_RECOVERY $(daemon_ms) ms"
  sleep 6
done
fio_wait rawkill
[ "$FIO_RC" = 0 ]; expect "fio rc 0 after $KILL_CYCLES kill -9 cycles, errors $FIO_ERR" $?

# qcycles LABEL N [recover args...]: N QUIESCE handovers under raw fio. The true drain is checked on every
# cycle: the kernel has no started request left when the daemon exits (sysfs inflight read by the daemon
# right after the drain), the exit code is 0, and the handover is quick (no 20 s stall).
qcycles() {
  local label=$1 n=$2; shift 2
  fio_raw_bg "raw$label" $((n * 14 + 40))
  sleep 5
  local c old t0 t1 ms ki dc
  for c in $(seq 1 "$n"); do
    old=$DPID
    t0=$(now_ns)
    tdc quiesce --timeout-ms 8000 > "$OUT/quiesce.$label.$c.txt" 2>&1; qrc=$?
    for i in $(seq 1 300); do kill -0 "$old" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$old" 2>/dev/null; then wedged "daemon $old did not exit within 30 s of QUIESCE (cycle $c); stopped, nothing killed"; fi
    wait "$old"; xrc=$?
    t1=$(now_ns); ms=$(( (t1 - t0) / 1000000 ))
    ki=$(sed -n 's/.*kernel_inflight=\([0-9,]*\).*/\1/p' "$OUT/quiesce.$label.$c.txt" | head -1)
    dc=$(sed -n 's/.*drain_commits=\([0-9]*\).*/\1/p' "$OUT/quiesce.$label.$c.txt" | head -1)
    [ "$qrc" = 0 ] && [ "$xrc" = 0 ] && [ "$ki" = "0,0" ] && [ "$ms" -lt 8000 ]
    expect "$label cycle $c: quiesce rc $qrc, exit code $xrc, kernel inflight $ki, drain commits $dc, $ms ms" $?
    state=$(td info --id "$DEVID" | sed -n 's/.*state=\([0-9]*\).*/\1/p' | head -1)
    say "  device state after exit: $state (2 = QUIESCED)"
    start_daemon recover "$@" || { fail "recovery $c failed"; break; }
    say "  recovered in $(daemon_ms) ms"
    sleep 6
  done
  fio_wait "raw$label"
  [ "$FIO_RC" = 0 ]; expect "$label: fio rc 0 after $n handovers, errors $FIO_ERR" $?
}

say ""; say "## QUIESCE handover under raw load x$QUIESCE_CYCLES (worker-pool engine; daemon exits, no stop command)"
qcycles quiesce "$QUIESCE_CYCLES"
say ""; say "## QUIESCE handover with an inline-completion engine (--workers 0, like the M11 engine) x$QUIESCE_CYCLES"
# move the device onto the inline engine through one more handover, then cycle
tdc quiesce --timeout-ms 8000 > "$OUT/quiesce.switch.txt" 2>&1; old=$DPID
for i in $(seq 1 300); do kill -0 "$old" 2>/dev/null || break; sleep 0.1; done
wait "$old"
start_daemon recover --workers 0 || { fail "start with the inline engine failed"; exit 1; }
qcycles inline "$QUIESCE_CYCLES" --workers 0
say ""; say "## QUIESCE with requests held by the engine (--io-delay-ms 40) x$QUIESCE_CYCLES: the drain commits them"
tdc quiesce --timeout-ms 8000 > "$OUT/quiesce.switch2.txt" 2>&1; old=$DPID
for i in $(seq 1 300); do kill -0 "$old" 2>/dev/null || break; sleep 0.1; done
wait "$old"
start_daemon recover --io-delay-ms 40 || { fail "start with a delaying engine failed"; exit 1; }
qcycles held "$QUIESCE_CYCLES" --io-delay-ms 40
held=$(cat "$OUT"/quiesce.held.*.txt | sed -n 's/.*drain_commits=\([0-9]*\).*/\1/p' | paste -sd+ | bc)
[ "${held:-0}" -gt 0 ]; expect "requests were held at the quiesce and committed by the layer's own drain (total $held)" $?
# back to the plain engine for the remaining cells (the delaying engine makes verify runs slow)
tdc quiesce --timeout-ms 8000 > "$OUT/quiesce.switch3.txt" 2>&1; old=$DPID
for i in $(seq 1 300); do kill -0 "$old" 2>/dev/null || break; sleep 0.1; done
wait "$old"
start_daemon recover || { fail "start with the plain engine failed"; exit 1; }
grep -q 'STOP' "$OUT"/daemon.*.log; [ $? -ne 0 ]; expect "no daemon log mentions a stop command" $?

say ""; say "## watchdog: a deliberately stuck control call"
out=$(tdc cmd stuck-ctl 1500); say "- $out"
echo "$out" | grep -q '^TIMEOUT op=GET_DEV_INFO'; expect "reported at the timeout" $?
kill -0 "$DPID" 2>/dev/null; expect "daemon still running" $?
dd if="$DEV" of=/dev/null bs=4k count=64 iflag=direct 2>/dev/null; expect "the device still serves reads" $?
fio_verify_once rawafterstuck; expect "raw verify after the stuck call (rc $?)" $?

say ""; say "## grow 2 -> 4 GiB and rejected sizes"
before=$(dev_bytes)
tdc grow --bytes $((4 << 30)) | tee -a "$R" | grep -q '^OK'; expect "grow to 4 GiB accepted" $?
[ "$(dev_bytes)" = $((4 << 30)) ]; expect "device size is 4 GiB without a remount (was $before)" $?
python3 -I - "$STATE/device" <<'PY'
import struct, sys
r = open(sys.argv[1], "rb").read()
(sec,) = struct.unpack_from("<Q", r, 56)
sys.exit(0 if sec == (4 << 30) >> 9 else 1)
PY
expect "state/device holds the new size" $?
for bad in "$(( (4 << 30) + 512 ))" "$(( 1 << 30 ))" "$(( 8 << 30 ))" 0 "$(( (4 << 30) - 4096 ))"; do
  res=$(tdc grow --bytes "$bad" 2>&1); rc=$?
  [ "$rc" != 0 ] && [ "$(dev_bytes)" = $((4 << 30)) ]; expect "grow $bad refused (rc $rc: $res); size unchanged" $?
done
tdc grow --bytes $((4 << 30)) | grep -q '^OK'; expect "grow to the current size is a no-op" $?
fio_verify_once rawgrown --offset=3g --size=64m --io_size=64m; expect "write+verify in the grown region (rc $?)" $?

say ""; say "## identity: refused records leave the device alone"
kill -9 "$DPID"; wait "$DPID" 2>/dev/null; sleep 2
cp "$STATE/device" "$STATE/device.good"
base=$(info_line); say "- device after kill -9: $base"
for variant in "boot=flip" "queues=2" "sectors=+8" "flags=0x80" "depth=64"; do
  python3 -I "$here/mkrec.py" "$STATE/device.good" "$STATE/device" $variant
  start_daemon recover; rc=$?
  now=$(info_line)
  [ "$rc" = 3 ] && [ "$now" = "$base" ] && grep -q '^REFUSED identity' "$DLOG"
  expect "$variant: refused (rc $rc), device info unchanged" $?
  say "  $(grep '^REFUSED' "$DLOG")"
done
cp "$STATE/device.good" "$STATE/device"
start_daemon recover; expect "the genuine record recovers the device" $?
sleep 1
fio_verify_once rawafteridentity; expect "raw verify after the identity tests (rc $?)" $?

say ""; say "## detach (TRY_STOP_DEV) and delete"
id=$DEVID
tdc detach | tee -a "$R" | grep -q '^OK'; expect "detach answered OK" $?
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
kill -0 "$DPID" 2>/dev/null; [ $? -ne 0 ]; expect "daemon exited" $?
wait "$DPID"; say "- daemon exit code $?"; DPID=""
[ ! -e "/dev/ublkb$id" ]; expect "/dev/ublkb$id is gone" $?
grep -q '^DETACHED' "$OUT"/daemon.*.log; expect "DETACHED reported" $?
DEVID=""

say ""; say "## USER_COPY (opt-in): raw verify, kill -9, recover, detach"
rm -f "$IMG"; rm -rf "$STATE"; mkdir -p "$STATE"
start_daemon run --create-gib 2 --user-copy || { say "start failed"; exit 1; }
flags=$(td info --id "$DEVID" | sed -n 's/.*flags=\(0x[0-9a-f]*\).*/\1/p' | head -1)
[ $(( flags & 0x80 )) -ne 0 ]; expect "USER_COPY flag set on the device ($flags)" $?
fio_verify_once rawcopy; expect "raw write+verify over USER_COPY (rc $?)" $?
fio_raw_bg rawcopykill 40
sleep 5; kill -9 "$DPID"; wait "$DPID" 2>/dev/null
start_daemon recover || fail "USER_COPY recovery failed"
fio_wait rawcopykill
[ "$FIO_RC" = 0 ]; expect "fio rc 0 across a kill -9 with USER_COPY, errors $FIO_ERR" $?
tdc detach | grep -q '^OK'; expect "USER_COPY detach" $?
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
wait "$DPID" 2>/dev/null; DPID=""; DEVID=""

say ""; say "overall FAIL=$FAIL"
echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
