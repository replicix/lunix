#!/usr/bin/env bash
# M17.B measurements (run on the test host from the repository root, through
# tools/remote.sh run or a detached shell there; never on a workstation).
#
#   bash bench/phase1/gc/run.sh churn   # GC on the stack: store vs live data
#   bash bench/phase1/gc/run.sh mark    # N25: mark cost from 1 M to 64 M chunks
#
# Output goes to bench/phase1/gc/raw/ (one log per cell); RESULTS.md holds
# the tables. CPUs: LUNIX_CPUS (default 0-7).
set -euo pipefail
cd "$(dirname "$0")/../../.."
raw=bench/phase1/gc/raw
mkdir -p "$raw"
cpus="${LUNIX_CPUS:-0-7}"
jobs="${LUNIX_JOBS:-8}"
export CARGO_INCREMENTAL=0

host_info() {
  {
    echo "date: $(date -u +%Y-%m-%dT%H:%MZ)"
    echo "kernel: $(uname -r | cut -d- -f1)"
    echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //'), $(nproc) vCPUs"
    echo "memory: $(awk '/MemTotal/ {printf "%.0f GiB", $2/1048576}' /proc/meminfo)"
    echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)"
    echo "rustc: $(rustc --version)"
    echo "cpus used: $cpus"
  } > "$raw/host.txt"
}

churn() {
  cargo build -j "$jobs" --release -p lunix-fsck
  local mib="${LUNIX_GC_CHURN_MIB:-256}" rounds="${LUNIX_GC_CHURN_ROUNDS:-10}"
  LUNIX_STACK_REQUIRE_FSCK=1 LUNIX_GC_CHURN_MIB="$mib" LUNIX_GC_CHURN_ROUNDS="$rounds" \
    taskset -c "$cpus" cargo test -j "$jobs" --release -p lunixd --features testsupport \
    --test stack_gc churn_ -- --nocapture --test-threads=2 2>&1 | tee "$raw/churn-${mib}mib.log"
}

mark() {
  cargo test -j "$jobs" --release -p lunix-gc --test mark_cost --no-run
  for n in 1048576 4194304 16777216 67108864; do
    LUNIX_GC_MARK_CHUNKS=$n /usr/bin/time -v taskset -c "$cpus" \
      cargo test -j "$jobs" --release -p lunix-gc --test mark_cost \
      -- --ignored --nocapture > "$raw/mark-$n.log" 2>&1 || true
    grep -E 'mark cost:|Maximum resident|Elapsed|test result|panicked' "$raw/mark-$n.log" || true
  done
}

host_info
case "${1:-}" in
  churn) churn ;;
  mark) mark ;;
  *) echo "usage: $0 churn|mark" >&2; exit 2 ;;
esac
