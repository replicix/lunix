#!/bin/bash
# Root self-test of the control flow of the M16 scripts against a FAKE lunix and lunixd (fakes/fake-lunix.sh: loop
# devices instead of ublk), with tiny parameters: Part A (including the null_blk reference and the verify_reads switch),
# and the chunk-size benchmark with the two fio workloads and a scale-1 pgbench. It finds script bugs; the numbers mean
# nothing. Disk/RAM: a few GiB under LUNIX_SPIKE_DIR (use a tmpfs directory on the host for a dry run).
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_SPIKE_DIR=/tmp/<dir> LUNIX_MIN_FREE_GIB=4 bash tests/perf/selftest-fake-root.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${LUNIX_SPIKE_DIR:?}" "${LUNIX_ROOT_TESTS:?}"
BIN="$LUNIX_SPIKE_DIR/fakebin"; mkdir -p "$BIN" "$LUNIX_SPIKE_DIR/run"
cp "$here/fakes/fake-lunix.sh" "$BIN/lunix"; cp "$here/fakes/fake-lunix.sh" "$BIN/lunixd"; chmod +x "$BIN"/*
export LUNIXD="$BIN/lunixd" LUNIX="$BIN/lunix" TESTD=/bin/false MKFS_GIB=1 SCRIPT_CAP=2400 DF_GIB=2
FAIL=0
run() {
  local label=$1; shift
  [ -z "${ONLY:-}" ] || case " $ONLY " in *" $label "*) ;; *) return 0 ;; esac
  echo "=== $label"; "$@" > "$LUNIX_SPIKE_DIR/fake-$label.log" 2>&1; local rc=$?
  tail -n 5 "$LUNIX_SPIKE_DIR/fake-$label.log"
  if [ "$rc" = 0 ]; then echo "PASS $label"; else echo "FAIL $label (rc $rc)"; FAIL=1; fi
}
run parta env OUT="$LUNIX_SPIKE_DIR/fake-parta-out" CHUNKS=65536 QUEUES_LIST=4 REPS=2 RUNTIME=6 RAMP=2 W_RUNTIME=6 WS_GIB=1 VOL_GIB=4 PERF=0 \
    DAEMON_CPUS=12-13 FIO_CPUS=14-15 QCPUS_4=12-13 JOBS=4 bash "$here/partA.sh"
run chunksize env OUT="$LUNIX_SPIKE_DIR/fake-chunk-out" CHUNKS=65536 WORKLOADS="randmix seqrand pgbench" VOL_GIB=3 RM_RUNTIME=10 RM_SIZE_GIB=1 SR_RUNTIME=8 \
    SR_SIZE_GIB=1 PG_SCALE=1 PG_TIME=10 PG_SHARED_BUFFERS=128MB FS_PGBENCH=ext4 GC_PASSES=1 GC_WAIT=2 QUEUES=2 DAEMON_CPUS=12-13 WL_CPUS=14-15 QCPUS=12-13 \
    bash "$here/chunksize.sh"
echo "overall FAIL=$FAIL"
exit "$FAIL"
