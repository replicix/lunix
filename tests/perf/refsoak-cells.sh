#!/bin/bash
# shellcheck disable=SC2319
# refsoak: one cell of the real-daemon shadow soak of the C5 reference table = one fresh daemon, one fresh volume, REPS (5)
# repetitions, `lunix fsck --deep` at the end. Root, on the test host, through tools/remote.sh root (via tests/perf/refsoak.sh).
#
#   CELL   c1  uniform 4K random write            (volume chunk CHUNK, the T6 w4k job)
#          c2  the same at 16K chunks             (CHUNK = 16384)
#          c3  zipf:1.2 4K random write           (w4kz.fio)
#          c4  dedup-heavy: 64K random write, dedupe_percentage = 50 (w64d.fio)
#          c5  compaction-heavy: per repetition 4K random write over a 4 GiB working set, a 1M sequential rewrite of its
#              first half, then 4K random write over the second half (comp.fio): many half-live segments
#          c6  large discard: per repetition refill [4 GiB, 10 GiB), blkdiscard it (6 GiB = 37.5 % of the 16 GiB volume), then w4k
#   RESTART_REP / RESTART_AT: in that repetition (c1: 3), RESTART_AT s into the measured window, one clean daemon stop through the
#          daemon's own shutdown path (stop_lunixd of lib.sh: TERM = quiesce every device, exit; a stopping daemon is never
#          killed) and a start that recovers the attached device while fio keeps running.
# Every 5 s (SAMPLE_S) `lunix gc --status --json` and `lunix status --json` go to series.jsonl with the phase name; once per
# second `lunix status --json` is timed (probe.tsv). reps.tsv has the window of every phase.
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR OUT CELL CHUNK; CFG_EXTRA (the arm's daemon settings), LOG_LEVEL.
here="$(cd "$(dirname "$0")" && pwd)"
: "${CELL:?}" "${CHUNK:?}" "${REPS:=5}" "${RAMP:=5}" "${W_RUNTIME:=60}" "${VOL_GIB:=16}" "${WS_GIB:=2}" "${JOBS:=8}" "${SAMPLE_S:=5}"
: "${DAEMON_CPUS:=0-7}" "${FIO_CPUS:=8-15}" "${QCPUS:=0-3}" "${VOLMODE:=integrity}" "${DESTAGE_WORKERS:=8}"
: "${RESTART_REP:=0}" "${RESTART_AT:=30}" "${CWS_GIB:=4}" "${CRUN:=30}" "${DISC_OFF_GIB:=4}" "${DISC_LEN_GIB:=6}"
CFG_EXTRA="${CFG_EXTRA:+$CFG_EXTRA
}destage_workers = $DESTAGE_WORKERS"
export CFG_EXTRA JOURNAL_MAX="${JOURNAL_MAX:-1073741824}" DAEMON_CPUS FIO_CPUS
export SCRIPT_CAP="${SCRIPT_CAP:-14400}"
. "$here/lib.sh"; . "$here/perflib.sh"
harness_init "refsoak-$CELL"
JOBDIR="$REPO/bench/phase1/perf/fio"
say "# refsoak cell $CELL ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus): chunk $CHUNK, $REPS reps, restart rep $RESTART_REP"
say "- daemon settings: $(echo "$CFG_EXTRA" | tr '\n' ';'); log_level ${LOG_LEVEL:-info}"
NOISY=0; quiet_check; gov_set
df_guard "$LUNIX_SPIKE_DIR" "${DF_GIB:-1}" || exit 2
WS=$((WS_GIB * GIB)); SLICE=$((WS / JOBS)); VOL=refsoak
CWS=$((CWS_GIB * GIB)); CHALF=$((CWS / 2))
PHASEF="$OUT/phase"; echo start > "$PHASEF"; : > "$OUT/reps.tsv"
now_ms() { date +%s%3N; }
phase() { echo "$1" > "$PHASEF"; }

