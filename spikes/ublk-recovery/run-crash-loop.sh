#!/bin/bash
# P0.3 crash-loop driver (root, scratch only).
#   SINK=file|mem (mem: P0.12 RAM buffer + keeper)  FS=ext4|xfs  KILL_MODE=kill9|quiesce  BATCH=0|1  CONTROL=none|noreissue|fault
#   PHASES="name:count:delay_s name:..."  (name=double means two kills inside the recovery window)
# Per phase: mount, fio (mixed verify job + fsync job) and a 1 ms heartbeat, kill/restart
# cycles at random 5-20 s intervals, then stop fio, umount, fsck, remount and re-verify.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out}" "${FS:=ext4}" "${KILL_MODE:=kill9}" "${BATCH:=0}" "${CONTROL:=none}"
: "${PHASES:=immediate:20:0 delay1s:10:1 delay10s:3:10 double:3:0}" "${SEED:=7}" "${IMG_GIB:=16}"
: "${MIN_INT:=5}" "${MAX_INT:=20}"
mkdir -p "$OUT"
IMG="$LUNIX_SPIKE_DIR/rec.img"; MNT="$LUNIX_SPIKE_DIR/mnt/rec"; need_safe "$IMG"; need_safe "$MNT"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; echo "not clean at start"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"; add_cleanup "rm -f $IMG"
add_cleanup 'del_dev'
add_cleanup 'stop_daemon_clean'
add_cleanup 'umount -l '"$MNT"
:
DARGS=()
[ "$BATCH" = 1 ] && DARGS+=(--batch)
[ "$CONTROL" = noreissue ] && DARGS+=(--no-reissue)
if [ "${SINK:-file}" = mem ]; then   # P0.12: RAM write buffer held by a keeper over kill -9
  KSOCK="$LUNIX_SPIKE_DIR/run/keeper.sock"; need_safe "$KSOCK"
  python3 "$here/keeper/keeper.py" "$KSOCK" > "$OUT/keeper.log" 2>&1 & KPID=$!
  add_cleanup 'kill $KPID'; add_cleanup "rm -f $KSOCK"
  for i in $(seq 1 50); do grep -q ready "$OUT/keeper.log" && break; sleep 0.1; done
  DARGS+=(--sink mem --keeper "$KSOCK"); QUEUES=1
fi
start_daemon "${DARGS[@]}" || exit 1
make_fs "$FS" "$DEV"
RC_ALL=0
echo "phase,cycles,fio_rc,fsck_rc,hb_errors,verify_rc(1=sha,10=ack-loss,100=fsyncer-err),notes" > "$OUT/phases.csv"

