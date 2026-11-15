#!/bin/bash
# M12a root cells for the review's "needs a run" items (root; real ublk devices):
#
#   Q1  two volumes under fio load quiesced together (QuiesceAll), the daemon exits, a new daemon recovers
#       both devices (same ids), fio verifies across the exit;
#   Q2  QuiesceAll with quiesce_timeout_ms = 1 under heavy load on two volumes, repeated until it succeeds:
#       a failed first QUIESCE_DEV, then the retry (the driver's answer to a second QUIESCE_DEV on a device
#       that is already quiescing or quiesced), and the inline retry when only one of two fails;
#   S1  a stale state/device of a detached volume (the id is gone): the record is cleared at start and the
#       volume stays detached;
#   S2  the same, with the id reused by another volume (the other volume is recovered, untouched).
#
# (The tombstone of a device left by a failed START_DEV needs a fault hook in lunix-ublk: not run here.)
# Binaries are built beforehand as the normal user. Disk: about 6 GiB peak (no GC yet); df-guard asks for 8.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd \
#       TESTD=<abs>/lunix-ublk-testd bash crates/lunixd/tests/root/m12a-lifecycle.sh
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/m12a-lib.sh"
harness_init m12a-lifecycle
df_guard "$LUNIX_SPIKE_DIR" "${LIFE_GIB:-8}" || exit 2
say "# M12a lifecycle cells ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus)"
GIB=$((1 << 30))
start_lunixd || exit 1
create_vol a $((2 * GIB)); expect "create volume a (2 GiB)" $?
create_vol b $((2 * GIB)); expect "create volume b (2 GiB)" $?
DIR_A=$(vol_dir a); DIR_B=$(vol_dir b)

recovered_both() { # log-file
  [ "$(grep -c 'volume recovered at start.*recovered=true' "$1")" = 2 ]
}

say ""; say "## Q1: two volumes quiesced together under load"
attach a 2; expect "attach a (2 queues)" $?
attach b 2; expect "attach b (2 queues)" $?
DA=$(attached_dev a); DB=$(attached_dev b)
say "- devices: a $DA, b $DB"
start_fio q1a "$DA" 0 1g --bs=64k --iodepth=16 --time_based --runtime=40 --rate=8m; FA=$LAST_FIO
start_fio q1b "$DB" 0 1g --bs=64k --iodepth=16 --time_based --runtime=40 --rate=8m; FB=$LAST_FIO
sleep 8
kill -0 "$FA" && kill -0 "$FB"; expect "both fio runs are running before the quiesce" $?
out=$(ctl '{"method":"quiesce_all"}'); rc=$?
say "- quiesce_all reply (rc $rc): $out"
echo "$out" | grep -q '"devices":2'; expect "QuiesceAll quiesced 2 devices" $?
wait_daemon_exit 30; expect "the daemon exited by itself (rc $DRC)" $?
[ "$DRC" = 0 ]; expect "exit code 0" $?
[ -b "$DA" ] && [ -b "$DB" ]; expect "both devices survive the exit (quiesced, no stop command)" $?
sleep 2
start_lunixd || wedged "lunixd did not restart after the quiesce"
recovered_both "$DLOG"; expect "the new daemon recovered both devices at start" $?
grep -o 'stall_ms=[0-9]*' "$DLOG" | sed 's/^/    /' | tee -a "$R"
[ "$(attached_dev a)" = "$DA" ] && [ "$(attached_dev b)" = "$DB" ]; expect "same device ids after recovery" $?
wait_fio "$FA" 120; expect "fio on a finished with verify passing across the quiesce (rc $?)" $?
wait_fio "$FB" 120; expect "fio on b finished with verify passing across the quiesce (rc $?)" $?
detach a; expect "detach a" $?
detach b; expect "detach b" $?
for l in "$OUT/lunixd.1.log" "$DLOG"; do grep -E ' (ERROR|WARN) ' "$l" | cut -c1-300 | sed 's/^/    log: /' | head -5 | tee -a "$R"; done

