#!/bin/bash
# fio-matrix.sh <device> <outdir> <matrix-file> [--daemon-pid PID]
# Matrix line: "bs rw iodepth numjobs" (# comments allowed). Per cell: REPS runs,
# fio JSON saved per run, summary.csv written at the end.
# Env: FIO_EXTRA (extra fio args, word-split)  FIO_CPUS (default 4-7)  RUNTIME (20)  RAMP (3)  REPS (3)  SIZE (e.g. 32g,
# default: whole device)  WATCHDOG_EXTRA (30 s added to the timeout -k watchdog).
. "$(dirname "$0")/lib.sh"
dev="$1"; out="$2"; matrix="$3"; shift 3
daemon_pid=""
[ "$1" = "--daemon-pid" ] && daemon_pid="$2"
: "${FIO_CPUS:=4-7}" "${RUNTIME:=20}" "${RAMP:=3}" "${REPS:=3}"
need_safe "$dev"
mkdir -p "$out"
here="$(cd "$(dirname "$0")" && pwd)"
cpu_ticks() { # utime+stime of all threads of the daemon, in clock ticks
  [ -n "$daemon_pid" ] && [ -d /proc/$daemon_pid ] || { echo 0; return; }
  cat /proc/$daemon_pid/stat | awk '{print $14+$15}'
}
tck=$(getconf CLK_TCK)
# daemon_cpu_s is scaled by RUNTIME/(RUNTIME+RAMP) to match fio's post-ramp I/O count
echo "cell,bs,rw,iodepth,numjobs,rep,daemon_cpu_s" > "$out/daemon_cpu.csv"
while read -r bs rw qd nj; do
  case "$bs" in ""|\#*) continue;; esac
  for rep in $(seq 1 "$REPS"); do
    tag="${bs}_${rw}_qd${qd}_nj${nj}_r${rep}"
    extra=()
    [ -n "$SIZE" ] && extra+=(--size="$SIZE")
    case "$rw" in read|write) # sequential: give each job its own region
      if [ -n "$SIZE" ] && [ "$nj" -gt 1 ]; then
        sz=$(numfmt --from=iec "${SIZE^^}"); part=$((sz / nj))
        extra=(--size="$part" --offset_increment="$part")
      fi;;
    esac
    t0=$(cpu_ticks)
    timeout -k 10 $((RUNTIME + RAMP + ${WATCHDOG_EXTRA:-30})) fio --name=cell --filename="$dev" \
      --direct=1 --ioengine=io_uring --rw="$rw" --bs="$bs" --iodepth="$qd" --numjobs="$nj" \
      --time_based --runtime="$RUNTIME" --ramp_time="$RAMP" --norandommap --randrepeat=0 \
      --group_reporting --lat_percentiles=1 --cpus_allowed="$FIO_CPUS" --cpus_allowed_policy=split \
      $FIO_EXTRA "${extra[@]}" --output-format=json --output="$out/$tag.json" >/dev/null 2>"$out/$tag.err"
    rc=$?
    t1=$(cpu_ticks)
    echo "$tag,$bs,$rw,$qd,$nj,$rep,$(awk -v a=$t0 -v b=$t1 -v t=$tck -v r=$RUNTIME -v m=$RAMP 'BEGIN{printf "%.3f",(b-a)/t*r/(r+m)}')" >> "$out/daemon_cpu.csv"
    [ $rc = 0 ] || echo "fio rc=$rc for $tag" >&2
    [ -s "$out/$tag.err" ] || rm -f "$out/$tag.err"
  done
done < "$matrix"
python3 "$here/summarize.py" "$out" > "$out/summary.csv"
