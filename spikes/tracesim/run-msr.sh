#!/bin/sh
# MSR Cambridge only, one process per volume file (bounded RSS), up to $JOBS in parallel.
# Usage: run-msr.sh TRACE_DIR OUT_DIR [JOBS]
set -eu
T=${1:?trace dir}; O=${2:?out dir}; J=${3:-3}
B=$(cd "$(dirname "$0")" && pwd)/target/release/tracesim
mkdir -p "$O"
# Scan-test volumes: 10 evenly spaced ranks (0,3,6,..,27) of the 36 files by compressed size, largest first.
SCAN=$(ls -S "$T"/msr/*.csv.gz | awk 'NR%3==1 && NR<=28' | xargs -n1 basename | sed 's/\.csv\.gz//' | paste -sd, -)
echo "scan volumes: $SCAN" | tee "$O/scan-volumes.txt"
ls -S "$T"/msr/*.csv.gz | xargs -P "$J" -I{} sh -c '
  v=$(basename {} .csv.gz)
  s=$(date +%s)
  taskset -c 8-15 timeout -k 30 14400 "$0" all --set msr --tag "$v" --fmt msr --out "$1" --chunk 65536,16384 --scan-list "$2" {} 2> "$1/log.$v.txt"
  echo "$v $(( $(date +%s) - s )) s" >> "$1/timing.txt"
' "$B" "$O" "$SCAN"
"$B" gate "$O" | tee "$O/gate.txt"
