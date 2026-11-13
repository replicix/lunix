#!/bin/bash
# M10 crash/handover loop on a MOUNTED file system (ported from spikes/ublk-recovery/run-crash-loop.sh).
#   FS=ext4|xfs  KILL_MODE=kill9|quiesce|crashpoint  CONTROL=none|noreissue|fault  USER_COPY=0|1
#   PHASES="name:count:delay_s ..."   (name=double: a second kill inside the recovery window)
# Per phase: mount, fio (verifying mixed job), two acknowledged-fsync writers and a 1 ms heartbeat run
# while the daemon is killed (kill -9), handed over (QUIESCE, the daemon exits) or killed at a crash
# point, and restarted with `recover`; then stop the load, umount, fsck, remount, compare checksums,
# verify every acknowledged fsync. Teardown: serving daemon, umount, detach (TRY_STOP_DEV), delete.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"; . "$here/tdlib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/out-loop}" "${FS:=ext4}" "${KILL_MODE:=kill9}" "${CONTROL:=none}" "${USER_COPY:=0}"
: "${PHASES:=immediate:20:0 delay1s:10:1 delay10s:3:10 double:3:0}" "${SEED:=7}" "${IMG_GIB:=16}"
: "${MIN_INT:=5}" "${MAX_INT:=20}" "${QUIESCE_MS:=15000}"
rm -rf "$OUT"; mkdir -p "$OUT"
IMG="$LUNIX_SPIKE_DIR/rec.img"; MNT="$LUNIX_SPIKE_DIR/mnt/rec"; STATE="$LUNIX_SPIKE_DIR/state/loop"
need_safe "$IMG"; need_safe "$MNT"; need_safe "$STATE"
need_clean_start "$OUT"
script_cap "${SCRIPT_CAP:-3000}"   # the script's own cap (W8); the wrapper timeout is larger
load_ublk
rm -rf "$STATE"; mkdir -p "$STATE" "$MNT"; rm -f "$IMG"
add_cleanup 'teardown >> "$OUT/teardown.log" 2>&1'
DARGS=()
[ "$USER_COPY" = 1 ] && DARGS+=(--user-copy)
[ "$CONTROL" = noreissue ] && DARGS+=(--no-reissue)
[ -n "${TESTD_ARGS:-}" ] && DARGS+=($TESTD_ARGS)   # e.g. --io-delay-ms 20 to hold requests at a quiesce
[ -n "${WORKERS:-}" ] && DARGS+=(--workers "$WORKERS")   # WORKERS=0: the engine completes inline (like the M11 engine)
MKFS_GIB=$IMG_GIB
df_guard "$LUNIX_SPIKE_DIR" "$IMG_GIB" || exit 2
MAX_GIB=$IMG_GIB
start_daemon run --create-gib "$IMG_GIB" "${DARGS[@]}" || exit 1
make_fs "$FS" "$DEV" || exit 1
RC_ALL=0; NOTES=""
echo "phase,cycles,fio_rc,fsck_rc,hb_errors,verify_rc(1=sha,10=ack-loss,100=fsyncer-err),notes" > "$OUT/phases.csv"

fio_jobs() { # runtime
  cat > "$MNT/jobs.fio" <<JOB
[global]
directory=$MNT
ioengine=io_uring
time_based
runtime=$1
cpus_allowed=$FIO_CPUS
[mixed]
rw=randrw
rwmixwrite=70
bssplit=4k/50:16k/30:64k/20
iodepth=32
numjobs=4
direct=1
verify=crc32c
verify_backlog=64
verify_fatal=1
do_verify=1
size=1g
norandommap
JOB
}

# handover N: QUIESCE (the daemon exits), with the kernel timeout QUIESCE_MS; a timeout is retried once, then
# the kill -9 fallback (recoverable like any crash) is taken and counted.
handover() {
  local old=$DPID qrc i t0 t1
  t0=$(now_ns)
  tdc quiesce --timeout-ms "$QUIESCE_MS" > "$OUT/quiesce.last.txt" 2>&1; qrc=$?
  if [ "$qrc" != 0 ] && kill -0 "$old" 2>/dev/null; then
    echo "quiesce attempt failed ($(cat "$OUT/quiesce.last.txt")); retrying once" | tee -a "$OUT/notes.txt"
    tdc quiesce --timeout-ms "$QUIESCE_MS" > "$OUT/quiesce.last.txt" 2>&1; qrc=$?
  fi
  if [ "$qrc" != 0 ] && kill -0 "$old" 2>/dev/null; then
    echo "quiesce failed twice; kill -9 fallback (counted)" | tee -a "$OUT/notes.txt"
    NOTES="$NOTES;quiesce-fallback"; kill -9 "$old"; wait "$old" 2>/dev/null; return 0
  fi
  for i in $(seq 1 300); do kill -0 "$old" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$old" 2>/dev/null && wedged "daemon $old did not exit within 30 s of QUIESCE; stopped, nothing killed"
  wait "$old"; local xrc=$?
  t1=$(now_ns)
  # true drain: the kernel has no started request left when the daemon exits, and the exit is quick
  local ki; ki=$(sed -n 's/.*kernel_inflight=\([0-9,]*\).*/\1/p' "$OUT/quiesce.last.txt" | head -1)
  echo "$(( (t1 - t0) / 1000000 )),$ki,$(sed -n 's/.*drain_commits=\([0-9]*\).*/\1/p' "$OUT/quiesce.last.txt" | head -1)" >> "$OUT/handover.csv"
  [ "$ki" = "0,0" ] || { echo "handover: kernel_inflight=$ki" | tee -a "$OUT/notes.txt"; NOTES="$NOTES;kernel-inflight-$ki"; }
  [ $(( (t1 - t0) / 1000000 )) -lt 8000 ] || { echo "handover took $(( (t1 - t0) / 1000000 )) ms" | tee -a "$OUT/notes.txt"; NOTES="$NOTES;slow-handover"; }
  [ "$xrc" = 0 ] || { echo "quiesce exit code $xrc" | tee -a "$OUT/notes.txt"; NOTES="$NOTES;quiesce-exit-$xrc"; }
}

