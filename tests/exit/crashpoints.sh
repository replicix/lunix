#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 3(b): kill at every crash point. For every point printed by `lunixd --list-crashpoints` (a build with
# --features crashpoints, LUNIXD_CP), run a workload with LUNIX_CRASH_AT=<point>:<hit> for hit in {1, middle, last
# reached}; the daemon kills itself at that execution of that point, the supervisor (this script) restarts it
# without a crash setting (recovery mode), the load runs on, then the checks of every cycle:
#   fio zero errors and zero verify failures, fsyncer zero bad acknowledged blocks, guest fsck clean, `lunix fsck
#   --deep` clean after detach (exit 0, or 1 with only orphans).
# ext4 for every point; xfs as well for the engine.c* points. Every run is capped at RUN_CAP_S (600) wall clock and
# the whole script at BUDGET_S (default 72000 = 20 h); a run past the cap is aborted and recorded, never extended.
#
# Hit counts: a counting run (LUNIX_CRASH_COUNT) of the same workload gives the number of executions per point;
# `middle` is (count+1)/2 and `last` is count. Counts of two runs differ a little, so a `last` run that finishes
# without the crash is repeated once at 90 % of the count (kind last90). A point the workload never reaches is
# recorded `unreached`, not passed. If the daemon does not write the counts (it must call
# lunix_iface::crashpoint::dump_counts on its clean exit; lunix-ublk-testd does, lunixd did not when this script was
# written) the hits are HITS_FALLBACK (default "1 25 250") and the report says so.
#
# Slices: POINTS_FROM / POINTS_TO (1-based, in sorted order) or POINTS="a b c"; one OUT directory per slice.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd LUNIXD_CP=<abs cp build>/lunixd \
#       LUNIX=<abs>/lunix TESTD=<abs>/lunix-ublk-testd [POINTS_FROM=1 POINTS_TO=20] bash tests/exit/crashpoints.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${BUDGET_S:=72000}" "${RUN_CAP_S:=600}" "${WL_SECS:=120}" "${POST_SECS:=30}" "${COUNT_SECS:=240}" "${CHUNK:=65536}"
: "${HITS_FALLBACK:=1 25 250}" "${VOL_GIB:=32}" "${VOLMODE:=integrity}" "${COMMIT_INTERVAL_MS:=2000}" "${GC_EVERY_S:=15}"
export SCRIPT_CAP="${SCRIPT_CAP:-$((BUDGET_S + 3600))}" COMMIT_INTERVAL_MS
. "$here/lib.sh"; . "$here/crashlib.sh"
: "${LUNIXD_CP:?LUNIXD_CP (lunixd built with --features crashpoints) is not set}"
harness_init m15-crashpoints
DAEMON_BIN="$LUNIXD_CP"
T0=$SECONDS
"$LUNIXD_CP" --list-crashpoints | sort > "$OUT/points-all.txt" || { say "FAIL: $LUNIXD_CP has no crash points (built without the crashpoints feature)"; exit 2; }
if [ -n "${POINTS:-}" ]; then tr ' ' '\n' <<< "$POINTS" | sort > "$OUT/points.txt"
else sed -n "${POINTS_FROM:-1},${POINTS_TO:-\$}p" "$OUT/points-all.txt" > "$OUT/points.txt"; fi
say "# M15 crash points ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus): $(wc -l < "$OUT/points.txt") of $(wc -l < "$OUT/points-all.txt") points, run cap $RUN_CAP_S s, budget $BUDGET_S s"

CRASHED=0
ensure_up() { # restart a dead daemon in recovery mode (no crash setting); returns 0 when it restarted one
  daemon_alive && return 1
  CRASH_ENV=""
  [ -n "${DPID:-}" ] && { wait "$DPID" 2>/dev/null; DRC=$?; }
  DPID=""; CRASHED=1; CRASH_T=$((SECONDS - RUN_T0))
  start_lunixd || wedged "lunixd did not restart after the crash point (exit $DRC)"
  if [ -n "$CUR_DEV" ] && [ -e "$ST/dev/$CUR_VOL" ]; then wait_recovered "$DLOG" 120 || say "- (no recovery line in $DLOG: the volume may not have been attached yet)"; fi
  return 0
}
vol_exists() { lx volume list --json 2>/dev/null | jq -e --arg n "$1" '.[] | select(.name==$n)' >/dev/null; }
GC_PID=""
stop_gc_loop() { [ -n "$GC_PID" ] && { kill "$GC_PID" 2>/dev/null; wait "$GC_PID" 2>/dev/null; GC_PID=""; }; return 0; }
add_cleanup stop_gc_loop

