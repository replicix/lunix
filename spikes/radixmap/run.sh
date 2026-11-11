#!/usr/bin/env bash
# Reproduces every number in bench/radixmap/RESULTS.md. Run on the test host
# (never on the authoring machine). Pinned to CPUs 8-15, one thread used.
#   bash run.sh [stage...]   stages: build seal diff cold sweep fan flat  (default: all)
# Output: $OUT (default ./out) CSV files; data packs in $DATA (default ./data),
# <= ~1 GiB at any time (2 GiB disk budget).
set -u
export PATH=$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")"
OUT=${OUT:-$PWD/out}; DATA=${DATA:-$PWD/data}
mkdir -p "$OUT" "$DATA"
cargo build --release -j 4 2>&1 | tail -2
BIN="timeout -k 10 3000 taskset -c 8-15 $PWD/target/release/radixmap"
R() { echo "+ radixmap $*" >&2; $BIN "$@" --dir "$DATA"; }
stages=${*:-build seal diff cold sweep fan flat}
has() { [[ " $stages " == *" $1 "* ]]; }

./hostinfo.sh > "$OUT/hostinfo.txt"
$BIN table > "$OUT/depth-table.md"

if has build; then
  : > "$OUT/build.csv"; H=--header
  # real file-backed builds at 1 TiB (leaves stored), sequential and random insertion order
  R build --tib 1 --leaf rb16 --pct 100 --order seq $H >> "$OUT/build.csv"; H=
  R build --tib 1 --leaf rb16 --pct 10  --order seq >> "$OUT/build.csv"
  R build --tib 1 --leaf rb16 --pct 100 --order rand >> "$OUT/build.csv"
  R build --tib 1 --leaf rb16 --pct 10  --order rand >> "$OUT/build.csv"
  R build --tib 2 --leaf rb16 --pct 100 --order rand >> "$OUT/build.csv"
  R build --tib 2 --leaf rb16 --pct 10  --order rand >> "$OUT/build.csv"
  # virtual-leaf builds (leaf bytes not retained, regenerated on read)
  for t in 1 10; do
    R build --tib $t --leaf rb16 --pct 100 --order seq --virtual >> "$OUT/build.csv"
    R build --tib $t --leaf rb16 --pct 10  --order seq --virtual >> "$OUT/build.csv"
  done
  R build --tib 10 --leaf row48lz4 --pct 100 --order seq --virtual >> "$OUT/build.csv"
  R build --tib 10 --leaf rb16 --pct 100 --order seq --virtual --frag 0 >> "$OUT/build.csv"
  for f in 256 1024; do R build --tib 10 --leaf rb16 --fan $f --pct 100 --order seq --virtual >> "$OUT/build.csv"; done
fi

if has seal; then
  R seal --tib 10 --leaf rb16 --virtual --flat > "$OUT/seal-10t-rb16.csv"
  R seal --tib 10 --leaf row48lz4 --virtual --pattern uniform --d 1000,10000 > "$OUT/seal-10t-row48lz4.csv"
  R seal --tib 10 --leaf rb16 --virtual --frag 0 --pattern uniform --d 1000,10000 > "$OUT/seal-10t-rb16-seqmodel.csv"
  R seal --tib 1 --leaf rb16 --pattern uniform,zipf,seq --d 1,10,100,1000,10000 > "$OUT/seal-1t-rb16-real.csv"
fi

if has fan; then
  for f in 256 512 1024; do
    R seal --tib 10 --leaf rb16 --fan $f --virtual --pattern uniform,zipf --d 100,1000,10000 > "$OUT/seal-10t-fan$f.csv"
  done
fi

if has diff; then
  R diff --tib 10 --leaf rb16 --virtual > "$OUT/diff-10t-rb16.csv"
  R diff --tib 10 --leaf row48lz4 --virtual --flat-blocks 1048576 > "$OUT/diff-10t-row48lz4.csv"
  R diff --tib 1 --leaf rb16 > "$OUT/diff-1t-rb16-real.csv"
fi

if has cold; then
  { R cold --tib 10 --leaf rb16 --virtual --latency-ms 25; R cold --tib 10 --leaf rb16 --virtual --latency-ms 0 | tail -n +2; } > "$OUT/cold-10t.csv"
  { R cold --tib 1 --leaf rb16 --latency-ms 25; R cold --tib 1 --leaf rb16 --latency-ms 0 | tail -n +2; } > "$OUT/cold-1t-real.csv"
fi

if has sweep; then
  : > "$OUT/sweep-10t.csv"; : > "$OUT/sweep-1t-real.csv"
  for c in 64 256 0; do R sweep --tib 10 --leaf rb16 --virtual --cache-mib $c | tee -a "$OUT/sweep-10t.csv.tmp" >/dev/null; done
  for c in 64 256 0; do R sweep --tib 1 --leaf rb16 --cache-mib $c | tee -a "$OUT/sweep-1t-real.csv.tmp" >/dev/null; done
  awk 'NR==1 || !/^tag/' "$OUT/sweep-10t.csv.tmp" > "$OUT/sweep-10t.csv"; awk 'NR==1 || !/^tag/' "$OUT/sweep-1t-real.csv.tmp" > "$OUT/sweep-1t-real.csv"
  rm -f "$OUT"/*.tmp
fi

if has flat; then
  R flatmem --tib 1 --leaf rb16 > "$OUT/flatmem-1t.csv"
fi
echo done >&2
