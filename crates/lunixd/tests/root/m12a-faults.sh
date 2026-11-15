#!/bin/bash
# M12a root cells that need the lunix-ublk fault hooks (root; real ublk devices). LUNIXD must be a
# `--features crashpoints` build, which compiles the hooks in (production builds do not have them):
#
#   F1  START_DEV fails (LUNIX_UBLK_FAULT_START_DEV): the attach fails; when the device is left in the
#       kernel the volume becomes an orphan (tombstone: status read_only with the device, a second attach
#       is busy), and SIGTERM aborts for recovery; a new daemon without the fault recovers or replaces the
#       device and the volume attaches again;
#   F2  one held request (LUNIX_UBLK_FAULT_HOLD) makes QUIESCE_DEV time out (quiesce_timeout_ms = 500); the
#       hold is released after the first timeout, so the retry (a second QUIESCE_DEV on a device already
#       quiescing) succeeds: QuiesceAll answers quiesced, exit 0, the new daemon recovers the device and
#       fio verifies across it;
#   F3  two volumes, only b's device held (<hold>.<dev_id>) for good: a is quiesced, b fails twice, the
#       daemon aborts for recovery (exit 134, both devices kept); the hold is removed and a new daemon
#       recovers both, fio verifies on both.
#
# Binaries are built beforehand as the normal user. Disk: about 4 GiB peak; df-guard asks for 6.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs crashpoints build>/lunixd \
#       TESTD=<abs>/lunix-ublk-testd bash crates/lunixd/tests/root/m12a-faults.sh
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/m12a-lib.sh"
harness_init m12a-faults
df_guard "$LUNIX_SPIKE_DIR" "${FAULT_GIB:-6}" || exit 2
say "# M12a fault-hook cells ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus)"
"$LUNIXD" --list-crashpoints >/dev/null 2>&1; expect "LUNIXD is a crashpoints build (the hooks are compiled in)" $?
GIB=$((1 << 30))
HOLD="$OUT/hold"; FAILSTART="$OUT/fail-start"
export LUNIX_UBLK_FAULT_HOLD="$HOLD" LUNIX_UBLK_FAULT_START_DEV="$FAILSTART"
add_cleanup 'rm -f "$HOLD" "$HOLD".* "$FAILSTART"'
start_lunixd || exit 1
create_vol a $((2 * GIB)); expect "create volume a (2 GiB)" $?
create_vol b $((2 * GIB)); expect "create volume b (2 GiB)" $?

say ""; say "## F1: START_DEV fails"
touch "$FAILSTART"
out=$(ctl '{"method":"attach","params":{"name":"a","queues":2}}'); rc=$?
say "- attach reply (rc $rc): $out"
echo "$out" | grep -q '"attached"'; [ $? != 0 ]; expect "the attach fails while START_DEV fails" $?
st=$(ctl '{"method":"volume_status","params":{"name":"a"}}')
say "- status: $(echo "$st" | jq -c '.ok | {state, device}')"
rm -f "$FAILSTART"
grep -E 'left the device in the kernel|new device was deleted again' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
if grep -q 'left the device in the kernel' "$DLOG"; then
  say "- the failed start left a tombstone"
  echo "$st" | jq -e '.ok.state == "read_only" and (.ok.device // "" | length > 0)' >/dev/null; expect "status: read_only with the device" $?
  out=$(ctl '{"method":"attach","params":{"name":"a","queues":2}}')
  echo "$out" | grep -q '"busy"'; expect "a second attach is busy (restart lunixd)" $?
  ctl '{"method":"quiesce_all"}' >/dev/null 2>&1
  wait_daemon_exit 30; say "- QuiesceAll with an orphan: daemon exit status $DRC (134 = abort for recovery)"
  [ "$DRC" = 134 ]; expect "the daemon aborts for recovery with an orphan" $?
  sleep 2
  start_lunixd || wedged "lunixd did not restart after the orphan"
  grep -h 'recorded device\|recovered at start\|DEAD' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
else
  grep -q 'new device was deleted again' "$DLOG"; expect "no tombstone: the new device was deleted again" $?
