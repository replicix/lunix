#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 3: negative controls. A harness that cannot fail proves nothing; each control here MUST trip.
#
#   N1 (M10 controls, the test daemon): crates/lunix-ublk/tests/root/run-crash-loop.sh with CONTROL=noreissue (requests
#      not reissued after the kill) and CONTROL=fault (writes dropped): each must report failures (its own rc 0 means
#      "tripped as designed"). Needs TESTD only. Skipped with NEG_M10=0.
#   N2 (full stack, lunixd): verify sensitivity. fio writes and verifies a region of a raw Lunix volume, the volume is
#      detached and attached, one 4 KiB block inside the region is overwritten with garbage through the device, and
#      `fio --verify_only` of the saved state must FAIL.
#   N3 (full stack): acknowledged-fsync sensitivity. fsyncer writes on ext4, stops, its acknowledged index is raised by
#      10000, and `fsyncer verify` must report bad blocks.
# The M10 controls that lose requests at the ublk layer (no REISSUE, dropped writes) have no switch in lunixd; a
# lunixd-level equivalent needs a file-armed fault hook like LUNIX_UBLK_FAULT_HOLD (request to the lead).
here="$(cd "$(dirname "$0")" && pwd)"
export SCRIPT_CAP="${SCRIPT_CAP:-5400}"
. "$here/lib.sh"
harness_init m15-negctl
say "# M15 negative controls ($(date -u +%FT%TZ))"
MIB=$((1 << 20))
ROOTDIR_M10="$REPO/crates/lunix-ublk/tests/root"
if [ "${NEG_M10:-1}" = 1 ]; then
  for c in "noreissue immediate:3:0" "fault immediate:2:0"; do
    ctl=${c%% *}; ph=${c#* }
    CONTROL=$ctl PHASES="$ph" OUT="$OUT/m10-$ctl" IMG_GIB=4 SCRIPT_CAP=1500 TESTD="$TESTD" bash "$ROOTDIR_M10/run-crash-loop.sh" > "$OUT/m10-$ctl.log" 2>&1
    rc=$?
    [ "$rc" = 0 ]; expect "N1 M10 control $ctl tripped (run-crash-loop.sh rc $rc; 0 = tripped as designed)" $?
    rec "negctl/m10/$ctl" "$([ "$rc" = 0 ] && echo pass || echo fail)" rc="$rc"
  done
fi
# ---- N2 / N3 on the full stack
VOL=neg
cell_begin "$VOL" 8 65536 integrity
D="$OUT/raw/neg"; mkdir -p "$D/state"
export DEV="$CUR_DEV" BASE=0 PER_JOB=$((256 * MIB)) JOBS=1 QD=16 RUNTIME=60 SEED=5 LOOPS=1 VONLY=0 TRIM_BASE=$((1024 * MIB)) TRIM_SIZE=$((64 * MIB))
fio_run neg-write 60 "$REPO/bench/phase1/exit/fio/verify-raw.fio" "--aux-path=$D/state" --section=main; rc=$?
[ "$rc" = 0 ]; expect "N2 setup: verified write of 256 MiB (rc $rc)" $?
detach_vol "$VOL"; attach_vol "$VOL"; CUR_DEV=$(dev_of "$VOL"); register_id "${CUR_DEV#/dev/ublkb}"; export DEV="$CUR_DEV"
VONLY=1 fio_run neg-ok 60 "$REPO/bench/phase1/exit/fio/verify-raw.fio" "--aux-path=$D/state" --section=main; rc=$?
[ "$rc" = 0 ]; expect "N2 control precondition: verify_only passes before the corruption (rc $rc)" $?
dd if=/dev/urandom of="$CUR_DEV" bs=4096 count=1 seek=$((100 * 256 + 3)) oflag=direct conv=notrunc 2>/dev/null   # inside the first 256 MiB
VONLY=1 fio_run neg-bad 60 "$REPO/bench/phase1/exit/fio/verify-raw.fio" "--aux-path=$D/state" --section=main; rc=$?
[ "$rc" != 0 ] || [ "$(fio_errors "$OUT/raw/fio-neg-bad.json")" != 0 ]; expect "N2 control tripped: verify_only after one overwritten 4 KiB block failed (rc $rc)" $?
rec negctl/full-stack/verify-corrupt "$([ "$rc" != 0 ] && echo pass || echo fail)" fio_rc="$rc"
# N3
mkfs_fs ext4 "$CUR_DEV" && mount_fs ext4 "$CUR_DEV" "$MNT"
python3 -I "$REPO/crates/lunix-ublk/tests/root/fsyncer.py" write "$MNT/fs.dat" "$D/ack" > "$D/fsyncer.out" 2>&1 & FP=$!
sleep 10; kill -TERM "$FP"; wait "$FP"; frc=$?
[ "$frc" = 0 ]; expect "N3 setup: fsyncer ran and stopped cleanly (rc $frc)" $?
python3 -I "$REPO/crates/lunix-ublk/tests/root/fsyncer.py" verify "$MNT/fs.dat" "$D/ack" > "$D/verify-ok.out" 2>&1; rc=$?
[ "$rc" = 0 ]; expect "N3 control precondition: acknowledged blocks verify before the tampering (rc $rc)" $?
printf '%020d\n' $(( 10#$(tr -d '\n' < "$D/ack") + 10000 )) > "$D/ack"
python3 -I "$REPO/crates/lunix-ublk/tests/root/fsyncer.py" verify "$MNT/fs.dat" "$D/ack" > "$D/verify-bad.out" 2>&1; rc=$?
[ "$rc" != 0 ]; expect "N3 control tripped: an acknowledged index beyond what was written is reported (rc $rc)" $?
rec negctl/full-stack/fsyncer-ack "$([ "$rc" != 0 ] && echo pass || echo fail)" rc="$rc"
umount_fs "$MNT"
cell_end "$VOL" neg
# ---- N4: lost acknowledged writes (the crashpoints build's file-armed hook LUNIX_UBLK_FAULT_DROP_WRITES): while the file
# exists WRITEs are acknowledged and thrown away; a verified write must then FAIL. Needs LUNIXD_CP.
if [ -n "${LUNIXD_CP:-}" ] && [ -x "$LUNIXD_CP" ]; then
  DAEMON_BIN="$LUNIXD_CP"; DROPF="$ST/dropw"; rm -f "$DROPF"
  CRASH_ENV="LUNIX_UBLK_FAULT_DROP_WRITES=$DROPF"
  VOL=neg4; cell_begin "$VOL" 8 65536 integrity
  D4="$OUT/raw/neg4"; mkdir -p "$D4/s1" "$D4/s2"
  export DEV="$CUR_DEV" BASE=0 PER_JOB=$((256 * MIB)) JOBS=1 QD=16 RUNTIME=60 SEED=6 LOOPS=1 VONLY=0 TRIM_BASE=$((1024 * MIB)) TRIM_SIZE=$((64 * MIB))
  fio_run neg4-ok 60 "$REPO/bench/phase1/exit/fio/verify-raw.fio" "--aux-path=$D4/s1" --section=main; rc=$?
  [ "$rc" = 0 ]; expect "N4 precondition: a verified write passes while the hook is not armed (rc $rc)" $?
  # a different seed and region: with the same ones the dropped writes would leave exactly the data of the run before
  : > "$DROPF"; export SEED=7 BASE=$((512 * MIB))
  fio_run neg4-bad 60 "$REPO/bench/phase1/exit/fio/verify-raw.fio" "--aux-path=$D4/s2" --section=main; rc=$?
  rm -f "$DROPF"
  [ "$rc" != 0 ] || [ "$(fio_errors "$OUT/raw/fio-neg4-bad.json")" != 0 ]; expect "N4 control tripped: the verified write fails while WRITEs are acknowledged and dropped (rc $rc)" $?
  rec negctl/full-stack/drop-writes "$([ "$rc" != 0 ] && echo pass || echo fail)" fio_rc="$rc"
  cell_end "$VOL" neg4
  CRASH_ENV=""; DAEMON_BIN=""
else say "- N4 skipped: LUNIXD_CP not set"; fi
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
