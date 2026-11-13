#!/bin/bash
# Runs tests/device_e2e.rs (MemBlockEngine + DeviceOracle behind real devices) as root, one test thread.
# The test binary is built beforehand as the normal user (never compile as root):
#   tools/remote.sh cargo test -p lunix-ublk --test device_e2e --no-run
#   TEST_BIN=$(ls -t ~/lunix-p1/m10/target/debug/deps/device_e2e-* | grep -v '\.d$' | head -1)
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=... TEST_BIN=$TEST_BIN bash .../run-e2e.sh
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib.sh"
require_root; need_root_tests
: "${TEST_BIN:?TEST_BIN is not set}" "${OUT:=$LUNIX_SPIKE_DIR/out-e2e}"
rm -rf "$OUT"; mkdir -p "$OUT"
bash "$here/../../../../spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; echo "not clean at start"; exit 2; }
: > "$CREATED_DEVICES"   # only devices recorded by this run are swept
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
# Whatever happens, devices this run created and recorded must not outlive it without a daemon:
# the sweep deletes the recorded ones that still exist (nothing is mounted in these tests).
sweep() {
  trap - EXIT INT TERM
  local d
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do
    [ -b "$d" ] && [ -n "${TESTD:-}" ] && { echo "sweep: deleting $d"; timeout -k 5 75 "$TESTD" del --id "${d#/dev/ublkb}" --force; }
  done
  true
}
add_cleanup sweep
taskset -c "${E2E_CPUS:-4-7}" timeout -s TERM -k 120 "${E2E_TIMEOUT:-1500}" "$TEST_BIN" --ignored --test-threads 1 --nocapture 2>&1 | tee "$OUT/e2e.log"
rc=${PIPESTATUS[0]}
echo "e2e rc=$rc" | tee "$OUT/result.txt"
exit "$rc"
