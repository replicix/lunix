#!/bin/bash
# P0.4 part 2: FLUSH / FUA / write counts per fsync on ext4, xfs, btrfs (default mkfs and mount
# options). A fresh daemon per file system; counters are snapshotted with SIGUSR1.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-flush}" "${FSES:=ext4 xfs btrfs}" "${N:=10000}" "${IMG_GIB:=8}"
mkdir -p "$OUT"
IMG="$LUNIX_SPIKE_DIR/rec.img"; MNT="$LUNIX_SPIKE_DIR/mnt/fl"; need_safe "$IMG"; need_safe "$MNT"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
add_cleanup "rm -f $IMG"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'; add_cleanup "umount -l $MNT"; add_cleanup 'pkill -x fio'
CSV="$OUT/flush-counts.csv"; echo "fs,step,jobs,reads,writes,fua_writes,flushes,per_fsync_writes,per_fsync_fua,per_fsync_flush" > "$CSV"
delta() { # prev(4 numbers) cur(4 numbers) -> 4 numbers
  python3 -c "import sys; a=list(map(int,sys.argv[1].split())); b=list(map(int,sys.argv[2].split())); print(*[y-x for x,y in zip(a,b)])" "$1" "$2"
}
rec() { # fs step jobs delta fsyncs
  read -r r w f fl <<< "$4"
  if [ -n "$5" ]; then
    echo "$1,$2,$3,$r,$w,$f,$fl,$(python3 -c "print(round($w/$5,3),round($f/$5,3),round($fl/$5,3),sep=',')")" >> "$CSV"
  else echo "$1,$2,$3,$r,$w,$f,$fl,,," >> "$CSV"; fi
}
for fs in $FSES; do
  DEVID=""; rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"; start_daemon || continue
  prev=$(counters)
  make_fs "$fs" "$DEV"; cur=$(counters); rec $fs mkfs - "$(delta "$prev" "$cur")"; prev=$cur
  mount_fs "$fs" "$DEV" "$MNT"; cur=$(counters); rec $fs mount - "$(delta "$prev" "$cur")"; prev=$cur
  for variant in fsync fdatasync o_sync o_dsync; do
    for nj in 1 8; do
      case $variant in fsync) opt="--fsync=1";; fdatasync) opt="--fdatasync=1";; o_sync) opt="--sync=1";; o_dsync) opt="--sync=dsync";; esac
      per=$((N / nj)); sz="$((per * 4))k"
      common=(--name="$variant$nj" --filename_format="$MNT/$variant$nj.\$jobnum" --rw=write --bs=4k --size="$sz" --ioengine=psync --numjobs=$nj --group_reporting)
      fio "${common[@]}" --create_only=1 >/dev/null 2>&1; sync -f "$MNT"
      prev=$(counters)
      timeout -k 10 300 fio "${common[@]}" $opt --end_fsync=0 --output-format=json --output="$OUT/fio-$fs-$variant-$nj.json" >/dev/null 2>&1
      cur=$(counters); rec $fs $variant $nj "$(delta "$prev" "$cur")" $N; prev=$cur
      echo "$fs $variant nj=$nj done"
    done
  done
  umount "$MNT"; cur=$(counters); rec $fs umount - "$(delta "$prev" "$cur")"
  stop_daemon_clean; del_dev; DEVID=""
done
rm -f "$IMG"
column -s, -t "$CSV"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
