#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 2: xfstests `-g quick` on ext4 and xfs, TEST and SCRATCH on Lunix volumes (16 GiB each), at chunk sizes
# 64 KiB and 16 KiB, and the same groups on a reference (loop devices over files on the same disk) in the same session.
#
#   LOOP_SECTOR=4096  (loop reference only: logical sector size of the loop devices; Lunix devices have 4096)
#   TARGET=lunix|loop   FS=ext4|xfs   CHUNK=65536|16384   GROUP=quick   TESTS="generic/001 ..." (overrides the group)
#   XFSTESTS_DIR=<abs>  (a built xfstests tree; the lead's host directory, in-tree `./check`)
#   DEV_GIB=16 (TEST and SCRATCH size)   VOLMODE=integrity|encrypted   CHECK_CAP_S (the cap of `./check`, 21600)
# Gate (done by xfstests-compare.py over the result directories): every test that passes on the reference passes on
# Lunix; tests failing on both are listed with the reason; tests failing only on Lunix are defects.
#
# Runtime: -g quick is 1-3 h per file system and target (about 6 runs x 2 h = 12 h in all); disk: two 16 GiB volumes
# (loop: two 16 GiB sparse files), the store stays below 20 GiB per run; df-guard asks for 2 x DEV_GIB + 4.
# Safety: mounts only under $LUNIX_SPIKE_DIR/mnt (nosuid,nodev); xfstests runs with `-x dangerous`; the script cap
# signals the script (TERM), the trap stops `./check`, unmounts, detaches. No users are created (tests needing the
# `fsgqa` users report "not run" on both targets).
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=... LUNIX=... TESTD=... \
#       XFSTESTS_DIR=<abs>/xfstests TARGET=lunix FS=ext4 CHUNK=65536 bash tests/exit/xfstests.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${TARGET:=lunix}" "${FS:=ext4}" "${CHUNK:=65536}" "${GROUP:=quick}" "${DEV_GIB:=16}" "${VOLMODE:=integrity}" "${CHECK_CAP_S:=21600}"
export SCRIPT_CAP="${SCRIPT_CAP:-$((CHECK_CAP_S + 3600))}"
. "$here/lib.sh"
: "${XFSTESTS_DIR:?XFSTESTS_DIR (a built xfstests tree) is not set}"
if [ "$TARGET" = loop ]; then : "${LUNIXD:=/nonexistent}" "${LUNIX:=/nonexistent}"; fi   # the reference run needs no Lunix binaries
harness_init "m15-xfstests-$TARGET-$FS$([ "$TARGET" = lunix ] && echo "-$((CHUNK / 1024))k")"
[ -x "$XFSTESTS_DIR/check" ] || { say "FAIL: $XFSTESTS_DIR/check is not built"; exit 2; }
TMNT="$LUNIX_SPIKE_DIR/mnt/$NAME-test"; SMNT="$LUNIX_SPIKE_DIR/mnt/$NAME-scratch"; XTMP="$OUT/tmp"; RESDIR="$OUT/results"
need_safe "$TMNT"; need_safe "$SMNT"; mkdir -p "$TMNT" "$SMNT" "$XTMP" "$RESDIR"
say "# M15 xfstests TARGET=$TARGET FS=$FS chunk=$CHUNK group=${TESTS:-$GROUP} ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus)"
say "- xfstests: $(git -C "$XFSTESTS_DIR" log -1 --format='%h %cs' 2>/dev/null || echo unknown); fsck tools: $(e2fsck -V 2>&1 | head -1), $(xfs_repair -V 2>&1)"
df_guard "$LUNIX_SPIKE_DIR" $((2 * DEV_GIB + 4)) || exit 2

LOOPS=()
# Reference: two sparse files on the same disk behind loop devices (direct I/O like a real device).
make_devices_loop() {
  local f n
  for n in test scratch; do
    f="$LUNIX_SPIKE_DIR/$NAME-$n.img"; need_safe "$f"; truncate -s "${DEV_GIB}G" "$f"
    local l; l=$(losetup -f --show --direct-io=on ${LOOP_SECTOR:+--sector-size "$LOOP_SECTOR"} "$f") || wedged "losetup failed"
    register_device "$l"; LOOPS+=("$l"); eval "DEV_${n^^}=$l"
  done
  add_cleanup 'for l in "${LOOPS[@]}"; do losetup -d "$l"; done; rm -f "$LUNIX_SPIKE_DIR/$NAME-test.img" "$LUNIX_SPIKE_DIR/$NAME-scratch.img"'
}
make_devices_lunix() {
  CUR_VOL=xt; CUR_MODE=$VOLMODE
  cell_reset; start_lunixd || wedged "lunixd did not start"
  local n
  for n in xt xs; do
    CUR_VOL=$n
    mkvol "$n" $((DEV_GIB * GIB)) "$CHUNK" "$VOLMODE"; expect "create $n (${DEV_GIB} GiB, chunk $CHUNK)" $? || exit 1
    attach_vol "$n"; expect "attach $n" $? || exit 1
    local dv; dv=$(dev_of "$n"); [ -b "$dv" ] || wedged "no device for $n"
    register_id "${dv#/dev/ublkb}"
    if [ "$n" = xt ]; then DEV_TEST=$dv; else DEV_SCRATCH=$dv; fi
  done
}
if [ "$TARGET" = loop ]; then make_devices_loop; else make_devices_lunix; fi
need_safe "$DEV_TEST"; need_safe "$DEV_SCRATCH"
say "- TEST_DEV $DEV_TEST, SCRATCH_DEV $DEV_SCRATCH"