# one_run LABEL FS CRASH_ENV_WORDS: setup, workload with the crash setting, checks. Sets RUN_STATUS and the fields.
one_run() {
  local label=$1 fs=$2; CRASH_ENV=$3
  RUN_T0=$SECONDS; CRASHED=0; CRASH_T=""; RUN_STATUS=pass; RUN_NOTES=""
  local vol="cp$$"
  CUR_VOL=$vol; CUR_MODE=$VOLMODE; CUR_DEV=""
  cell_reset
  start_lunixd || wedged "lunixd did not start for $label"
  local tries=0
  until vol_exists "$vol"; do
    mkvol "$vol" $((VOL_GIB * GIB)) "$CHUNK" "$VOLMODE"; ensure_up
    tries=$((tries + 1)); [ $tries -ge 3 ] && break
  done
  tries=0
  while [ -z "$CUR_DEV" ] || [ ! -b "$CUR_DEV" ]; do
    attach_vol "$vol"; ensure_up
    CUR_DEV=$(dev_of "$vol")
    tries=$((tries + 1)); [ $tries -ge 3 ] && break
  done
  [ -b "$CUR_DEV" ] || { RUN_STATUS=fail; RUN_NOTES="setup: no device"; return; }
  register_id "${CUR_DEV#/dev/ublkb}"
  # mkfs and mount: a crash inside them only delays them (the kernel holds the requests until a daemon serves)
  mkfs_fs "$fs" "$CUR_DEV" || { RUN_STATUS=fail; RUN_NOTES="setup: mkfs"; return; }
  ensure_up
  mount_fs "$fs" "$CUR_DEV" "$MNT" || { RUN_STATUS=fail; RUN_NOTES="setup: mount"; return; }
  local rt=$((WL_SECS + POST_SECS + 120))
  wl_start "$label" "$fs" "$rt" 268435456 4
  ( while :; do sleep "$GC_EVERY_S"; timeout -k 5 120 "$LUNIX" --socket "$SOCK" gc "$vol" >/dev/null 2>&1; done ) </dev/null >/dev/null 2>&1 &
  GC_PID=$!
  # run until the crash (then POST_SECS more), or WL_SECS without a crash, or the run cap
  local deadline_post=""
  while :; do
    sleep 0.5
    if ! daemon_alive; then
      if [ "$CRASHED" = 0 ]; then ensure_up; deadline_post=$((SECONDS + POST_SECS)); say "- crashed after ${CRASH_T}s (exit $DRC)"
      else wedged "daemon died again without a crash setting ($label)"; fi
    fi
    [ -n "$deadline_post" ] && [ "$SECONDS" -ge "$deadline_post" ] && break
    [ -z "$deadline_post" ] && [ $((SECONDS - RUN_T0)) -ge "$WL_SECS" ] && break
    [ $((SECONDS - RUN_T0)) -ge "$RUN_CAP_S" ] && { RUN_NOTES="$RUN_NOTES;run cap"; break; }
    wl_alive || { RUN_NOTES="$RUN_NOTES;load ended early"; break; }
  done
  stop_gc_loop
  if [ "$CRASHED" = 0 ]; then RUN_STATUS=unreached; fi
  # stop the load: it ends by its own runtime (rt) at the latest; TERM it now so the run stays short
  WL_STOPPED=1; kill -TERM "$FIO_PID" 2>/dev/null
  wl_finish "$fs" 600
  detach_vol "$vol"; local drc=$?
  lunix_fsck "$vol" "$label"; local frc=$?
  { wl_ok && [ "$drc" = 0 ] && [ "$frc" = 0 ]; } || RUN_STATUS=fail
  WL_STOPPED=0
  [ "$CRASHED" = 0 ] && [ "$RUN_STATUS" = fail ] && RUN_STATUS=fail
  RUN_SECS=$((SECONDS - RUN_T0))
  [ $RUN_SECS -gt "$RUN_CAP_S" ] && RUN_NOTES="$RUN_NOTES;exceeded the ${RUN_CAP_S}s cap"
  RUN_REC_MS=$(daemon_ms "$DLOG")
}
report_run() { # CELL POINT KIND HIT FS
  rec "$1" "$RUN_STATUS" point="$2" kind="$3" hit="$4" fs="$5" crashed="$CRASHED" crash_after_s="${CRASH_T:-}" secs="$RUN_SECS" \
      recovery_stall_ms="${RUN_REC_MS:-}" fio_rc="$WL_FIO_RC" fio_errors="$WL_FIO_ERRS" guest_fsck="$WL_FSCK_RC" \
      fsyncer_verify="$WL_VERIFY_RC" lunix_fsck="$FSCK_RC" notes="$RUN_NOTES"
  case "$RUN_STATUS" in
    pass) pass "$1 (crashed after ${CRASH_T}s, recovery stall ${RUN_REC_MS:-?} ms)" ;;
    unreached) say "- UNREACHED $1: the workload finished without executing hit $4 of $2" ;;
    *) fail "$1 (fio rc $WL_FIO_RC errors $WL_FIO_ERRS, guest fsck $WL_FSCK_RC, fsyncer $WL_VERIFY_RC, lunix fsck $FSCK_RC;$RUN_NOTES)"; FAIL=1 ;;
  esac
}
budget_left() { [ $((SECONDS - T0 + RUN_CAP_S + 300)) -lt "$BUDGET_S" ]; }

