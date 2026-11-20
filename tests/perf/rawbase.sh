#!/bin/bash
# ADR-33 T6, W9: the raw fio baseline on the file system and device that hold the journal and the store.
#   env DIR=<dir on that file system> OUT=<result dir> bash tests/perf/rawbase.sh      (no root needed)
# Runs the Lunix cells' patterns directly on one file in DIR (fill first), back to back, one repetition set of REPS
# (default 3) of RUNTIME s (30) each, then the fdatasync commit cells. fio on FIO_CPUS (8-15), like the Lunix cells.
# Results: $OUT/raw/fio-<cell>-repN.json, $OUT/rawbase.tsv (cell, rep, IOPS, MB/s, p50/p99 us, errors).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
: "${DIR:?DIR is not set}" "${OUT:?OUT is not set}" "${REPS:=3}" "${RUNTIME:=30}" "${RAMP:=5}" "${WS_GIB:=4}" "${JOBS:=8}" "${FIO_CPUS:=8-15}"
JOB="$here/../../bench/phase1/perf/fio/raw-t6.fio"
mkdir -p "$DIR" "$OUT/raw"
WS=$((WS_GIB << 30)); SLICE=$((WS / JOBS))
export DIR WS SLICE RAMP CPUS="$FIO_CPUS" JOBS SEED=7
: > "$OUT/rawbase.tsv"
echo -e "cell\trep\tiops\tbw_bytes\tp50_us\tp99_us\tp999_us\terrors" > "$OUT/rawbase.tsv"
echo "# raw baseline $(date -u +%FT%TZ) dir fs: $(findmnt -no FSTYPE -T "$DIR"), $(nproc) cpus, WS ${WS_GIB} GiB" | tee "$OUT/rawbase.txt"
trap 'rm -f "$DIR/rawfile"' EXIT
# fill: the whole file, 1 MiB direct sequential (same as the Lunix fill)
RUNTIME=1 timeout 900 fio --name=fill --directory="$DIR" --filename=rawfile --ioengine=io_uring --direct=1 --rw=write --bs=1m --iodepth=32 \
  --size="$WS" --refill_buffers=1 --buffer_compress_percentage=50 --end_fsync=1 --cpus_allowed="$FIO_CPUS" --output-format=json --output="$OUT/raw/fio-fill.json" >/dev/null 2>&1
sync; sleep 5
for rep in $(seq 1 "$REPS"); do
  for c in w4k r4k seqw seqr commit64k commit4k commit4k-x8 r4k-buffered seqr-buffered; do
    case "$c" in *-buffered) cat "$DIR/rawfile" > /dev/null ;; esac
    rt=$RUNTIME; case "$c" in w4k) rt=60 ;; esac
    f="$OUT/raw/fio-$c-rep$rep.json"
    RUNTIME=$rt timeout -k 30 $((rt + RAMP + 120)) fio "$JOB" --section="$c" --output-format=json --output="$f" > "$f.stdout" 2>&1
    sed -n '/^{/,$p' "$f" | jq -r --arg c "$c" --arg r "$rep" '.jobs[0] as $j |
      ([$j.read,$j.write]|map(.iops//0)|add) as $i | ([$j.read,$j.write]|map(.bw_bytes//0)|add) as $b |
      ([$j.read,$j.write]|map(select((.total_ios//0)>0))|.[0]) as $d | ($d.clat_ns.percentile // {}) as $p |
      [$c,$r,$i,$b,(($p["50.000000"]//0)/1000),(($p["99.000000"]//0)/1000),(($p["99.900000"]//0)/1000),([.jobs[]|((.error//0)|tonumber)]|add)]|@tsv' >> "$OUT/rawbase.tsv"
    tail -1 "$OUT/rawbase.tsv" | tee -a "$OUT/rawbase.txt"
  done
done