# local.config from the template, a single exclude file
sed -e "s|@FSTYP@|$FS|" -e "s|@TEST_DEV@|$DEV_TEST|" -e "s|@TEST_DIR@|$TMNT|" -e "s|@SCRATCH_DEV@|$DEV_SCRATCH|" \
    -e "s|@SCRATCH_MNT@|$SMNT|" -e "s|@RESULT_BASE@|$RESDIR|" -e "s|@TMPDIR@|$XTMP|" "$here/xfstests/local.config.in" > "$OUT/local.config"
cat "$here/xfstests/exclude.common" "$here/xfstests/exclude.$FS" > "$OUT/exclude"
grep -v '^[[:space:]]*#' "$OUT/exclude" | grep -q . || say "- exclude list: empty"

mkfs_fs "$FS" "$DEV_TEST" || wedged "mkfs of the TEST device failed"
xfstests_cleanup() {
  [ -n "${CHECK_PID:-}" ] && kill -TERM "$CHECK_PID" 2>/dev/null
  local i; for i in $(seq 1 120); do [ -n "${CHECK_PID:-}" ] && kill -0 "$CHECK_PID" 2>/dev/null || break; sleep 1; done
  mountpoint -q "$SMNT" && timeout -k 10 180 umount "$SMNT"
  mountpoint -q "$TMNT" && timeout -k 10 180 umount "$TMNT"
  return 0
}
add_cleanup xfstests_cleanup

# On a host whose coreutils are the Rust ones (uutils, Ubuntu 26.04) xfstests prints `error: error: invalid value for one of the
# arguments` in front of the output of 244 of 684 ext4 tests (the message is in /usr/bin/coreutils, clap's wording); the tests
# are written against GNU coreutils. Where the package `gnu-coreutils` ships /usr/bin/gnu<tool>, a directory of <tool> links
# to them goes first in PATH for ./check, on the reference and on Lunix alike.
GNUBIN="$OUT/gnubin"; mkdir -p "$GNUBIN"
for g in /usr/bin/gnu*; do [ -x "$g" ] && ln -sf "$g" "$GNUBIN/$(basename "$g" | sed 's/^gnu//')"; done
SEL=(-g "$GROUP" -x dangerous); [ -n "${TESTS:-}" ] && read -ra SEL <<< "$TESTS"
t0=$SECONDS
( cd "$XFSTESTS_DIR" && PATH="$GNUBIN:$PATH" HOST_OPTIONS="$OUT/local.config" timeout -k 120 "$CHECK_CAP_S" ./check -E "$OUT/exclude" "${SEL[@]}" > "$OUT/check.stdout" 2>&1 ) &
CHECK_PID=$!
# Stall watchdog (the same rule for the reference and for Lunix): `./check` has no per-test timeout, and one test that never
# ends costs the whole run (xfs/118 hung for 4 h on the plain loop reference and the run hit its cap). When ./check's output
# has not grown for STALL_S seconds (default 1800; every test prints at its start and its end), the test that is running is
# killed (its process tree, KILL) and the event is appended to stalled-tests.txt; ./check then records the test as failed
# and goes on. A test that stalls on both targets is "fails on both" in the comparison; one that stalls only on Lunix is a defect.
tree() { local c; for c in $(pgrep -P "$1"); do tree "$c"; done; echo "$1"; }
(
  last=-1; since=$SECONDS
  while kill -0 "$CHECK_PID" 2>/dev/null; do
    sleep 60
    sz=$(stat -c %s "$OUT/check.stdout" 2>/dev/null || echo 0)
    if [ "$sz" != "$last" ]; then last=$sz; since=$SECONDS; continue; fi
    [ $((SECONDS - since)) -ge "${STALL_S:-1800}" ] || continue
    t=$(tail -c 300 "$OUT/check.stdout" | grep -o '[a-z0-9]*/[0-9][0-9]*' | tail -1)
    echo "$(date -u +%FT%TZ) stalled ${t:-unknown}" >> "$OUT/stalled-tests.txt"
    root=$(pgrep -f "tests/${t:-nonexistent}\$" | head -1)
    [ -n "$root" ] && for p in $(tree "$root"); do kill -KILL "$p" 2>/dev/null; done
    since=$SECONDS
  done
) &
WD_PID=$!
wait "$CHECK_PID"; CRC=$?; kill "$WD_PID" 2>/dev/null; CHECK_PID=""
secs=$((SECONDS - t0))
say "- ./check ended with status $CRC after ${secs}s"
[ "$CRC" = 124 ] && fail "./check hit its cap of $CHECK_CAP_S s (aborted, not extended)"
cp "$RESDIR"/check.log "$OUT/check.log" 2>/dev/null; cp "$RESDIR"/check.time "$OUT/check.time" 2>/dev/null
tail -n 12 "$OUT/check.stdout" | tee -a "$R"
# umount before anything else touches the devices; then fsck the guest file systems and, for Lunix, detach and fsck
xfstests_cleanup
guest_fsck "$FS" "$DEV_TEST"
expect "guest fsck of the TEST device clean after the run" $? || true
if [ "$TARGET" = lunix ]; then
  for n in xt xs; do CUR_VOL=$n; detach_vol "$n"; expect "detach $n" $? || true; lunix_fsck "$n" "$n"; expect "lunix fsck --deep $n clean (exit $FSCK_RC)" $? || true; done
fi
[ -s "$OUT/stalled-tests.txt" ] && say "- STALLED tests (killed by the watchdog): $(cut -d" " -f3 "$OUT/stalled-tests.txt" | tr "\n" " ")"
rec "xfstests/$TARGET/$FS/$([ "$TARGET" = lunix ] && echo "c$((CHUNK / 1024))k" || echo ref)" info check_status="$CRC" secs="$secs" results="$RESDIR" group="${TESTS:-$GROUP}"
say "- compare with: python3 bench/phase1/exit/xfstests-compare.py <reference dir> <lunix dir>..."
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
