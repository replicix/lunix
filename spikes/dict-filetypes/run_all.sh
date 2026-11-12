#!/bin/sh
# Full reproduction on the test host (about 6-10 h on 4 cores). Run from this directory.  Needs: python3 (3.14, stdlib compression.zstd),
# python3-numpy/scipy/brotli/magic, libzstd-dev, gcc, git, curl, ffmpeg-free.  Outputs under $DICTFT_WORK; summaries copied to $1.
set -eu
export DICTFT_CORPUS=${DICTFT_CORPUS:-$HOME/dictft/corpus} DICTFT_WORK=${DICTFT_WORK:-$HOME/dictft/work} NPROC=4
PIN="taskset -c 8-11"; OUT=${1:-$DICTFT_WORK/out}
mkdir -p "$DICTFT_CORPUS" "$DICTFT_WORK" "$OUT"
$PIN python3 -I fetch.py projects.tsv "$DICTFT_CORPUS" --lock sources.lock.tsv       # pinned commits from sources.lock.tsv when present
for s in logs loghub jars wasm elf sqlite; do $PIN python3 -I extras.py "$DICTFT_CORPUS" $s; done
$PIN python3 -I train.py                      # 2 arms x 5 dictionary sets
$PIN python3 -I train.py --sweep              # dictionary-size and training-size sweeps
$PIN python3 -I measure.py                    # compress every test object with every variant
$PIN python3 -I measure.py --chunk-sens       # 1 MiB / 16 MiB chunk sensitivity
$PIN python3 -I select_dict.py                # extension map, libmagic, naive Bayes, combinations
$PIN python3 -I sweep_eval.py "$OUT/sweeps.csv"
$PIN python3 -I analyze_sel.py "$DICTFT_WORK/out" "$OUT"
$PIN python3 -I analyze.py "$OUT"
$PIN python3 -I chunk_sens_summary.py "$OUT"
$PIN python3 -I decide.py "$OUT" > "$OUT/decision.md"
$PIN python3 -I hybrid_eval.py "$OUT"
$PIN python3 -I figs.py "$OUT" "$OUT/figs"
$PIN python3 -I md_tables.py "$OUT" > "$OUT/tables.md"
gcc -O2 tp.c -o "$DICTFT_WORK/tpbin" -lzstd && $PIN python3 -I throughput.py "$OUT"   # use one idle core (see throughput.py)