SPID=""; PPID_=""
sampler() {
  while sleep "$SAMPLE_S"; do
    local t g s ph a b
    t=$(now_ms); ph=$(cat "$PHASEF" 2>/dev/null)
    a=$(date +%s%N); g=$(LX_TIMEOUT=20 lx gc "$VOL" --status --json 2>/dev/null); b=$(date +%s%N)
    s=$(LX_TIMEOUT=20 lx status "$VOL" --json 2>/dev/null | jq -c '{write_path,throttle,space,journal_used,stats:{commits:.stats.commits}}' 2>/dev/null)
    jq -c -n --arg t "$t" --arg ph "$ph" --arg ms "$(((b - a) / 1000000))" --argjson gc "${g:-null}" --argjson st "${s:-null}" \
      '{t:($t|tonumber),phase:$ph,gc_call_ms:($ms|tonumber),gc:$gc,status:$st}' >> "$OUT/series.jsonl" 2>/dev/null
  done
}
prober() {
  while sleep 1; do
    local a b rc
    a=$(date +%s%N); LX_TIMEOUT=30 lx status "$VOL" --json >/dev/null 2>&1; rc=$?; b=$(date +%s%N)
    printf '%s\t%s\t%s\t%s\n' "$(now_ms)" "$(((b - a) / 1000000))" "$rc" "$(cat "$PHASEF" 2>/dev/null)" >> "$OUT/probe.tsv"
  done
}

# run_fio LABEL JOBFILE RUNTIME_S QD REP [restart]: one bounded fio, its window appended to reps.tsv
CELLNO=0
run_fio() {
  local label=$1 job=$2 rt=$3 qd=$4 rep=$5 restart=${6:-0}
  CELLNO=$((CELLNO + 1))
  local grace=$((rt / 2 + 60)) t0 t1 fp frc rc2
  export DEV="$CUR_DEV" WS SLICE RUNTIME=$rt RAMP CPUS="$FIO_CPUS" JOBS QD=$qd SEED=$((100 + CELLNO)) LATLOG="$OUT/raw/lat-$label"
  export CWS CHALF CSLICE=$((CWS / JOBS)) CHSLICE=$((CHALF / JOBS)) CRUN
  read -r l1 l5 _ < /proc/loadavg
  phase "$label"; t0=$(now_ms)
  FIO_GRACE=$grace fio_bg "$label" "$rt" "$JOBDIR/$job.fio"; fp=$LAST_FIO
  if [ "$restart" = 1 ]; then
    sleep "$((RAMP + RESTART_AT))"
    lx gc "$VOL" --status --json > "$OUT/raw/gc-before-restart.json" 2>/dev/null
    lx gc "$VOL" --status > "$OUT/raw/gc-before-restart.txt" 2>/dev/null
    phase "$label-restart"; local r0 r1 r2; r0=$(now_ms)
    stop_lunixd || wedged "clean stop of the daemon failed"
    r1=$(now_ms)
    start_lunixd || wedged "daemon did not restart"
    r2=$(now_ms)
    local k st=""
    for k in $(seq 1 600); do
      st=$(status_json "$VOL" 2>/dev/null | jq -r '.state // empty'); [ "$st" = attached ] && break; sleep 0.2
    done
    printf 'restart\t%s\t%s\t%s\t%s\tstate=%s\n' "$r0" "$r1" "$r2" "$(now_ms)" "$st" >> "$OUT/restart.tsv"
    say "- restart in $label: stop $((r1 - r0)) ms, start $((r2 - r1)) ms, state '$st' after $(($(now_ms) - r0)) ms"
    phase "$label"
  fi
  wait_fio "$fp" $((rt + grace + 600)); frc=$?
  t1=$(now_ms)
  local jf="$OUT/raw/fio-${label//\//_}.json"
  [ "$frc" = 0 ] && [ "$(fio_errors "$jf")" = 0 ]; rc2=$?
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$rep" "$label" "$t0" "$t1" "$l1" "$l5" "$([ $rc2 = 0 ] && echo ok || echo FIO_ERROR)" >> "$OUT/reps.tsv"
  say "- $label: fio rc $frc, errors $(fio_errors "$jf"), $(fio_summary "$jf" | awk '{printf "%d IOPS, p99 %.0f us", $1, $4}')"
  [ "$rc2" = 0 ] || FAIL=1
}

