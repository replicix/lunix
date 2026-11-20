#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 full-volume cell (ADR-32, research note 2026-11-18-write-backpressure, section 9.4): a cap (`cache_max`) below the incompressible
# working set. Checked in order:
#   1. fio sees ENOSPC; no write completes later than full_timeout + 5 s; no hung-task line in the kernel log;
#   2. the gate says `full` (status `throttle.state`);
#   3. `blkdiscard` of half the device brings writes back within 90 s;
#   4. SIGTERM with writes parked: the daemon exits within quiesce_timeout_ms (30 s);
#   5. kill -9 with writes parked, `space.on_full = "error"` written to volume.json, restart: fio ends (with errors) and the device is
#      usable, with NO forced device deletion (the script never calls `del`);
#   6. what ext4 and xfs do with the error (recorded, not judged: the UNVERIFIED claims of section 5.5).
# Disk: the cap is 8 GiB, the working set 16 GiB; the store stays near 8 GiB plus the journal ring.
here="$(cd "$(dirname "$0")" && pwd)"
: "${CAP:=8G}" "${WS_GIB:=16}" "${FULL_TIMEOUT_S:=5}" "${CHUNK:=65536}" "${VOLMODE:=integrity}" "${VOL_GIB:=32}"
export SCRIPT_CAP="${SCRIPT_CAP:-6000}"
. "$here/lib.sh"
harness_init m15-fullvol
say "# M15 full-volume cell ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1)): cap $CAP, working set $WS_GIB GiB, full_timeout ${FULL_TIMEOUT_S}s"
VOL=fullv; D="$OUT/raw/fullvol"; mkdir -p "$D"
hung() { dmesg | grep -ciE 'hung task|blocked for more than'; }
# the gate state: `throttle.state` of the status (T5 of ADR-32); a build without those fields is judged by the ledger (used within the headroom of the limit)
gate_state() { lx status "$VOL" --json 2>/dev/null | jq -r '.throttle.state // empty' | grep . || lx space "$VOL" --json 2>/dev/null | jq -r '(if (.figures.used + (.settings.headroom // 0)) >= .figures.limit then "full-by-ledger" else "open" end)'; }
cell_begin "$VOL" "$VOL_GIB" "$CHUNK" "$VOLMODE"
lx space "$VOL" --cache-max "$CAP" --on-full wait --full-timeout "${FULL_TIMEOUT_S}s" --json > "$D/space-set.json" 2>&1; expect "set cache_max $CAP, on_full wait, full_timeout ${FULL_TIMEOUT_S}s" $? || true
H0=$(hung)
fill_job() { # label size_gib runtime_s : incompressible sequential write that carries on after errors
  taskset -c "$FIO_CPUS" timeout -k 30 "$3" fio --name="$1" --filename="$CUR_DEV" --rw=write --bs=1M --iodepth=32 --ioengine=io_uring --direct=1 \
    --size="$2"g --refill_buffers=1 --continue_on_error=all --output-format=json --output="$D/fio-$1.json" > "$D/fio-$1.stdout" 2>&1
}
# ---- 1, 2
fill_job fill1 "$WS_GIB" 300; frc=$?
J="$D/fio-fill1.json"
errs=$(fio_json "$J" | jq -r '[.jobs[]|(if (.error // 0) != 0 then 1 else 0 end) + (.total_err // 0)]|add' 2>/dev/null); mlat=$(fio_json "$J" | jq -r '[.jobs[]|.write.lat_ns.max]|max/1000000|floor' 2>/dev/null)
say "- fill: fio rc $frc, errors $errs, longest write completion ${mlat} ms"
[ "${errs:-0}" -gt 0 ]; expect "fio saw write errors (ENOSPC) once the cap was reached" $? || true
[ "${mlat:-999999}" -le $((FULL_TIMEOUT_S * 1000 + 5000)) ]; expect "no write completed later than full_timeout + 5 s (${mlat} ms)" $? || true
grep -qi "No space left\|\"error\" *: *28\|error=28" "$J" "$D/fio-fill1.stdout"; expect "the errors are ENOSPC" $? || true
st=$(gate_state); say "- gate state after the fill: $st"
[ "$st" = full ] || [ "$st" = stop ] || [ "$st" = full-by-ledger ]; expect "the status shows the gate full (got $st)" $? || true
[ "$(hung)" = "$H0" ]; expect "no hung-task line after the fill" $? || true
lx space "$VOL" --json > "$D/space-full.json" 2>&1
lx status "$VOL" --json | jq -c .throttle > "$D/throttle-full.json" 2>&1
# ---- 3: blkdiscard of half the device
HALF=$((VOL_GIB * GIB / 2))
blkdiscard -o 0 -l "$HALF" "$CUR_DEV"; expect "blkdiscard of half the device (rc $?)" $? || true
t0=$SECONDS; ok=1
while [ $((SECONDS - t0)) -lt 90 ]; do
  if dd if=/dev/zero of="$CUR_DEV" bs=1M count=8 seek=$(( (VOL_GIB * 1024) / 2 + 4096 )) oflag=direct 2>/dev/null; then ok=0; break; fi
  sleep 3
