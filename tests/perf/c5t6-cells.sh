#!/bin/bash
# shellcheck disable=SC2319
# c5t6: one block of the real-daemon T6 of active C5 (condemn-on-cut) = one fresh daemon, one fresh volume, REPS (5) repetitions of one fio
# job, `lunix fsck --deep` at the end. Root, on the test host, through tools/remote.sh root (via tests/perf/c5t6.sh).
#
#   CELL   w4k   uniform 4K random write            (volume chunk CHUNK; the T6 w4k job)
#          w4kz  zipf:1.2 4K random write           (w4kz.fio)
#          pin   as w4k, with a snapshot pin on the tip commit held through the block (plain-mode volume: the pin is sealed with the
#                keyless form 0 MAC, tests/perf/c5t6-pin.py); reported apart, not gated
#   RESTART_REP / RESTART_AT: in that repetition, RESTART_AT s into the measured window, one clean daemon stop through the daemon's
#          own shutdown path (stop_lunixd of lib.sh: TERM = quiesce every device, exit) and a start that recovers the attached device
#          while fio keeps running (as refsoak cell 1). Reported apart, not gated.
# Every SAMPLE_S (2) s `lunix gc --status --json` and `lunix status --json` go to series.jsonl with the phase name; once per second
# `lunix status --json` is timed (probe.tsv). Per repetition the two JSONs are kept before and after (raw/rep<N>-<job>/), so the exact
# counters (cuts, cut_bytes_sum, cut_us_sum, propose_*, orphan_*, hold_*) are read as deltas. reps.tsv has the window of every phase.
# Environment: LUNIXD LUNIX TESTD LUNIX_SPIKE_DIR OUT CELL CHUNK; CFG_EXTRA (the arm's daemon settings), LOG_LEVEL.
here="$(cd "$(dirname "$0")" && pwd)"
: "${CELL:?}" "${CHUNK:?}" "${REPS:=5}" "${RAMP:=5}" "${W_RUNTIME:=60}" "${VOL_GIB:=16}" "${WS_GIB:=2}" "${JOBS:=8}" "${SAMPLE_S:=2}"
: "${DAEMON_CPUS:=0-7}" "${FIO_CPUS:=8-15}" "${QCPUS:=0-3}" "${VOLMODE:=integrity}" "${DESTAGE_WORKERS:=8}"
: "${RESTART_REP:=0}" "${RESTART_AT:=30}"
export DIST="${DIST:-random}" RWMIXREAD="${RWMIXREAD:-30}"   # pgracemix: the offset distribution and read share of w4kmix.fio
CFG_EXTRA="${CFG_EXTRA:+$CFG_EXTRA
}destage_workers = $DESTAGE_WORKERS"
export CFG_EXTRA JOURNAL_MAX="${JOURNAL_MAX:-1073741824}" DAEMON_CPUS FIO_CPUS
export SCRIPT_CAP="${SCRIPT_CAP:-14400}"
. "$here/lib.sh"; . "$here/perflib.sh"
harness_init "c5t6-$CELL"
JOBDIR="$REPO/bench/phase1/perf/fio"
say "# c5t6 block $CELL ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus): chunk $CHUNK, $REPS reps, restart rep $RESTART_REP"
say "- daemon settings: $(echo "$CFG_EXTRA" | tr '\n' ';'); log_level ${LOG_LEVEL:-info}"
NOISY=0; quiet_check; gov_set
df_guard "$LUNIX_SPIKE_DIR" "${DF_GIB:-1}" || exit 2
WS=$((WS_GIB * GIB)); SLICE=$((WS / JOBS)); VOL=c5t6
PHASEF="$OUT/phase"; echo start > "$PHASEF"; : > "$OUT/reps.tsv"
now_ms() { date +%s%3N; }
phase() { echo "$1" > "$PHASEF"; }

SPID=""; PPID_=""; SVD=""
sampler() {
  while sleep "$SAMPLE_S"; do
    local t g s ph a b
    t=$(now_ms); ph=$(cat "$PHASEF" 2>/dev/null)
    a=$(date +%s%N); g=$(LX_TIMEOUT=20 lx gc "$VOL" --status --json 2>/dev/null); b=$(date +%s%N)
    s=$(LX_TIMEOUT=20 lx status "$VOL" --json 2>/dev/null | jq -c '{write_path,throttle,space,journal_used,stats:{commits:.stats.commits}}' 2>/dev/null)
    # c5def: the sealed segment files that exist at this sample (listing of the segment directory)
    [ -n "$SVD" ] || SVD=$(vol_dir "$VOL" 2>/dev/null)
    ns=$(ls "$SVD/segments" 2>/dev/null | grep -c '\.seg$')
    jq -c -n --arg t "$t" --arg ph "$ph" --arg ms "$(((b - a) / 1000000))" --arg ns "$ns" --argjson gc "${g:-null}" --argjson st "${s:-null}" \
      '{t:($t|tonumber),phase:$ph,gc_call_ms:($ms|tonumber),nseg:($ns|tonumber),gc:$gc,status:$st}' >> "$OUT/series.jsonl" 2>/dev/null
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
  read -r l1 l5 _ < /proc/loadavg
  local rd="$OUT/raw/$label"; mkdir -p "$rd"
  lx gc "$VOL" --status --json > "$rd/gc-before.json" 2>/dev/null; status_json "$VOL" > "$rd/status-before.json" 2>/dev/null
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
  lx gc "$VOL" --status --json > "$rd/gc-end.json" 2>/dev/null; status_json "$VOL" > "$rd/status-end.json" 2>/dev/null
  local jf="$OUT/raw/fio-${label//\//_}.json"
  [ "$frc" = 0 ] && [ "$(fio_errors "$jf")" = 0 ]; rc2=$?
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$rep" "$label" "$t0" "$t1" "$l1" "$l5" "$([ $rc2 = 0 ] && echo ok || echo FIO_ERROR)" >> "$OUT/reps.tsv"
  say "- $label: fio rc $frc, errors $(fio_errors "$jf"), $(fio_summary "$jf" | awk '{printf "%d IOPS, p99 %.0f us", $1, $4}')"
  [ "$rc2" = 0 ] || FAIL=1
}