# ---- the block ----------------------------------------------------------------------------------------------
cell_reset; start_lunixd || wedged "lunixd did not start"
mkvol "$VOL" $((VOL_GIB * GIB)) "$CHUNK" "$VOLMODE"; expect "create $VOL" $? || exit 1
attach_pinned "$VOL" 4 "$QCPUS"; expect "attach with 4 queues" $? || exit 1
CUR_VOL=$VOL; CUR_DEV=$(dev_of "$VOL"); register_id "${CUR_DEV#/dev/ublkb}"; [ -b "$CUR_DEV" ] || wedged "no device"
sleep 5
export DEV="$CUR_DEV" WS RUNTIME=60 RAMP CPUS="$FIO_CPUS" JOBS QD=32 SEED=1
phase fill; FIO_GRACE=600 fio_run "fill" "$((WS_GIB * 60))" "$JOBDIR/fill.fio"; expect "write the ${WS_GIB} GiB working set" $? || FAIL=1
# the first attach has no cut yet: let the daemon settle a moment, then start sampling
sampler & SPID=$!; prober & PPID_=$!
add_cleanup "kill $SPID $PPID_ 2>/dev/null"
t_block0=$(now_ms); echo "$t_block0" > "$OUT/block-start-ms.txt"

for rep in $(seq 1 "$REPS"); do
  rs=0; [ "$rep" = "$RESTART_REP" ] && rs=1
  case "$CELL" in
    c1|c2) run_fio "rep$rep-w4k" w4k "$W_RUNTIME" 128 "$rep" "$rs" ;;
    c3) run_fio "rep$rep-w4kz" w4kz "$W_RUNTIME" 128 "$rep" "$rs" ;;
    c4) run_fio "rep$rep-w64d" w64d "$W_RUNTIME" 32 "$rep" "$rs" ;;
    c5)
      run_fio "rep$rep-comp" comp "$((CRUN * 2 + 40))" 128 "$rep" "$rs" ;;
    c6)
      export DEV="$CUR_DEV" OFF=$((DISC_OFF_GIB * GIB)) LEN=$((DISC_LEN_GIB * GIB)) RAMP SEED=$((50 + rep)) CPUS="$FIO_CPUS"
      run_fio "rep$rep-refill" fillr 60 32 "$rep"
      phase "rep$rep-discard"; d0=$(now_ms)
      timeout -k 10 900 blkdiscard -o "$OFF" -l "$LEN" "$CUR_DEV"; drc=$?
      d1=$(now_ms)
      printf '%s\trep%s-discard\t%s\t%s\t0\t0\t%s\n' "$rep" "$rep" "$d0" "$d1" "$([ $drc = 0 ] && echo ok || echo DISCARD_RC_$drc)" >> "$OUT/reps.tsv"
      say "- rep$rep discard of $DISC_LEN_GIB GiB at $DISC_OFF_GIB GiB: rc $drc, $((d1 - d0)) ms"
      [ "$drc" = 0 ] || FAIL=1
      run_fio "rep$rep-w4k" w4k "$W_RUNTIME" 128 "$rep" ;;
    *) echo "unknown cell $CELL" >&2; exit 2 ;;
  esac
done
phase end
sleep "$((SAMPLE_S * 2))"
kill "$SPID" "$PPID_" 2>/dev/null; wait "$SPID" "$PPID_" 2>/dev/null
lx gc "$VOL" --status --json > "$OUT/raw/gc-final.json" 2>/dev/null
lx gc "$VOL" --status > "$OUT/raw/gc-final.txt" 2>/dev/null
status_json "$VOL" > "$OUT/raw/status-final.json" 2>/dev/null
detach_vol "$VOL"; expect "detach" $? || FAIL=1
lunix_fsck "$VOL" refsoak-"$CELL"; expect "lunix fsck --deep clean (exit $FSCK_RC)" $? || FAIL=1
cp "$(vol_dir "$VOL" 2>/dev/null || echo /nonexistent)/volume.json" "$OUT/raw/" 2>/dev/null
stop_lunixd || FAIL=1
say ""; say "reference table log lines: $(cat "$OUT"/lunixd.*.log | grep -c 'reference table')"
say "EIO/ENOSPC log lines: $(cat "$OUT"/lunixd.*.log | grep -c -E 'EIO|ENOSPC|No space left|Input/output error')"
say "overall FAIL=$FAIL (noisy host: $NOISY)"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
