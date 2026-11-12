#!/bin/bash
# Runs ON THE TEST HOST. Everything under ~/fsync-trace. CPUs 8-15 only.
# usage: run.sh setup | one FS SCEN REP | all "ext4 xfs" | teardown
# SCEN: s1 s2a s2b s3 s3e s4 s4b s6   (see README.md); WB=nowb disables write-behind in the stream generator
set -u
ROOT=$HOME/fsync-trace; SRC=$ROOT/src; OUT=$ROOT/out; MNT=$ROOT/mnt
T=/sys/kernel/tracing/instances/fsynctrace
SECS=${SECS:-20}; PIN="taskset -c 8-15"
mkdir -p "$OUT" "$MNT"

devname() { basename /sys/bus/pseudo/drivers/scsi_debug/adapter*/host*/target*/*/block/*; }

setup() {
  [ -x "$ROOT/gen" ] || cc -O2 -o "$ROOT/gen" "$SRC/gen.c" || exit 1
  if ! lsmod | grep -q '^scsi_debug'; then
    # blk-mq ram-backed disk with write-back cache + FUA (brd is bio-based and write-through: no rq events, no flushes)
    sudo modprobe scsi_debug dev_size_mb=4096 dpofua=1 delay=0 ndelay=0 && touch "$ROOT/.loaded_scsi_debug"
    sleep 2
  fi
  sudo mkdir -p $T
  echo mono | sudo tee $T/trace_clock >/dev/null
  echo 32768 | sudo tee $T/buffer_size_kb >/dev/null
}

teardown() {
  sudo pkill -f "cat $T/trace_pipe" 2>/dev/null
  for e in block_bio_queue block_rq_issue block_rq_complete; do echo 0 | sudo tee $T/events/block/$e/enable >/dev/null 2>&1; done
  echo | sudo tee $T/trace >/dev/null 2>&1
  sudo rmdir $T 2>/dev/null
  mountpoint -q "$MNT" && sudo umount "$MNT"
  [ -f "$ROOT/.loaded_scsi_debug" ] && sudo rmmod scsi_debug && rm -f "$ROOT/.loaded_scsi_debug"
}

mkfs_mount() { # fs
  local d=/dev/$(devname)
  case $1 in
    ext4) sudo mkfs.ext4 -q -F -E lazy_itable_init=0,lazy_journal_init=0 $d ;;
    xfs) sudo mkfs.xfs -q -f $d ;;
    btrfs) sudo mkfs.btrfs -q -f $d ;;
  esac || return 1
  sudo mount $d "$MNT" && sudo chown "$(id -u):$(id -g)" "$MNT"
}

trace_on() { # outfile
  local mn=$(cat /sys/block/$(devname)/dev) ; local maj=${mn%:*} min=${mn#*:}
  local dv=$(( (maj << 20) | min ))
  for e in block_bio_queue block_rq_issue block_rq_complete; do
    echo "dev == $dv" | sudo tee $T/events/block/$e/filter >/dev/null
  done
  echo | sudo tee $T/trace >/dev/null
  sudo $PIN cat $T/trace_pipe > "$1" &
  sleep 0.3
  for e in block_bio_queue block_rq_issue block_rq_complete; do echo 1 | sudo tee $T/events/block/$e/enable >/dev/null; done
}
trace_off() {
  for e in block_bio_queue block_rq_issue block_rq_complete; do echo 0 | sudo tee $T/events/block/$e/enable >/dev/null; done
  sleep 0.5; sudo pkill -f "cat $T/trace_pipe"; sleep 0.3
  echo | sudo tee $T/trace >/dev/null
}

one() { # fs scen rep
  local fs=$1 sc=$2 rep=$3 key=$1-$2-$3 mn
  local tr=$OUT/$key.trace g=$OUT/$key.gen
  : > $g
  mkfs_mount $fs || { echo "mkfs/mount failed $key"; return 1; }
  local sync=fsync labs=small=$OUT/$key.small.map,stream=$OUT/$key.stream.map
  case $sc in s3|s3e) sync=fdatasync ;; esac
  case $sc in
    s2a|s2b|s2p|s3e) $ROOT/gen prep $MNT/stream 1024 ;;
    s4) $ROOT/gen prep $MNT/dfile 64 ;;
  esac
  : > $MNT/small; sync -f $MNT; sleep 3
  trace_on $tr
  local t0=$(date +%s.%N)
  case $sc in
    s1|s3) $PIN $ROOT/gen small $MNT/small $SECS 50 $sync >> $g ;;
    s2a|s3e) $PIN $ROOT/gen stream $MNT/stream $SECS 200 1024 ${WB:-wb} >> $g & sp=$!; $PIN $ROOT/gen small $MNT/small $SECS 50 $sync >> $g; wait $sp ;;
    s2p) $PIN $ROOT/gen stream $MNT/stream $SECS 200 1024 nowb >> $g & sp=$!; $PIN $ROOT/gen small $MNT/small $SECS 50 $sync >> $g; wait $sp ;;
    s2b) $PIN $ROOT/gen stream $MNT/stream $SECS 1000 1024 ${WB:-wb} >> $g & sp=$!; $PIN $ROOT/gen small $MNT/small $SECS 50 $sync >> $g; wait $sp ;;
    s4) $PIN $ROOT/gen dsync $MNT/dfile $SECS 200 64 ow >> $g ;;
    s4b) $PIN $ROOT/gen dsync $MNT/dfile $SECS 200 64 append >> $g ;;
    s6) $PIN python3 $SRC/sqlite_wal.py $MNT/db $SECS 100 >> $g ;;
  esac
  sleep 1
  trace_off
  sync -f $MNT   # never bare sync: it would wait on other users' file systems
  local seqlabel=small
  case $sc in
    s4|s4b) cp /dev/null $OUT/$key.small.map; filefrag -v -b512 $MNT/dfile > $OUT/$key.small.map ;;
    s6) filefrag -v -b512 $MNT/db > $OUT/$key.small.map; filefrag -v -b512 $MNT/db-wal > $OUT/$key.stream.map; seqlabel=stream ;;
    *) filefrag -v -b512 $MNT/small > $OUT/$key.small.map 2>/dev/null; [ -e $MNT/stream ] && filefrag -v -b512 $MNT/stream > $OUT/$key.stream.map ;;
  esac
  [ $sc = s4b ] && { filefrag -v -b512 $MNT/dfile > $OUT/$key.small.map; }
  sudo umount "$MNT"
  mn=$(cat /sys/block/$(devname)/dev)
  python3 -I $SRC/analyze.py $tr $mn $OUT/$key "fs=$fs,scen=$sc,rep=$rep,secs=$SECS,sync=$sync,seqlabel=$seqlabel" \
     small=$OUT/$key.small.map stream=$OUT/$key.stream.map
  tail -n 3 $g
  [ "${KEEP:-0}" = 1 ] || rm -f $tr
}

case ${1:-} in
  setup) setup ;;
  teardown) teardown ;;
  one) setup; one "$2" "$3" "$4" ;;
  all) setup
       for fs in $2; do for sc in ${SCENS:-s1 s2a s2b s3 s3e s4 s4b s6}; do for rep in 1 2 3; do
         echo "== $fs $sc $rep $(date +%T)"; one $fs $sc $rep
       done; done; done ;;
esac