fi
if [ -z "$(attached_dev a)" ]; then attach a 2; expect "a attaches without the fault" $?; else pass "a was recovered at start"; fi
DA=$(attached_dev a); [ -n "$DA" ] && [ -b "$DA" ]; expect "a has a device ($DA)" $?
FIO_RW=write start_fio f1w "$DA" 0 256m --bs=128k --iodepth=16 --io_size=256m; wait_fio "$LAST_FIO" 120
expect "write 256 MiB with verify to a (rc $?)" $?
detach a; expect "detach a" $?

say ""; say "## F2: a held request times QUIESCE_DEV out once; the retry succeeds"
stop_lunixd; expect "lunixd exits on SIGTERM with nothing attached" $?
sed -i 's/^quiesce_timeout_ms.*//' "$ST/lunixd.toml"; echo 'quiesce_timeout_ms = 500' >> "$ST/lunixd.toml"
start_lunixd || exit 1
attach a 2; expect "attach a" $?
DA=$(attached_dev a)
start_fio f2a "$DA" 0 1g --bs=64k --iodepth=8 --time_based --runtime=30 --rate=4m; FA=$LAST_FIO
sleep 3
touch "$HOLD"; sleep 1
( ctl '{"method":"quiesce_all"}' > "$OUT/f2.reply" 2>&1; echo $? > "$OUT/f2.rc" ) &
QP=$!
sleep 0.8   # past the first QUIESCE_DEV's 500 ms
rm -f "$HOLD"
wait "$QP"
say "- quiesce_all reply (rc $(cat "$OUT/f2.rc")): $(cat "$OUT/f2.reply")"
grep -q '"quiesced"' "$OUT/f2.reply"; expect "QuiesceAll succeeded after the retry" $?
wait_daemon_exit 30; expect "the daemon exited by itself (rc $DRC)" $?
[ "$DRC" = 0 ]; expect "exit code 0" $?
grep -E 'quiesce failed|retrying once|aborting for recovery|quiesced; exiting' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
grep -q 'retrying once' "$DLOG"; expect "the log shows the failed QUIESCE_DEV and the retry" $?
sleep 2
start_lunixd || wedged "lunixd did not restart after F2"
grep -q 'volume recovered at start.*recovered=true' "$DLOG"; expect "the new daemon recovered a" $?
wait_fio "$FA" 120; expect "fio on a verified across the held quiesce (rc $?)" $?
detach a; expect "detach a" $?

say ""; say "## F3: b held for good: a quiesced, b fails twice, abort for recovery"
attach a 2; expect "attach a" $?
attach b 2; expect "attach b" $?
DA=$(attached_dev a); DB=$(attached_dev b); IB=${DB#/dev/ublkb}
start_fio f3a "$DA" 0 1g --bs=64k --iodepth=8 --time_based --runtime=30 --rate=4m; FA=$LAST_FIO
start_fio f3b "$DB" 0 1g --bs=64k --iodepth=8 --time_based --runtime=30 --rate=4m; FB=$LAST_FIO
sleep 3
touch "$HOLD.$IB"; sleep 1
out=$(ctl '{"method":"quiesce_all"}'); rc=$?
say "- quiesce_all reply (rc $rc): $out"
for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
if kill -0 "$DPID" 2>/dev/null; then rm -f "$HOLD.$IB"; wedged "the daemon did not end with b held (hold released; stop and report)"; fi
wait "$DPID" 2>/dev/null; DRC=$?; DPID=""
say "- daemon exit status $DRC (134 = abort for recovery)"
[ "$DRC" = 134 ]; expect "abort for recovery with one device that cannot be quiesced" $?
grep -E 'quiesce failed|retrying once|aborting for recovery' "$DLOG" | cut -c1-260 | sed 's/^/    log: /' | tee -a "$R"
[ -b "$DA" ] && [ -b "$DB" ]; expect "both devices kept" $?
rm -f "$HOLD.$IB"
sleep 2
start_lunixd || wedged "lunixd did not restart after F3"
[ "$(grep -c 'volume recovered at start.*recovered=true' "$DLOG")" = 2 ]; expect "the new daemon recovered both" $?
wait_fio "$FA" 120; expect "fio on a verified (rc $?)" $?
wait_fio "$FB" 120; expect "fio on b verified across the abort (rc $?)" $?
detach a; expect "detach a" $?
detach b; expect "detach b" $?

say ""; say "## end"
stop_lunixd; expect "lunixd exits on SIGTERM" $?
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
