#!/bin/sh
# Usage: run.sh TRACE_DIR OUT_DIR   (expects TRACE_DIR/{msr,alibaba,tencent}/ with trace files)
set -eu
T=${1:?trace dir}; O=${2:?out dir}
B=$(dirname "$0")/target/release/tracesim
mkdir -p "$O"
for fmt in msr alibaba tencent; do
  [ -d "$T/$fmt" ] || { echo "not run: no $T/$fmt"; continue; }
  files=$(ls "$T/$fmt"/* )
  # shellcheck disable=SC2086
  taskset -c 8-15 "$B" chunk --set "$fmt" --fmt "$fmt" --out "$O" $files
  taskset -c 8-15 "$B" cache --set "$fmt" --fmt "$fmt" --out "$O" $files
done
taskset -c 8-15 "$B" gate "$O" | tee "$O/gate.txt"
