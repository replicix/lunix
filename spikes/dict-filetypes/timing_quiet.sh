#!/bin/sh
# Timing cells on a quiet host (run there, from ~/dictft-bench/code).  Re-fetches a small subset at the pinned commits, trains
# four dictionaries on it (speed does not depend on dictionary content, only on size), builds tp.c and runs throughput.py on one pinned core.
set -eu
B=$HOME/dictft-bench; export DICTFT_CORPUS=$B/corpus DICTFT_WORK=$B/work NPROC=4 TIMING_SUBSET=1 TPCORE=4
mkdir -p "$DICTFT_CORPUS" "$DICTFT_WORK" "$B/out"; cd "$B/code"
SUBSET=requests,flask,django,pandas,redis,lua,curl,react,express,lodash,bootstrap,guava,gson,fmt,ripgrep,hugo-docs,bcd,vega-datasets,jq,libuv
taskset -c 4-11 python3 -I fetch.py projects.tsv "$DICTFT_CORPUS" --lock sources.lock.tsv --only $SUBSET
awk -F'\t' 'NR>1{print $1"\t"$6}' "$DICTFT_CORPUS/manifest.tsv" | sort > "$B/out/subset_hashes.tsv"   # id, sha256 (compared with the busy host)
taskset -c 4-11 python3 -I train.py
gcc -O2 tp.c -o "$DICTFT_WORK/tpbin" -lzstd
python3 -I throughput.py "$B/out"