run_phase() { # name count delay
  local name=$1 count=$2 delay=$3 d="$OUT/$1"; mkdir -p "$d"; frc=""
  echo "== phase $name count=$count delay=$delay $(date +%T)"
  mount_fs "$FS" "$DEV" "$MNT" || { echo "mount failed"; RC_ALL=1; return; }
  if [ "$KILL_MODE" = crashpoint ]; then   # from now on every daemon dies by itself at a crash point
    kill -9 "$DPID"; wait "$DPID" 2>/dev/null
    CRASH_AT="$CRASH_POINT:$((4000 + RANDOM % 4000))"
    start_daemon recover || { RC_ALL=1; return; }
  fi
  RANDOM=$SEED; local ints=() total=0 i v
  for ((i=0;i<count;i++)); do v=$((MIN_INT + RANDOM % (MAX_INT - MIN_INT + 1))); ints+=($v); total=$((total + v + delay + 4)); done
  local rt=$((total + 25))
  fio_jobs "$rt"
  rm -f "$MNT"/mixed.* "$MNT"/fs?.dat "$MNT"/hb.dat
  (cd "$MNT" && exec timeout -k 10 $((rt + 300)) fio jobs.fio --output-format=json --output="$d/fio.json" > "$d/fio.stdout" 2>&1) &
  FIO_PID=$!
  python3 "$here/heartbeat.py" "$MNT/hb.dat" "$d/hb.csv" & HB_PID=$!
  python3 "$here/fsyncer.py" write "$MNT/fs1.dat" "$d/ack1" > "$d/fsyncer1.out" 2>&1 & S1_PID=$!
  python3 "$here/fsyncer.py" write "$MNT/fs2.dat" "$d/ack2" > "$d/fsyncer2.out" 2>&1 & S2_PID=$!
  sleep 3
  echo "phase,cycle,t_kill_ns,t_ready_ns,daemon_ms" > "$d/cycles.csv"
  local cyc=0 aborted="" iv tk tr
  for iv in "${ints[@]}"; do
    cyc=$((cyc + 1))
    if [ "$KILL_MODE" = crashpoint ]; then
      # the daemon dies by itself at the crash point (SIGKILL from inside)
      for i in $(seq 1 3000); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
      kill -0 "$DPID" 2>/dev/null && { aborted="daemon did not hit $CRASH_AT in 300 s at cycle $cyc"; break; }
      tk=$(now_ns); wait "$DPID" 2>/dev/null
      # only a daemon that is meant to crash gets a crash point: none after the last cycle
      if [ "$cyc" -lt "$count" ]; then CRASH_AT="$CRASH_POINT:$((4000 + RANDOM % 4000))"; else CRASH_AT=""; fi
    else
      sleep "$iv"
      [ -n "$FIO_PID" ] && ! kill -0 "$FIO_PID" 2>/dev/null && { aborted="fio ended early at cycle $cyc"; break; }
      if [ "$CONTROL" = fault ]; then tdc cmd fault on >/dev/null 2>&1; sleep 0.5; fi
      # one acknowledged FUA write is dropped, then the daemon dies (sensitivity of the harness to a single lost write)
      if [ "$CONTROL" = dropfua ]; then tdc cmd fault drop-fua-one >/dev/null 2>&1; sleep 2; fi
      tk=$(now_ns)
      if [ "$KILL_MODE" = quiesce ]; then
        handover
      else
        kill -9 "$DPID"; wait "$DPID" 2>/dev/null
      fi
    fi
    if [ "$name" = double ]; then
      # second kill inside the recovery window: start recovery, kill it shortly after exec
      env ${CRASH_AT:+LUNIX_CRASH_AT=$CRASH_AT} taskset -c "$DAEMON_CPUS" "$TESTD" recover --state "$STATE" --backing "$IMG" --queues "$QUEUES" \
        --depth "$DEPTH" --cpus "$DAEMON_CPUS" ${MAX_GIB:+--max-gib $MAX_GIB} "${DARGS[@]}" > "$OUT/daemon.dk$cyc.log" 2>&1 &
      local kp=$!; sleep "0.$((RANDOM % 4 + 1))"; kill -9 "$kp" 2>/dev/null; wait "$kp" 2>/dev/null
    fi
    [ "$delay" != 0 ] && sleep "$delay"
    start_daemon recover "${DARGS[@]}" || { aborted="recovery failed at cycle $cyc"; break; }
    tr=$(now_ns)
    echo "$name,$cyc,$tk,$tr,$(daemon_ms)" >> "$d/cycles.csv"
  done
  [ -n "$aborted" ] && echo "ABORTED: $aborted" | tee "$d/aborted.txt"
  CRASH_AT=""
  if [ "$KILL_MODE" = crashpoint ]; then   # the running daemon still carries a crash point: replace it
    kill -9 "$DPID" 2>/dev/null; wait "$DPID" 2>/dev/null
    start_daemon recover "${DARGS[@]}" || wedged "no daemon after the crash-point cycles"
  fi
  # a serving daemon is needed for the rest of the phase
  kill -0 "$DPID" 2>/dev/null || start_daemon recover "${DARGS[@]}" || wedged "no daemon after the cycles"
  [ -n "$FIO_PID" ] && { wait "$FIO_PID"; frc=$?; } ; FIO_PID=""
  local frc=${frc:-NA}
  kill -TERM "$HB_PID" "$S1_PID" "$S2_PID" 2>/dev/null; wait "$HB_PID" 2>/dev/null
  wait "$S1_PID"; local s1rc=$?; wait "$S2_PID"; local s2rc=$?; HB_PID=""; S1_PID=""; S2_PID=""
  local hberr; hberr=$(grep -o 'errors=[0-9]*' "$d/hb.csv" | cut -d= -f2)
  sha256sum "$MNT"/mixed.* "$MNT"/fs?.dat 2>/dev/null | sed "s#$MNT/##" > "$d/sha-before.txt"
  timeout -k 5 120 umount "$MNT" || wedged "umount failed or timed out in phase $name"
  local fr; fr=$(check_fs "$FS" "$DEV")
  mount_fs "$FS" "$DEV" "$MNT"
  sha256sum "$MNT"/mixed.* "$MNT"/fs?.dat 2>/dev/null | sed "s#$MNT/##" > "$d/sha-after.txt"
  local vr=0; cmp -s "$d/sha-before.txt" "$d/sha-after.txt" || vr=1
  python3 "$here/fsyncer.py" verify "$MNT/fs1.dat" "$d/ack1" > "$d/fsverify1.out" 2>&1 || vr=$((vr + 10))
  python3 "$here/fsyncer.py" verify "$MNT/fs2.dat" "$d/ack2" > "$d/fsverify2.out" 2>&1 || vr=$((vr + 10))
  [ "$s1rc" = 0 ] && [ "$s2rc" = 0 ] || vr=$((vr + 100))
  timeout -k 5 120 umount "$MNT" || wedged "second umount failed or timed out in phase $name"
  jq -c '[.jobs[] | {jobname, error, total_err}]' "$d/fio.json" > "$d/fio-errors.json" 2>/dev/null
  jq '[.jobs[] | [.read.clat_ns.max, .write.clat_ns.max] | max] | max / 1e6' "$d/fio.json" > "$d/fio-max-clat-ms.txt" 2>/dev/null
  echo "$name,$cyc,$frc,$fr,$hberr,$vr,$aborted$NOTES" >> "$OUT/phases.csv"
  echo "phase $name done: cycles=$cyc fio_rc=$frc fsck_rc=$fr verify_rc=$vr"
  if [ "$CONTROL" != none ]; then   # negative control: it must trip
    { [ "$frc" != 0 ] || [ "$fr" != 0 ] || [ "$vr" != 0 ]; } || RC_ALL=1
  else
    { [ "$frc" = 0 ] && [ "$fr" = 0 ] && [ "$vr" = 0 ] && [ -z "$aborted" ]; } || RC_ALL=1
  fi
}

[ "$KILL_MODE" = crashpoint ] && : "${CRASH_POINT:=ublk.before_commit}"
for p in $PHASES; do IFS=: read -r n c dl <<< "$p"; run_phase "$n" "$c" "$dl"; done
python3 "$here/stall.py" $(for p in $PHASES; do echo "$OUT/${p%%:*}"; done) > "$OUT/stall-table.md" 2>/dev/null
cat "$OUT/phases.csv"; cat "$OUT/stall-table.md"
echo "overall rc=$RC_ALL (CONTROL=$CONTROL: for a negative control 0 means it tripped as designed)" | tee "$OUT/result.txt"
exit "$RC_ALL"
