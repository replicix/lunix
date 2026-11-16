#!/bin/bash
# Root self-test of the control flow of the M15 scripts against a FAKE lunix and lunixd (fakes/fake-lunix.sh: loop
# devices instead of ublk): fio verify, the random-kill loop, the crash points, the ratios and the negative controls
# run end to end with tiny parameters. It finds script bugs (quoting, ordering, parsing); it says nothing about Lunix.
# Disk: a few GiB of RAM-backed scratch if LUNIX_SPIKE_DIR is on tmpfs; df-guard asks for MKFS_GIB (1).
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=/tmp/<dir> LUNIX_MIN_FREE_GIB=4 bash tests/exit/selftest-fake-root.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIX_SPIKE_DIR:?}" "${LUNIX_ROOT_TESTS:?}"
BIN="$LUNIX_SPIKE_DIR/fakebin"; mkdir -p "$BIN" "$LUNIX_SPIKE_DIR/run"
cp "$here/fakes/fake-lunix.sh" "$BIN/lunix"; cp "$here/fakes/fake-lunix.sh" "$BIN/lunixd"; chmod +x "$BIN"/*
export LUNIXD="$BIN/lunixd" LUNIXD_CP="$BIN/lunixd" LUNIX="$BIN/lunix" TESTD=/bin/false MKFS_GIB=1 VOL_GIB=1 SCRIPT_CAP=1500
export DAEMON_CPUS="${DAEMON_CPUS:-12-13}" FIO_CPUS="${FIO_CPUS:-14-15}"
FAIL=0
run() { # label cmd...   (ONLY="fio points" runs a subset)
  local label=$1; shift
  [ -z "${ONLY:-}" ] || case " $ONLY " in *" $label "*) ;; *) return 0 ;; esac
  echo "=== $label"; "$@" > "$LUNIX_SPIKE_DIR/fake-$label.log" 2>&1; local rc=$?
  tail -n 6 "$LUNIX_SPIKE_DIR/fake-$label.log"; grep -c 'FAIL' "$LUNIX_SPIKE_DIR/fake-$label.log" | sed "s/^/FAIL lines: /"
  if [ "$rc" = 0 ]; then echo "PASS $label"; else echo "FAIL $label (rc $rc)"; FAIL=1; fi
}
run fio env VOL_GIB=2 OUT="$LUNIX_SPIKE_DIR/fake-fio-out" CHUNKS=65536 MODES=integrity TARGETS="raw ext4" PROFILES="8:1 8:2" RUNTIME=8 PER_PROFILE_MIB=64 TRIM_MIB=32 ZERO_MIB=16 VERIFY_GRACE=120 bash "$here/fio-verify.sh"
run crash env OUT="$LUNIX_SPIKE_DIR/fake-crash-out" FS=ext4 PHASES="immediate:3:0 double:2:0" INT_MIN=3 INT_MAX=5 FIO_SIZE=33554432 FIO_JOBS=2 bash "$here/crash-random.sh"
run quiesce env OUT="$LUNIX_SPIKE_DIR/fake-quiesce-out" FS=ext4 MODE=quiesce CYCLES=3 INT_MIN=3 INT_MAX=4 FIO_SIZE=33554432 FIO_JOBS=2 bash "$here/crash-random.sh"
run points env VOL_GIB=3 OUT="$LUNIX_SPIKE_DIR/fake-points-out" WL_SECS=10 POST_SECS=5 COUNT_SECS=8 RUN_CAP_S=90 BUDGET_S=1200 FAKE_CRASH_AFTER=4 bash "$here/crashpoints.sh"
run ratios env OUT="$LUNIX_SPIKE_DIR/fake-ratios-out" CHUNKS=65536 SIZE_GIB=1 VOL_GIB=2 COMPRESS_PCTS="0 50" DEDUPE_PCTS="0 50" ROOTFS_SRC=none bash "$here/ratios.sh"
run negctl env OUT="$LUNIX_SPIKE_DIR/fake-neg-out" NEG_M10=0 VOL_GIB=2 bash "$here/negctl.sh"
echo "overall FAIL=$FAIL"
exit "$FAIL"
