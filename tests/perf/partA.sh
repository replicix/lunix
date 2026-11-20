#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M16 Part A: the DESIGN §20 targets on the raw device of a local volume, QUIET HOST ONLY.
#
# Per (chunk size, queue count): a fresh 32 GiB volume (journal_max 1 GiB) attached with QUEUES in {4, 8}, queue
# threads pinned to QCPUS_<n>, the daemon process to DAEMON_CPUS, fio to FIO_CPUS (disjoint sets); cells, each REPS (3)
# times, median and min/max in the analysis:
#   r4k   4k randread, QD 128, 8 jobs, 4 GiB working set written first (30 s + 5 s ramp)
#   w4k   4k randwrite, QD 128, 8 jobs (60 s)
#   seqw  1M write, QD 32, 8 jobs, 50 % compressible data (30 s)
#   seqr  1M read of what seqw wrote, QD 32, 8 jobs
# then once each of r4k and seqr with verify_reads = true (volume.json of the detached volume; the cost, Spec gaps 10).
# Per cell: fio JSON (kept), daemon CPU seconds (utime+stime of the process, before/after the measured window) and the
# thread split, CPU microseconds per I/O, resident set (idle after attach, loaded after the cell, high-water mark),
# journal use and the status JSON at the end of write cells and again 10 s later (destage backlog). For the first
# repetition of each cell `perf stat` (and `perf record` when PERF=1) of the daemon runs next to fio.
# Same session references: null_blk with the same cells, and the journal file system's own raw fio limit.
# Every fio run is under an outer `timeout -k` of 1.5 x its runtime. Gate: each row meets its DESIGN §20 target or the
# report explains the shortfall with this evidence; no threshold is changed (bench/phase1/perf/analyze.py applies them).
#
# Runtime: per (chunk, queues): fill 1 min + 3 x (35 + 65 + 35 + 35) s + verify_reads cells + attach/detach about 20 min;
# two chunk sizes x two queue counts about 80 min, references 5 min. Disk: the 4 GiB working set and 8 GiB of seqw slices
# rewritten in place, GC on: the store stays below 25 GiB; df-guard asks for 30 before the volume.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=... LUNIX=... TESTD=... \
#       bash tests/perf/partA.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${CHUNKS:=65536 16384}" "${QUEUES_LIST:=4 8}" "${REPS:=3}" "${RUNTIME:=30}" "${RAMP:=5}" "${W_RUNTIME:=60}" "${WS_GIB:=4}" "${VOL_GIB:=32}"
: "${DAEMON_CPUS:=0-7}" "${FIO_CPUS:=8-15}" "${QCPUS_4:=0-3}" "${QCPUS_8:=0-7}" "${VERIFY_READS:=1}" "${PERF:=1}" "${NULLBLK:=1}"
: "${JOBS:=8}" "${VOLMODE:=integrity}"
# Destage pool worker threads (lunixd.toml `destage_workers`, 1 to 64; the plan's "8 workers" of the seqw cell). A daemon
# setting read at start: it holds for every cell of the run (recorded per cell). DESTAGE_WORKERS=default leaves the
# daemon's own choice, max(2, cpus / 2) of the CPUs the daemon may run on.
: "${DESTAGE_WORKERS:=8}"
if [ "$DESTAGE_WORKERS" != default ]; then CFG_EXTRA="${CFG_EXTRA:+$CFG_EXTRA
}destage_workers = $DESTAGE_WORKERS"; fi
export CFG_EXTRA
export JOURNAL_MAX="${JOURNAL_MAX:-1073741824}" DAEMON_CPUS FIO_CPUS
read -ra _c <<< "$CHUNKS"; read -ra _q <<< "$QUEUES_LIST"
export SCRIPT_CAP="${SCRIPT_CAP:-$((${#_c[@]} * ${#_q[@]} * 3600 + 3600))}"
. "$here/lib.sh"; . "$here/perflib.sh"
harness_init m16-parta
JOBDIR="$REPO/bench/phase1/perf/fio"
say "# M16 Part A ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus): chunks $CHUNKS, queues $QUEUES_LIST, $REPS reps"
say "- destage_workers: $DESTAGE_WORKERS (lunixd.toml; seqw of the plan runs on 8 workers); read_cache_max left at the volume default (recorded per cell)"
say "- daemon CPUs $DAEMON_CPUS (process), queue threads QCPUS_4=$QCPUS_4 / QCPUS_8=$QCPUS_8, fio CPUS $FIO_CPUS; script cap $SCRIPT_CAP s"
NOISY=0; quiet_check; gov_set
WS=$((WS_GIB * GIB)); SLICE=$((WS / JOBS))
df_guard "$LUNIX_SPIKE_DIR" "${DF_GIB:-30}" || exit 2

