#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# Workload and supervisor helpers of the crash scripts (source after lib.sh and harness_init).
#
# The load on the mounted file system is fio (verify with backlog), two acknowledged-fsync writers (fsyncer.py: every
# block whose fsync returned must survive any crash) and a 1 ms heartbeat writer whose gaps are the stall times. The
# Python helpers are the M10 ones in crates/lunix-ublk/tests/root.
ROOTDIR="$REPO/crates/lunix-ublk/tests/root"
FIO_JOB="$REPO/bench/phase1/exit/fio/crash-mixed.fio"

# wl_start NAME FS RUNTIME_S SIZE_BYTES NUMJOBS: mount is already done at $MNT; starts the four loads
wl_start() {
  WL_NAME=$1; local rt=$3
  WL_DIR="$OUT/raw/$1"; mkdir -p "$WL_DIR"
  rm -f "$MNT"/mixed.* "$MNT"/fs?.dat "$MNT"/hb.dat
  DIR="$MNT" RUNTIME=$rt SIZE=$4 NUMJOBS=$5 fio_bg "wl-$1" "$rt" "$FIO_JOB"; FIO_PID=$LAST_FIO
  python3 -I "$ROOTDIR/heartbeat.py" "$MNT/hb.dat" "$WL_DIR/hb.csv" & HB_PID=$!
  python3 -I "$ROOTDIR/fsyncer.py" write "$MNT/fs1.dat" "$WL_DIR/ack1" > "$WL_DIR/fsyncer1.out" 2>&1 & S1_PID=$!
  python3 -I "$ROOTDIR/fsyncer.py" write "$MNT/fs2.dat" "$WL_DIR/ack2" > "$WL_DIR/fsyncer2.out" 2>&1 & S2_PID=$!
}
wl_alive() { kill -0 "$FIO_PID" 2>/dev/null && kill -0 "$S1_PID" 2>/dev/null && kill -0 "$S2_PID" 2>/dev/null; }
# wl_finish FS: wait for fio, stop the writers, umount, guest fsck, remount, verify acknowledged fsyncs, umount.
# Sets WL_FIO_RC WL_FSCK_RC WL_VERIFY_RC (0 = clean). Every wait is bounded; a wedge ends the run.
wl_finish() {
  local fs=$1 rt=${2:-900}
  wait_fio "$FIO_PID" "$rt"; WL_FIO_RC=$?
  kill -TERM "$HB_PID" "$S1_PID" "$S2_PID" 2>/dev/null
  wait "$HB_PID" 2>/dev/null; wait "$S1_PID"; local s1=$?; wait "$S2_PID"; local s2=$?
  HB_PID=""; S1_PID=""; S2_PID=""
  WL_FIO_ERRS=$(fio_errors "$OUT/raw/fio-wl-$WL_NAME.json")
  umount_fs "$MNT"
  guest_fsck "$fs" "$CUR_DEV"; WL_FSCK_RC=$?
  mount_fs "$fs" "$CUR_DEV" "$MNT" || wedged "remount after the workload failed"
  WL_VERIFY_RC=0
  python3 -I "$ROOTDIR/fsyncer.py" verify "$MNT/fs1.dat" "$WL_DIR/ack1" > "$WL_DIR/fsverify1.out" 2>&1 || WL_VERIFY_RC=$((WL_VERIFY_RC + 10))
  python3 -I "$ROOTDIR/fsyncer.py" verify "$MNT/fs2.dat" "$WL_DIR/ack2" > "$WL_DIR/fsverify2.out" 2>&1 || WL_VERIFY_RC=$((WL_VERIFY_RC + 10))
  [ "$s1" = 0 ] && [ "$s2" = 0 ] || WL_VERIFY_RC=$((WL_VERIFY_RC + 100))
  umount_fs "$MNT"
}
# wl_ok: the verdict of the last wl_finish (fio zero errors and zero verify failures, fsyncer zero bad acknowledged
# blocks, guest fsck clean)
wl_ok() { [ "$WL_FIO_RC" = 0 ] && [ "$WL_FIO_ERRS" = 0 ] && [ "$WL_FSCK_RC" = 0 ] && [ "$WL_VERIFY_RC" = 0 ]; }

# daemon_ms LOG: the stall figure the daemon logged for its recovery (ms), empty when absent
daemon_ms() { grep -o 'stall_ms=[0-9]*' "$1" | head -1 | cut -d= -f2; }
# wait_recovered LOG SECS: the restarted daemon logs that it recovered the device
wait_recovered() {
  local n=$(($2 * 10))
  while [ "$n" -gt 0 ]; do recovered_log "$1" && return 0; kill -0 "$DPID" 2>/dev/null || return 1; sleep 0.1; n=$((n - 1)); done
  return 1
}
# phase_detach_check FS TAG: after a phase: detach (the file system is unmounted), `lunix fsck --deep`, attach again
# (returns the fsck verdict)
phase_detach_check() {
  local fs=$1 tag=$2
  detach_vol "$CUR_VOL"; expect "$tag: detach" $? || return 1
  lunix_fsck "$CUR_VOL" "$tag"; local rc=$?
  expect "$tag: lunix fsck --deep clean after detach (exit $FSCK_RC)" $rc || true
  attach_vol "$CUR_VOL"; expect "$tag: attach again" $? || return 1
  CUR_DEV=$(dev_of "$CUR_VOL"); register_id "${CUR_DEV#/dev/ublkb}"
  return $rc
}
