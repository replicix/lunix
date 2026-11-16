#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# Measurement helpers of the M16 scripts (source after lib.sh and harness_init).
#
# CPU accounting follows CONVENTIONS "Benchmark hygiene": the daemon's CPU seconds are utime+stime of /proc/<pid>/stat
# (the whole thread group: queue threads, journal, workers), sampled before and after a cell; the thread table groups
# the threads by their comm name so the split between queue threads and the rest is visible.
CLK_TCK=$(getconf CLK_TCK)

# quiet_check: performance numbers are taken only on a quiet host (plan: "quiet host only"). Refuses unless
# LUNIX_HOST_QUIET=1 says the lead checked; records the load average at the start; FORCE_NOISY=1 runs anyway and the
# report must say so.
quiet_check() {
  local l1 l5; read -r l1 l5 _ < /proc/loadavg
  say "- load average at the start: $l1 $l5 (1 and 5 min), $(nproc) cpus"
  if [ "${LUNIX_HOST_QUIET:-}" != 1 ]; then
    if [ "${FORCE_NOISY:-}" = 1 ]; then say "- WARNING: LUNIX_HOST_QUIET is not set; running anyway (FORCE_NOISY=1); the numbers are noisy by definition"; NOISY=1
    else echo "refusing: performance cells run only on a quiet host (the lead sets LUNIX_HOST_QUIET=1 after checking); FORCE_NOISY=1 overrides for a dry run" >&2; exit 77; fi
  fi
  [ "$(awk -v l="$l5" 'BEGIN{print (l > 2.0)}')" = 1 ] && { say "- WARNING: 5-minute load $l5 is above 2: the host is not quiet"; NOISY=1; }
  return 0
}
NOISY=${NOISY:-0}
# governor: set `performance` on every CPU that has cpufreq and restore the previous values at cleanup
GOV_SAVED=()
gov_set() {
  local f
  for f in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    [ -w "$f" ] || continue
    GOV_SAVED+=("$f=$(cat "$f")"); echo performance > "$f"
  done
  if [ ${#GOV_SAVED[@]} = 0 ]; then say "- governor: not exposed (no cpufreq on this machine), nothing set"; else say "- governor: performance on ${#GOV_SAVED[@]} cpus (restored at the end)"; add_cleanup gov_restore; fi
}
gov_restore() { local kv; for kv in "${GOV_SAVED[@]}"; do echo "${kv#*=}" > "${kv%%=*}"; done; GOV_SAVED=(); }

# cpu_secs PID: utime+stime of the process (all threads) in seconds
cpu_secs() { awk -v t="$CLK_TCK" '{ s=$0; sub(/^.*\) /, "", s); n=split(s, a, " "); printf "%.3f\n", (a[12] + a[13]) / t }' "/proc/$1/stat"; }
# thread_cpu PID: "comm seconds count" per thread name, sorted by seconds
thread_cpu() {
  local t c
  for t in /proc/"$1"/task/*; do
    c=$(tr -d '\n' < "$t/comm" 2>/dev/null | tr ' ' '_'); [ -n "$c" ] || continue
    awk -v c="$c" -v clk="$CLK_TCK" '{ s=$0; sub(/^.*\) /, "", s); split(s, a, " "); printf "%s %.3f\n", c, (a[12] + a[13]) / clk }' "$t/stat" 2>/dev/null
  done | awk '{ s[$1] += $2; n[$1]++ } END { for (k in s) printf "%s %.3f %d\n", k, s[k], n[k] }' | sort -k2 -nr
}
rss_kb() { awk '/^VmRSS:/{print $2}' "/proc/$1/status"; }
hwm_kb() { awk '/^VmHWM:/{print $2}' "/proc/$1/status"; }
# fio_val FILE jq-expression: a number out of a fio JSON report (warnings in front are skipped)
fio_val() { fio_json "$1" | jq -r "$2" 2>/dev/null; }
# fio_summary FILE: iops, bw (bytes/s), clat percentiles (us) of the aggregated job: "iops bw p50 p99 p999 total_ios"
fio_summary() {
  fio_json "$1" | jq -r '.jobs[0] as $j |
    ([$j.read, $j.write] | map(.iops // 0) | add) as $iops |
    ([$j.read, $j.write] | map(.bw_bytes // 0) | add) as $bw |
    ([$j.read, $j.write] | map(select((.total_ios // 0) > 0)) | .[0]) as $d |
    ($d.clat_ns.percentile // {}) as $p |
    [$iops, $bw, (($p["50.000000"] // 0) / 1000), (($p["99.000000"] // 0) / 1000), (($p["99.900000"] // 0) / 1000),
     ([$j.read, $j.write] | map(.total_ios // 0) | add)] | @tsv' 2>/dev/null
}
# snap NAME FILE: the volume's status JSON (counters, space ledger, journal use)
snap() { status_json "$1" > "$2" 2>/dev/null; }
# attach_pinned NAME QUEUES CPULIST: attach with the queue threads on their CPUs
attach_pinned() {
  # shellcheck disable=SC2046
  lx_logged "attach-$1" attach "$1" --queues "$2" --cpus "$3" $(key_opts "$CUR_MODE")
}