# ---- the block ----------------------------------------------------------------------------------------------
cell_reset; start_lunixd || wedged "lunixd did not start"
# shipcheck: the daemon settings of the block (no paths), for the assertion of the effective config
grep -v -E '^(state_dir|socket|dev_dir)' "$ST/lunixd.toml" > "$OUT/lunixd-settings.toml" 2>/dev/null
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
if [ "$CELL" = pin ]; then
  # a snapshot pin on the tip commit, held through the block: wait for a cut to name the filled data, then seal and write the pin
  sleep 12
  seq=$(status_json "$VOL" | jq -r '.last_commit_seq'); uuid=$(status_json "$VOL" | jq -r '.uuid'); vd=$(vol_dir "$VOL")
  python3 "$here/c5t6-pin.py" "$vd" "$uuid" "$seq" pin1; expect "snapshot pin on commit $seq written" $? || { FAIL=1; }
  cp "$vd/snapshots/pin1" "$OUT/raw/pin1.bin" 2>/dev/null
  echo "$seq" > "$OUT/raw/pin-seq.txt"
fi

for rep in $(seq 1 "$REPS"); do
  rs=0; [ "$rep" = "$RESTART_REP" ] && rs=1
  case "$CELL" in
    w4k|pin) run_fio "rep$rep-w4k" w4k "$W_RUNTIME" 128 "$rep" "$rs" ;;
    w4kz) run_fio "rep$rep-w4kz" w4kz "$W_RUNTIME" 128 "$rep" "$rs" ;;
    w4kmix) run_fio "rep$rep-w4kmix" w4kmix "$W_RUNTIME" 128 "$rep" "$rs" ;;
    *) echo "unknown cell $CELL" >&2; exit 2 ;;
  esac
  # c5def: the pin cell releases its pin after repetition PIN_RELEASE_AFTER (0 = held through the block)
  if [ "$CELL" = pin ] && [ "${PIN_RELEASE_AFTER:-0}" = "$rep" ]; then
    rm -f "$vd/snapshots/pin1"; now_ms > "$OUT/raw/pin-released-ms.txt"; say "- snapshot pin removed after repetition $rep"
  fi
done
# pgracet6: fio read-back verification while the collector is still working off the backlog of the last repetition (V_RUNTIME s, 0 = skip)
if [ "${V_RUNTIME:-30}" != 0 ]; then
  phase verify; vrd="$OUT/raw/verify"; mkdir -p "$vrd"
  lx gc "$VOL" --status --json > "$vrd/gc-before.json" 2>/dev/null; status_json "$VOL" > "$vrd/status-before.json" 2>/dev/null
  export DEV="$CUR_DEV" WS SLICE RUNTIME="${V_RUNTIME:-30}" CPUS="$FIO_CPUS" JOBS QD=32 SEED=900
  FIO_GRACE=300 fio_bg "verify-w4k" "${V_RUNTIME:-30}" "$JOBDIR/w4kv.fio"; vfp=$LAST_FIO
  wait_fio "$vfp" $((${V_RUNTIME:-30} + 600)); vrc=$?
  lx gc "$VOL" --status --json > "$vrd/gc-end.json" 2>/dev/null; status_json "$VOL" > "$vrd/status-end.json" 2>/dev/null
  vjf="$OUT/raw/fio-verify-w4k.json"; verr=$(fio_errors "$vjf")
  printf 'rc=%s job_errors=%s\n' "$vrc" "$verr" > "$OUT/verify.txt"
  say "- verify-w4k: fio rc $vrc, errors $verr, $(fio_summary "$vjf" | awk '{printf "%d IOPS", $1}')"
  [ "$vrc" = 0 ] && [ "$verr" = 0 ]; expect "fio verify clean (rc $vrc, errors $verr)" $? || FAIL=1
fi
phase end
sleep "$((SAMPLE_S * 2))"
kill "$SPID" "$PPID_" 2>/dev/null; wait "$SPID" "$PPID_" 2>/dev/null
lx gc "$VOL" --status --json > "$OUT/raw/gc-final.json" 2>/dev/null
lx gc "$VOL" --status > "$OUT/raw/gc-final.txt" 2>/dev/null
status_json "$VOL" > "$OUT/raw/status-final.json" 2>/dev/null
detach_vol "$VOL"; expect "detach" $? || FAIL=1
lunix_fsck "$VOL" c5t6-"$CELL"; expect "lunix fsck --deep clean (exit $FSCK_RC)" $? || FAIL=1
cp "$(vol_dir "$VOL" 2>/dev/null || echo /nonexistent)/volume.json" "$OUT/raw/" 2>/dev/null
stop_lunixd || FAIL=1
say ""; say "reference table log lines: $(cat "$OUT"/lunixd.*.log | grep -c 'reference table')"
say "EIO/ENOSPC log lines: $(cat "$OUT"/lunixd.*.log | grep -c -E 'EIO|ENOSPC|No space left|Input/output error')"
say "overall FAIL=$FAIL (noisy host: $NOISY)"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