done
say "- writes after the discard: ok=$((1 - ok)) after $((SECONDS - t0)) s"
[ "$ok" = 0 ]; expect "writes came back within 90 s of the discard ($((SECONDS - t0)) s)" $? || true
# ---- 4: SIGTERM with writes parked (long full_timeout so that they park)
lx space "$VOL" --full-timeout 120s --json > "$D/space-long.json" 2>&1
fill_job fill2 "$WS_GIB" 1200 &
FP=$!
# wait (up to 400 s) until writes are parked: the working set is written again from the start; inside the band the writes are only paced (4 MiB/s at the
# hard mark), so the cap is reached slowly
for i in $(seq 1 200); do [ "$(lx space "$VOL" --json 2>/dev/null | jq -r '.figures.waiters')" -gt 0 ] 2>/dev/null && break; sleep 2; done
say "- state with writes parked: $(gate_state), waiters $(lx space "$VOL" --json | jq -r '.figures.waiters')"
[ "$(lx space "$VOL" --json | jq -r '.figures.waiters')" -gt 0 ]; expect "writes are parked at the cap" $? || true
T0=$SECONDS; kill -TERM "$DPID"
for i in $(seq 1 400); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
exit_s=$((SECONDS - T0))
if kill -0 "$DPID" 2>/dev/null; then fail "the daemon did not exit within 40 s of SIGTERM with writes parked (never killed)"; wedged "daemon stuck after SIGTERM with parked writes"; fi
wait "$DPID" 2>/dev/null; DPID=""
[ "$exit_s" -le 30 ]; expect "SIGTERM with writes parked: the daemon exited in ${exit_s}s (quiesce_timeout_ms 30 s)" $? || true
# restart (recovers the device), fill again until parked, then kill -9
start_lunixd || wedged "restart after SIGTERM failed"
wait "$FP" 2>/dev/null
lx space "$VOL" --full-timeout 120s --json >/dev/null 2>&1
fill_job fill3 "$WS_GIB" 1200 &
FP=$!
for i in $(seq 1 200); do [ "$(lx space "$VOL" --json 2>/dev/null | jq -r '.figures.waiters')" -gt 0 ] 2>/dev/null && break; sleep 2; done
kill -KILL "$DPID"; wait "$DPID" 2>/dev/null; DPID=""
# ---- 5: on_full = error in volume.json, restart
VJ="$(vol_dir "$VOL" 2>/dev/null)"
[ -n "$VJ" ] || VJ=$(ls -d "$VOLS"/*/ | head -1)
jq '.space.on_full = "error"' "$VJ/volume.json" > "$VJ/volume.json.new" && mv "$VJ/volume.json.new" "$VJ/volume.json"; expect "wrote space.on_full = error to volume.json" $? || true
start_lunixd || wedged "restart after kill -9 failed"
for i in $(seq 1 600); do kill -0 "$FP" 2>/dev/null || break; sleep 0.5; done
kill -0 "$FP" 2>/dev/null && wedged "fio did not end within 300 s after the restart with on_full = error"
wait "$FP"; say "- fio after the restart: rc $?"
expect "fio ended after the kill -9 and the restart with on_full = error" 0 || true
# with on_full = error and the store still at its cap a write may well fail: it must fail with ENOSPC at once, not hang and not with EIO
dd if=/dev/zero of="$CUR_DEV" bs=1M count=1 seek=$(( (VOL_GIB * 1024) / 2 + 8192 )) oflag=direct 2> "$D/usable-dd.txt"; urc=$?
{ [ "$urc" = 0 ] || grep -q "No space left" "$D/usable-dd.txt"; }; expect "the device answers after the restart (a write succeeds or fails with ENOSPC, rc $urc)" $? || true
dd if="$CUR_DEV" of=/dev/null bs=1M count=64 iflag=direct 2>/dev/null; expect "a read succeeds" $? || true
[ "$(hung)" = "$H0" ]; expect "no hung-task line in the whole cell" $? || true
# ---- 6: what ext4 and xfs do with the error (recorded)
lx space "$VOL" --on-full error --full-timeout 5s --json >/dev/null 2>&1
blkdiscard "$CUR_DEV" 2>/dev/null
# the discard frees the store only after the collector's grace: wait until the ledger is below half the cap (up to 5 min) before mkfs
for i in $(seq 1 100); do
  u=$(lx space "$VOL" --json | jq -r '.figures.used'); l=$(lx space "$VOL" --json | jq -r '.figures.limit')
  [ "$u" -lt $((l / 2)) ] 2>/dev/null && break; sleep 3
done
say "- store after the full discard: used $((u >> 20)) MiB of $((l >> 20)) MiB"
for fs in ext4 xfs; do
  mkfs_fs "$fs" "$CUR_DEV" || continue
  mount_fs "$fs" "$CUR_DEV" "$MNT" || continue
  dd if=/dev/urandom of="$MNT/fill" bs=1M count=$((WS_GIB * 1024)) conv=fsync > "$D/fs-$fs-dd.txt" 2>&1; drc=$?
  sync > "$D/fs-$fs-sync.txt" 2>&1; src=$?
  touch "$MNT/after" > "$D/fs-$fs-touch.txt" 2>&1; trc=$?
  grep " $MNT " /proc/mounts | grep -o "r[ow]," | head -1 > "$D/fs-$fs-mountstate.txt"
  dmesg | grep -iE "$fs|ublkb" | tail -8 > "$D/fs-$fs-dmesg.txt"
  say "- $fs on the full volume: dd rc $drc ($(tail -1 "$D/fs-$fs-dd.txt" | cut -c1-80)), sync rc $src, touch rc $trc, mount $(cat "$D/fs-$fs-mountstate.txt")"
  rm -f "$MNT/fill" 2>/dev/null
  timeout -k 10 120 umount "$MNT" || umount -l "$MNT"
  guest_fsck "$fs" "$CUR_DEV"; say "- $fs guest fsck after the error: rc $?"
  blkdiscard "$CUR_DEV" 2>/dev/null
  for i in $(seq 1 100); do u=$(lx space "$VOL" --json | jq -r '.figures.used'); [ "$u" -lt $((l / 2)) ] 2>/dev/null && break; sleep 3; done
done
rec fullvol/cell "$([ "$FAIL" = 0 ] && echo pass || echo fail)" cap="$CAP" full_timeout_s="$FULL_TIMEOUT_S" sigterm_exit_s="$exit_s" fill_errors="${errs:-}" fill_max_lat_ms="${mlat:-}"
cell_end "$VOL" fullvol || true
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