say ""; say "## Q2: failed QUIESCE_DEV and its retries (quiesce_timeout_ms = 1)"
stop_lunixd; expect "lunixd exits on SIGTERM with nothing attached" $?
sed -i 's/^quiesce_timeout_ms.*//' "$ST/lunixd.toml"; echo 'quiesce_timeout_ms = 1' >> "$ST/lunixd.toml"
# One CPU for the daemon in this cell: in rounds 2 and 3 two busy loops of ours on that CPU starve the queue
# threads, so that started requests stay in the driver longer than the 1 ms drain.
OLD_CPUS=$DAEMON_CPUS; DAEMON_CPUS=2; sed -i "s/^cpus = .*/cpus = \"$DAEMON_CPUS\"/" "$ST/lunixd.toml"
HOGS=()
add_cleanup 'hogs_stop'
hogs_start() { local i; for i in 1 2; do taskset -c 2 bash -c 'while :; do :; done' & HOGS+=($!); done; }
hogs_stop() { local p; for p in "${HOGS[@]}"; do kill -KILL "$p" 2>/dev/null; wait "$p" 2>/dev/null; done; HOGS=(); }
start_lunixd || exit 1
attach a 2; attach b 2
DA=$(attached_dev a); DB=$(attached_dev b)
say "- devices: a $DA, b $DB"
# Round 1 is plain heavy load; rounds 2 and 3 add the busy loops on the daemon's CPU.
for round in 1 2 3; do
  HEAVY=(--bs=128k --iodepth=64 --io_size=512m)
  say ""; say "### Q2 round $round: both volumes heavily loaded (${HEAVY[*]})"
  start_fio "q2a$round" "$DA" 0 1g "${HEAVY[@]}"; FA=$LAST_FIO
  start_fio "q2b$round" "$DB" 0 1g "${HEAVY[@]}"; FB=$LAST_FIO
  [ "$round" -ge 2 ] && hogs_start
  sleep 2
  n=0; ok=0; DEXIT=0
  while [ $n -lt 8 ]; do
    n=$((n + 1))
    out=$(ctl '{"method":"quiesce_all"}'); rc=$?
    say "- try $n: rc $rc: $out"
    hogs_stop
    if echo "$out" | grep -q '"quiesced"'; then ok=1; break; fi
    kill -0 "$DPID" 2>/dev/null || break
    sleep 1
  done
  if kill -0 "$DPID" 2>/dev/null; then
    if [ "$ok" = 1 ]; then wait_daemon_exit 30 || DEXIT=1; else DEXIT=1; fi
  else
    wait "$DPID" 2>/dev/null; DRC=$?; DPID=""
  fi
  [ "${DEXIT:-0}" = 0 ] || wedged "the daemon neither answered success nor exited after $n tries (a device may be draining; stop and report)"
  say "- daemon ended after $n QuiesceAll call(s), exit status $DRC (0 = quiesced, 134 = abort for recovery)"
  grep -E 'quiesce failed|retrying once|aborting for recovery|quiesced; exiting|final commit' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
  sleep 2
  start_lunixd || wedged "lunixd did not restart after round $round"
  recovered_both "$DLOG"; expect "round $round: the new daemon recovered both devices at start" $?
  hogs_stop
  wait_fio "$FA" 180; expect "round $round: fio on a verified across the quiesce and recovery (rc $?)" $?
  wait_fio "$FB" 180; expect "round $round: fio on b verified (rc $?)" $?
done
detach a; detach b
stop_lunixd; expect "lunixd exits on SIGTERM with nothing attached" $?
DAEMON_CPUS=$OLD_CPUS; sed -i "s/^cpus = .*/cpus = \"$DAEMON_CPUS\"/" "$ST/lunixd.toml"
sed -i 's/^quiesce_timeout_ms.*//' "$ST/lunixd.toml"
start_lunixd || exit 1

say ""; say "## S1: stale state/device, the device id is gone"
attach a 2; expect "attach a" $?
DA=$(attached_dev a); IA=${DA#/dev/ublkb}
cp "$DIR_A/state/device" "$OUT/a.device.record"; [ -s "$OUT/a.device.record" ]; expect "copied a's device record (dev $IA)" $?
detach a; expect "detach a" $?
[ ! -e "$DIR_A/state/device" ]; expect "the record is removed by the detach" $?
for i in $(seq 1 100); do [ -e "$DA" ] || break; sleep 0.1; done
[ ! -e "$DA" ]; expect "the device is gone" $?
stop_lunixd; expect "lunixd exits on SIGTERM with nothing attached" $?
cp "$OUT/a.device.record" "$DIR_A/state/device"
start_lunixd || exit 1
grep -q 'the recorded device is gone' "$DLOG"; expect "start log: the recorded device is gone, record cleared" $?
grep -h 'recorded device' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
[ ! -e "$DIR_A/state/device" ]; expect "the stale record was removed" $?
[ -z "$(attached_dev a)" ]; expect "a stays detached (no device attached for it)" $?
[ ! -e "$DA" ]; expect "no device was created for the stale record" $?

say ""; say "## S2: stale state/device, the id reused by another volume"
attach a 2; expect "attach a" $?
DA=$(attached_dev a); IA=${DA#/dev/ublkb}
cp "$DIR_A/state/device" "$OUT/a.device.record2"
detach a; expect "detach a" $?
attach b 2; expect "attach b" $?
DB=$(attached_dev b); IB=${DB#/dev/ublkb}
say "- a had device $IA, b now has $IB"
[ "$IA" = "$IB" ]; expect "b took the id a's record names" $?
FIO_RW=write start_fio s2w "$DB" 0 256m --bs=128k --iodepth=16 --io_size=256m; wait_fio "$LAST_FIO" 120; expect "write 256 MiB with verify to b (rc $?)" $?
stop_lunixd; expect "SIGTERM: QuiesceAll quiesces b and the daemon exits" $?
cp "$OUT/a.device.record2" "$DIR_A/state/device"
start_lunixd || exit 1
[ "$(grep -c 'volume recovered at start.*recovered=true' "$DLOG")" = 1 ]; expect "b was recovered at start" $?
[ "$(attached_dev b)" = "$DB" ]; expect "b has the same device $DB" $?
grep -h 'recorded device\|recovered at start' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
[ ! -e "$DIR_A/state/device" ]; expect "a's stale record was removed" $?
[ -z "$(attached_dev a)" ]; expect "a stays detached" $?
taskset -c "$FIO_CPUS" fio --name=s2v --filename="$DB" --direct=1 --ioengine=libaio --rw=write --bs=128k --iodepth=16 --size=256m --verify=crc32c --verify_only=1 --output="$OUT/fio-s2v.txt" >/dev/null 2>&1
expect "verify-only of b's 256 MiB after the recovery (rc $?)" $?
detach b; expect "detach b" $?

say ""; say "## end"
stop_lunixd; expect "lunixd exits on SIGTERM" $?
du -sh "$VOLS" | tee -a "$R"
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