# ---- counting run: the same workload with the daemon counting every point, clean exit ---------------------------
COUNTS="${COUNTS:-$OUT/counts.txt}"
if [ ! -s "$COUNTS" ]; then
  say ""; say "## counting run ($COUNT_SECS s of the workload)"
  WL_SECS_ORIG=$WL_SECS; POST_SECS_ORIG=$POST_SECS; WL_SECS=$COUNT_SECS; POST_SECS=0
  one_run count ext4 "LUNIX_CRASH_COUNT=$OUT/counts.raw" || true
  stop_lunixd || wedged "the counting daemon did not exit"   # the counts are written on its clean exit
  [ -s "$OUT/counts.raw" ] && cp "$OUT/counts.raw" "$OUT/counts.txt"
  WL_SECS=$WL_SECS_ORIG; POST_SECS=$POST_SECS_ORIG
fi
[ -s "$COUNTS" ] && COUNT_SRC=counted || COUNT_SRC="HITS_FALLBACK ($HITS_FALLBACK: the daemon wrote no counts)"
say "- hit counts: $COUNT_SRC"

while read -r p; do
  [ -z "$p" ] && continue
  budget_left || { say "- BUDGET reached: $p and the points after it are not run"; rec "crashpoint/$p" skip why=budget; continue; }
  if [ -s "$COUNTS" ]; then
    c=$(awk -v p="$p" '$1==p{print $2}' "$COUNTS"); c=${c:-0}
    if [ "$c" = 0 ]; then say "- UNREACHED $p: the counting run never executed it"; rec "crashpoint/$p" unreached why=count0; continue; fi
    hits="first:1 mid:$(((c + 1) / 2)) last:$c"
  else hits="first:$(echo "$HITS_FALLBACK" | cut -d' ' -f1) mid:$(echo "$HITS_FALLBACK" | cut -d' ' -f2) last:$(echo "$HITS_FALLBACK" | cut -d' ' -f3)"; fi
  fss="ext4"; case "$p" in engine.c*) fss="ext4 xfs" ;; esac
  for fs in $fss; do
    for hk in $hits; do
      kind=${hk%%:*}; hit=${hk##*:}
      budget_left || { say "- BUDGET reached before $p:$hit ($fs)"; rec "crashpoint/$p/$kind/$fs" skip why=budget; continue; }
      cell="crashpoint/$p/$kind-$hit/$fs"; say ""; say "## $cell"
      one_run "$(echo "$p-$kind-$fs" | tr '.' '_')" "$fs" "LUNIX_CRASH_AT=$p:$hit"
      report_run "$cell" "$p" "$kind" "$hit" "$fs"
      if [ "$kind" = last ] && [ "$RUN_STATUS" = unreached ] && [ "$hit" -gt 4 ]; then
        h90=$((hit * 9 / 10)); cell="crashpoint/$p/last90-$h90/$fs"; say "## $cell (last run did not reach the count)"
        one_run "$(echo "$p-last90-$fs" | tr '.' '_')" "$fs" "LUNIX_CRASH_AT=$p:$h90"
        report_run "$cell" "$p" last90 "$h90" "$fs"
      fi
      [ -e "$OUT/WEDGED" ] && break 3
    done
  done
done < "$OUT/points.txt"
say ""; say "## end"
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