# fio_cell CELL JOB REPLABEL RUNTIME QD [extra env]: runs the cell with CPU accounting, writes one record
CELLNO=0
fio_cell() { # chunkk q name job rep runtime qd
  local ck=$1 q=$2 name=$3 job=$4 rep=$5 rt=$6 qd=$7
  local cell="partA/c${ck}k/q$q/$name/rep$rep"; local d; d=$(cell_dir "$cell"); CELLNO=$((CELLNO + 1))
  local RUNTIME=$rt   # the job files read RUNTIME; the global default is not changed
  export DEV="$CUR_DEV" WS SLICE RUNTIME RAMP CPUS="$FIO_CPUS" JOBS QD=$qd SEED=$((100 + CELLNO)) LATLOG="$d/lat"
  local grace=$(((rt + RAMP) / 2 + RAMP))
  read -r l1 _ < /proc/loadavg
  snap "$VOL" "$d/status-before.json"
  local rss0; rss0=$(rss_kb "$DPID")
  FIO_GRACE=$grace fio_bg "$cell" "$rt" "$JOBDIR/$job.fio"; local fp=$LAST_FIO
  sleep "$((RAMP + 1))"
  local c0 t0; c0=$(cpu_secs "$DPID"); thread_cpu "$DPID" > "$d/threads-0.txt"; t0=$SECONDS
  local pp=""
  if [ "$rep" = 1 ] && command -v perf >/dev/null && [ "$PERF" = 1 ]; then
    perf stat -p "$DPID" -e task-clock,context-switches,cpu-migrations,cycles,instructions -o "$d/perf-stat.txt" -- sleep $((rt / 3)) >/dev/null 2>&1 &
    pp=$!
    perf record -F 99 -g -p "$DPID" -o "$d/perf.data" -- sleep $((rt / 3)) >/dev/null 2>&1 &
    pp="$pp $!"
  fi
  # SAMPLE=1: every SAMPLE_S (5) s during the cell the status JSON, the collector status and the daemon's threads (name, state,
  # kernel wait channel) go to series.jsonl, to find where a stalled write waits; stopped with the cell
  local sp="" st0=$SECONDS pp2=""
  # STALL_PROFILE=1 (first repetition): kernel stacks of the daemon's threads that sit in uninterruptible sleep, every 0.5 s for
  # 20 s, and an off-CPU `perf record` (sched_switch with user call graphs, dwarf) for 6 s: where the blocked threads wait
  if [ "${STALL_PROFILE:-0}" = 1 ] && [ "$rep" = 1 ]; then
    ( sleep "$((RAMP + 3))"; for _i in $(seq 1 40); do
        for t in /proc/"$DPID"/task/*; do
          if [ "$(awk '{print $3}' "$t/stat" 2>/dev/null)" = D ]; then printf '== %s %s\n' "$(cat "$t/comm" 2>/dev/null)" "$(cat "$t/stack" 2>/dev/null | head -12 | tr '\n' '|')"; fi
        done; sleep 0.5; done > "$d/dstacks.txt" 2>/dev/null ) & pp2=$!
    ( sleep "$((RAMP + 5))"; perf record -e sched:sched_switch --call-graph dwarf,8192 -p "$DPID" -o "$d/offcpu.data" -- sleep 6 >/dev/null 2>&1
      perf script -i "$d/offcpu.data" -F comm,tid,time,ip,sym --no-inline 2>/dev/null | head -40000 > "$d/offcpu-script.txt"; perf report --stdio --no-children -i "$d/offcpu.data" --sort comm,sym -g caller,0.5,callee --max-stack 12 2>/dev/null | head -150 > "$d/offcpu-report.txt"; rm -f "$d/offcpu.data" ) & pp2="$pp2 $!"
  fi
  # STATUS_PROBE=1 (ADR-33 T6): `lunix status NAME --json` once per second during the cell, with its wall time in ms (probe.tsv:
  # seconds into the cell, ms, exit code); the status must stay fast under load (ADR-33 gate: milliseconds)
  local pr=""
  if [ "${STATUS_PROBE:-0}" = 1 ]; then
    ( while sleep 1; do
        a=$(date +%s%N); lx status "$VOL" --json >/dev/null 2>&1; rcp=$?; b=$(date +%s%N)
        printf '%s\t%s\t%s\n' "$((SECONDS - st0))" "$(((b - a) / 1000000))" "$rcp" >> "$d/probe.tsv"
      done ) & pr=$!
  fi
  # SHARES=1 (first repetition of w4k and seqw): where the wall time of the daemon's threads goes, for the ADR-33 T6 shares:
  # an on-CPU `perf record` (cpu-clock, dwarf call graphs, 8 s) and an off-CPU one (sched_switch of the daemon's threads, all
  # CPUs so that the switch-in is seen too, dwarf, 6 s), both raw scripts
  # kept as compressed text for tests/perf/shares.py; a per-thread state sample (every 0.2 s for 10 s: comm, state, wchan)
  local pp3=""
  if [ "${SHARES:-0}" = 1 ] && [ "$rep" = 1 ] && [ "$name" != r4k ] && [ "$name" != seqr ]; then
    ( sleep "$((RAMP + 4))"
      perf record -e cpu-clock -F 499 --call-graph dwarf,16384 -p "$DPID" -o "$d/oncpu.data" -- sleep 8 >/dev/null 2>&1
      perf script -i "$d/oncpu.data" -F comm,tid,time,ip,sym --no-inline 2>/dev/null | gzip -1 > "$d/oncpu-script.txt.gz"; rm -f "$d/oncpu.data" ) & pp3=$!
    ( sleep "$((RAMP + 14))"
      perf record -a -e sched:sched_switch --filter 'prev_comm ~ "lunix*" || next_comm ~ "lunix*"' --call-graph dwarf,16384 -o "$d/offcpu2.data" -- sleep 6 >/dev/null 2>&1
      perf script -i "$d/offcpu2.data" -F comm,tid,time,trace,ip,sym --no-inline 2>/dev/null | gzip -1 > "$d/offcpu-script.txt.gz"; rm -f "$d/offcpu2.data" ) & pp3="$pp3 $!"
    ( sleep "$((RAMP + 22))"; for _i in $(seq 1 100); do
        for t in /proc/"$DPID"/task/*; do printf '%s %s %s %s\n' "$_i" "$(cat "$t/comm" 2>/dev/null | tr ' ' _)" "$(awk '{print $3}' "$t/stat" 2>/dev/null)" "$(cat "$t/wchan" 2>/dev/null)"; done
        sleep 0.2; done > "$d/thread-states.txt" 2>/dev/null ) & pp3="$pp3 $!"
  fi
  if [ "${SAMPLE:-0}" = 1 ]; then
    ( while sleep "${SAMPLE_S:-5}"; do
        ths=$(for t in /proc/"$DPID"/task/*; do printf '%s:%s:%s\n' "$(cat "$t/comm" 2>/dev/null)" "$(awk '{print $3}' "$t/stat" 2>/dev/null)" "$(cat "$t/wchan" 2>/dev/null)"; done | sort | uniq -c | jq -R -s -c 'split("\n")|map(select(length>0))')
        jq -c -n --argjson st "$(status_json "$VOL" 2>/dev/null || echo null)" --argjson gc "$(lx gc "$VOL" --status --json 2>/dev/null || echo null)" --argjson th "${ths:-null}" --arg t "$((SECONDS - st0))" '{t:$t,status:$st,gc:$gc,threads:$th}' >> "$d/series.jsonl" 2>/dev/null
      done ) & sp=$!
  fi
  wait_fio "$fp" $((rt + grace + 120)); local frc=$?
  [ -n "$sp" ] && { kill "$sp" 2>/dev/null; wait "$sp" 2>/dev/null; }
  [ -n "$pr" ] && { kill "$pr" 2>/dev/null; wait "$pr" 2>/dev/null; }
  # shellcheck disable=SC2086
  [ -n "$pp3" ] && wait $pp3 2>/dev/null
  # shellcheck disable=SC2086
  [ -n "$pp2" ] && wait $pp2 2>/dev/null
  local c1 w; c1=$(cpu_secs "$DPID"); w=$((SECONDS - t0)); [ "$w" -ge 1 ] || w=1
  thread_cpu "$DPID" > "$d/threads-1.txt"
  # shellcheck disable=SC2086
  [ -n "$pp" ] && wait $pp 2>/dev/null
  [ -f "$d/perf.data" ] && { perf report --stdio --no-children -i "$d/perf.data" 2>/dev/null | head -80 > "$d/perf-report.txt"; rm -f "$d/perf.data"; }
  local rss1 hwm; rss1=$(rss_kb "$DPID"); hwm=$(hwm_kb "$DPID")
  snap "$VOL" "$d/status-end.json"
  local jf="$OUT/raw/fio-${cell//\//_}.json"
  local iops bw p50 p99 p999 ios; read -r iops bw p50 p99 p999 ios <<< "$(fio_summary "$jf")"
  local cpu_s cpu_us; cpu_s=$(awk -v a="$c0" -v b="$c1" 'BEGIN{printf "%.3f", b-a}')
  cpu_us=$(awk -v c="$cpu_s" -v i="${iops:-0}" -v w="$w" 'BEGIN{ if (i>0) printf "%.2f", c*1e6/(i*w); else print "" }')
  local split; split=$(awk 'NR==FNR{a[$1]=$2; next} {d=$2-a[$1]; if (d>0.05) printf "%s=%.2f(%d) ", $1, d, $3}' "$d/threads-0.txt" "$d/threads-1.txt")
  local ju0 ju1=""; ju0=$(jq -r '.journal_used // ""' "$d/status-end.json" 2>/dev/null)
  case "$name" in w4k|seqw) sleep 10; snap "$VOL" "$d/status-idle10.json"; ju1=$(jq -r '.journal_used // ""' "$d/status-idle10.json" 2>/dev/null) ;; esac
  local rcm cache_b mapb; rcm=$(jq -r '.read_cache_max // ""' "$(vol_dir "$VOL")/volume.json" 2>/dev/null)
  cache_b=$(jq -r '.space.cache // ""' "$d/status-end.json" 2>/dev/null); mapb=$(jq -r '.stats.map_bytes_written // ""' "$d/status-end.json" 2>/dev/null)
  # ADR-33 write-path counters at the end of the cell (status-end.json), and the destage stage times over the cell
  local wp stg; wp=$(jq -r '.write_path // {} | [.ring_waiters, .ring_wait_longest_ms, .window_resident_bytes, .window_hits, .window_misses, .last_cut_bytes, .last_cut_ms] | map(. // "") | join(",")' "$d/status-end.json" 2>/dev/null)
  stg=$(jq -r -n --slurpfile a "$d/status-before.json" --slurpfile b "$d/status-end.json" '[range(0;5)] | map(($b[0].stats.stage_ns[.] // 0) - ($a[0].stats.stage_ns[.] // 0)) | join(",")' 2>/dev/null)
  local cm; cm=$(jq -r -n --slurpfile a "$d/status-before.json" --slurpfile b "$d/status-end.json" '($b[0].stats.commits // 0) - ($a[0].stats.commits // 0)' 2>/dev/null)
  local pmax; pmax=$(sort -k2 -nr "$d/probe.tsv" 2>/dev/null | head -1 | cut -f2); local pn; pn=$(wc -l < "$d/probe.tsv" 2>/dev/null)
  [ "$frc" = 0 ] && [ "$(fio_errors "$jf")" = 0 ]; local ok=$?
  say "- $cell: ${iops:-?} IOPS, $(awk -v b="${bw:-0}" 'BEGIN{printf "%.2f", b/1e9}') GB/s, p99 ${p99:-?} us, daemon CPU ${cpu_s}s (${cpu_us:-?} us/IO), rss ${rss1} KiB, split $split"
  rec "$cell" "$([ $ok = 0 ] && echo info || echo fail)" chunk_k="$ck" queues="$q" cell_name="$name" rep="$rep" runtime="$rt" iops="${iops:-}" bw_bytes="${bw:-}" \
      p50_us="${p50:-}" p99_us="${p99:-}" p999_us="${p999:-}" ios="${ios:-}" daemon_cpu_s="$cpu_s" window_s="$w" cpu_us_per_io="${cpu_us:-}" rss_kib_before="$rss0" \
      rss_kib_after="$rss1" hwm_kib="$hwm" journal_used_end="${ju0:-}" journal_used_idle10="${ju1:-}" thread_split="$split" load1="$l1" destage_workers="$DESTAGE_WORKERS" read_cache_max="${rcm:-}" cache_bytes_end="${cache_b:-}" map_bytes_written_end="${mapb:-}" fio_rc="$frc" noisy_host="$NOISY" write_path_end="${wp:-}" stage_ns_delta="${stg:-}" commits_delta="${cm:-}" status_probe_max_ms="${pmax:-}" status_probe_n="${pn:-}" journal_max="$JOURNAL_MAX" ws_gib="$WS_GIB"
  [ "$ok" = 0 ] || FAIL=1
}

# ---- same-session references -----------------------------------------------------------------------------------
if [ "$NULLBLK" = 1 ]; then
  say ""; say "## references"
  if [ -d /sys/module/null_blk ]; then say "- null_blk already loaded by someone else: not touched, reference skipped"
  elif modprobe null_blk nr_devices=1 gb=16 bs=4096 memory_backed=1 queue_mode=2 submit_queues=8 completion_nsec=0 irqmode=0 2>/dev/null; then
    add_cleanup 'rmmod null_blk'; register_device /dev/nullb0
    DEV=/dev/nullb0 RUNTIME=$RUNTIME CPUS=$FIO_CPUS FIO_GRACE=$((RUNTIME * 4)) fio_run nullblk-ref $((RUNTIME * 4)) "$JOBDIR/nullblk-ref.fio"
    rec "ref/nullblk" info file="raw/fio-nullblk-ref.json"; say "- null_blk reference: raw/fio-nullblk-ref.json"
  else say "- null_blk not available: reference skipped"; fi
  JD="$VOLS/journal-ref"; mkdir -p "$JD"; need_safe "$JD"
  DIR="$JD" SIZE=4g RUNTIME=$RUNTIME CPUS=$FIO_CPUS FIO_GRACE=$((RUNTIME * 6)) fio_run journal-raw $((RUNTIME * 3)) "$JOBDIR/journal-raw.fio"
  rec "ref/journal-fs" info file="raw/fio-journal-raw.json"; say "- journal file system raw limits: raw/fio-journal-raw.json"
  rm -rf "$JD"
fi

# ---- the cells --------------------------------------------------------------------------------------------------
for chunk in "${_c[@]}"; do
  ck=$((chunk / 1024)); VOL="pa${ck}k"; CUR_VOL=$VOL; CUR_MODE=$VOLMODE
  say ""; say "## chunk ${ck} KiB"
  cell_reset; start_lunixd || wedged "lunixd did not start"
  mkvol "$VOL" $((VOL_GIB * GIB)) "$chunk" "$VOLMODE"; expect "create $VOL" $? || exit 1
  # READ_CACHE_BYTES: a supplementary run with a read cache that holds the working set (volume.json read_cache_max; the
  # default 128 MiB does not, so the plan's "cache hits" row reads from the store); unset = the volume default
  if [ -n "${READ_CACHE_BYTES:-}" ]; then set_vj "$VOL" read_cache_max "$READ_CACHE_BYTES"; expect "read_cache_max = $READ_CACHE_BYTES" $? || exit 1; fi
  for q in "${_q[@]}"; do
    qc_var="QCPUS_$q"; qc=${!qc_var:-$QCPUS_8}
    say ""; say "### chunk ${ck} KiB, $q queues (queue CPUs $qc)"
    attach_pinned "$VOL" "$q" "$qc"; expect "attach with $q queues" $? || exit 1
    CUR_DEV=$(dev_of "$VOL"); register_id "${CUR_DEV#/dev/ublkb}"; [ -b "$CUR_DEV" ] || wedged "no device"
    sleep 5; IDLE_RSS=$(rss_kb "$DPID"); say "- idle resident set after attach: $IDLE_RSS KiB"
    rec "partA/c${ck}k/q$q/idle" info rss_kib="$IDLE_RSS" queues="$q" chunk_k="$ck"
    export DEV="$CUR_DEV" WS RUNTIME=$RUNTIME RAMP CPUS="$FIO_CPUS" JOBS QD=32 SEED=1
    FIO_GRACE=600 fio_run "fill-c${ck}-q$q" "$((WS_GIB * 60))" "$JOBDIR/fill.fio"; expect "write the ${WS_GIB} GiB working set" $? || FAIL=1
    if [ "${SKIP_READ_CELLS:-0}" != 1 ]; then for rep in $(seq 1 "$REPS"); do fio_cell "$ck" "$q" r4k r4k "$rep" "$RUNTIME" 128; done; fi
    if [ "${SKIP_WRITE_CELLS:-0}" != 1 ]; then   # the supplementary read-cache run skips the journal-bound write cells
      for rep in $(seq 1 "$REPS"); do fio_cell "$ck" "$q" w4k w4k "$rep" "$W_RUNTIME" 128; done
      for rep in $(seq 1 "$REPS"); do fio_cell "$ck" "$q" seqw seqw "$rep" "$RUNTIME" 32; done
    fi
    if [ "${SKIP_READ_CELLS:-0}" != 1 ]; then for rep in $(seq 1 "$REPS"); do fio_cell "$ck" "$q" seqr seqr "$rep" "$RUNTIME" 32; done; fi
    if [ "$VERIFY_READS" = 1 ]; then
      detach_vol "$VOL"; expect "detach for verify_reads" $? || exit 1
      set_vj "$VOL" verify_reads true; expect "volume.json verify_reads = true" $? || { FAIL=1; }
      attach_pinned "$VOL" "$q" "$qc"; CUR_DEV=$(dev_of "$VOL"); register_id "${CUR_DEV#/dev/ublkb}"; sleep 3
      fio_cell "$ck" "$q" r4k-verifyreads r4k 1 "$RUNTIME" 128
      fio_cell "$ck" "$q" seqr-verifyreads seqr 1 "$RUNTIME" 32
      detach_vol "$VOL"; set_vj "$VOL" verify_reads false
    else detach_vol "$VOL"; fi
    lunix_fsck "$VOL" "parta-c${ck}k-q$q"; expect "lunix fsck --deep clean after q$q (exit $FSCK_RC)" $? || FAIL=1
  done
  stop_lunixd
done
say ""; say "overall FAIL=$FAIL (noisy host: $NOISY)"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