fio_jobs() { # runtime
  # Job A: mixed random read/write with verification. Job B: fsync-heavy 4k writes.
  # 4 jobs x 1 GiB = the plan's 4 GiB total working set.
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

run_phase() { # name count delay
  local name=$1 count=$2 delay=$3 d="$OUT/$1"; mkdir -p "$d"
  echo "== phase $name count=$count delay=$delay $(date +%T)"
  mount_fs "$FS" "$DEV" "$MNT" || { echo "mount failed"; RC_ALL=1; return; }
  # schedule: fixed-seed random intervals
  RANDOM=$SEED; local ints=() total=0 i
  for ((i=0;i<count;i++)); do v=$((MIN_INT + RANDOM % (MAX_INT - MIN_INT + 1))); ints+=($v); total=$((total + v + delay + 4)); done
  local rt=$((total + 25))
  fio_jobs "$rt"
  rm -f "$MNT"/mixed.* "$MNT"/fs?.dat "$MNT"/hb.dat
  (cd "$MNT" && timeout -k 10 $((rt + 120)) fio jobs.fio --output-format=json --output="$d/fio.json" > "$d/fio.stdout" 2>&1; echo $? > "$d/fio.rc") &
  local fpid=$!
  python3 "$here/heartbeat.py" "$MNT/hb.dat" "$d/hb.csv" & local hpid=$!
  python3 "$here/fsyncer.py" write "$MNT/fs1.dat" "$d/ack1" > "$d/fsyncer1.out" 2>&1 & local s1=$!
  python3 "$here/fsyncer.py" write "$MNT/fs2.dat" "$d/ack2" > "$d/fsyncer2.out" 2>&1 & local s2=$!
  sleep 3
  echo "phase,cycle,t_kill_ns,t_ready_ns,daemon_ms" > "$d/cycles.csv"
  local cyc=0 aborted=""
  for iv in "${ints[@]}"; do
    cyc=$((cyc + 1))
    sleep "$iv"
    [ -s "$d/fio.rc" ] && { aborted="fio ended early at cycle $cyc"; break; }
    if [ "$CONTROL" = fault ]; then kill -HUP "$DPID"; sleep 0.5; fi
    local tk; tk=$(now_ns)
    if [ "$KILL_MODE" = quiesce ]; then
      kill -USR2 "$DPID"; for i in $(seq 1 300); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.05; done
      kill -0 "$DPID" 2>/dev/null && { echo "WEDGED: daemon did not exit after quiesce; stopping at once, nothing else touched"; touch "$OUT/WEDGED"; trap - EXIT INT TERM; exit 9; }
    else
      kill -9 "$DPID"; wait "$DPID" 2>/dev/null
    fi
    if [ "$name" = double ]; then
      # second kill inside the recovery window: start recovery, kill it again shortly after exec
      taskset -c "$DAEMON_CPUS" "$REC_BIN" run --backing "$IMG" --queues "$QUEUES" --cpus "$DAEMON_CPUS" --dev-id "$DEVID" --recover "${DARGS[@]}" > "$OUT/daemon.dk$cyc.log" 2>&1 &
      local kp=$!; sleep "0.$((RANDOM % 4 + 1))"; kill -9 "$kp" 2>/dev/null; wait "$kp" 2>/dev/null
    fi
    [ "$delay" != 0 ] && sleep "$delay"
    start_daemon --recover "${DARGS[@]}" || { aborted="recovery failed at cycle $cyc"; break; }
    local tr; tr=$(now_ns)
    echo "$name,$cyc,$tk,$tr,$(daemon_ms)" >> "$d/cycles.csv"
  done
  [ -n "$aborted" ] && echo "ABORTED: $aborted" | tee "$d/aborted.txt"
  # let fio finish by itself
  wait "$fpid"; local frc; frc=$(cat "$d/fio.rc" 2>/dev/null || echo NA)
  kill -TERM "$hpid" "$s1" "$s2" 2>/dev/null; wait "$hpid" 2>/dev/null
  wait "$s1"; local s1rc=$?; wait "$s2"; local s2rc=$?
  local hberr; hberr=$(grep -o 'errors=[0-9]*' "$d/hb.csv" | cut -d= -f2)
  sha256sum "$MNT"/mixed.* "$MNT"/fs?.dat 2>/dev/null | sed "s#$MNT/##" > "$d/sha-before.txt"
  umount "$MNT"
  local fr; fr=$(check_fs "$FS" "$DEV")
  mount_fs "$FS" "$DEV" "$MNT"
  sha256sum "$MNT"/mixed.* "$MNT"/fs?.dat 2>/dev/null | sed "s#$MNT/##" > "$d/sha-after.txt"
  local vr=0; cmp -s "$d/sha-before.txt" "$d/sha-after.txt" || vr=1
  python3 "$here/fsyncer.py" verify "$MNT/fs1.dat" "$d/ack1" > "$d/fsverify1.out" 2>&1 || vr=$((vr + 10))
  python3 "$here/fsyncer.py" verify "$MNT/fs2.dat" "$d/ack2" > "$d/fsverify2.out" 2>&1 || vr=$((vr + 10))
  [ "$s1rc" = 0 ] && [ "$s2rc" = 0 ] || vr=$((vr + 100))
  umount "$MNT"
  jq -c '[.jobs[] | {jobname, error, total_err}]' "$d/fio.json" > "$d/fio-errors.json" 2>/dev/null
  jq '[.jobs[] | [.read.clat_ns.max, .write.clat_ns.max] | max] | max / 1e6' "$d/fio.json" > "$d/fio-max-clat-ms.txt" 2>/dev/null
  echo "$name,$cyc,$frc,$fr,$hberr,$vr,$aborted" >> "$OUT/phases.csv"
  echo "phase $name done: cycles=$cyc fio_rc=$frc fsck_rc=$fr sha_equal=$((1-vr))"
  [ "$frc" = 0 ] && [ "$fr" = 0 ] && [ "$vr" = 0 ] && [ -z "$aborted" ] || RC_ALL=1
}

for p in $PHASES; do IFS=: read -r n c dl <<< "$p"; run_phase "$n" "$c" "$dl"; done
python3 "$here/stall.py" $(for p in $PHASES; do echo "$OUT/${p%%:*}"; done) > "$OUT/stall-table.md" 2>/dev/null
cat "$OUT/phases.csv"; cat "$OUT/stall-table.md"
echo "overall rc=$RC_ALL" | tee "$OUT/result.txt"
bash "$here/../common/ublk-leftovers.sh" > /dev/null; true
exit 0
