#!/bin/bash
# P0.4 part 1: UPDATE_SIZE online grow on ext4 / xfs / btrfs under load, special cases,
# and the write-cache sysfs attributes. Root, scratch only. Env: OUT, FSES, RUNTIME.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-resize}" "${FSES:=ext4 xfs btrfs}" "${RUNTIME:=45}" "${IMG_GIB:=16}"
mkdir -p "$OUT"
IMG="$LUNIX_SPIKE_DIR/rec.img"; MNT="$LUNIX_SPIKE_DIR/mnt/rz"; need_safe "$IMG"; need_safe "$MNT"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
add_cleanup "rm -f $IMG"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'; add_cleanup "umount -l $MNT"
R="$OUT/results.md"; : > "$R"
say() { echo "$*" | tee -a "$R"; }
sizes() { # label
  local s; s=$(cat /sys/block/ublkb$DEVID/size)
  say "- $1: sysfs size=$s sectors ($((s * 512)) bytes), blockdev --getsize64=$(blockdev --getsize64 "$DEV")"
}
fresh() { # fs [daemon args...]  -> new image, new device, mkfs, mount
  local fs=$1; shift
  DEVID=""; rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"
  start_daemon "$@" || return 1
  make_fs "$fs" "$DEV"; mount_fs "$fs" "$DEV" "$MNT"
}
finish() { # fs  -> unmount, fsck, delete device
  local fs=$1
  umount "$MNT"; local rc; rc=$(check_fs "$fs" "$DEV")
  say "- fsck/repair/check ($fs) after unmount: rc=$rc"
  stop_daemon_clean; del_dev; DEVID=""
}
fio_load() { # outfile
  (cd "$MNT" && timeout -k 10 $((RUNTIME + 60)) fio --name=g --directory="$MNT" --rw=randwrite --bs=4k --iodepth=16 --numjobs=2 \
     --size=1g --ioengine=io_uring --direct=1 --verify=crc32c --verify_backlog=64 --verify_fatal=1 --norandommap \
     --time_based --runtime="$RUNTIME" --cpus_allowed="$FIO_CPUS" --output-format=json --output="$1" >/dev/null 2>&1; echo $? > "$1.rc") &
  FPID=$!
}
grow_fs() { # fs
  case "$1" in
    ext4) resize2fs "$DEV" 2>&1 | tail -1;;
    xfs) xfs_growfs "$MNT" 2>&1 | grep -E "data +=" | head -1;;
    btrfs) btrfs filesystem resize max "$MNT" 2>&1;;
  esac
}
fs_size() { df -B1 --output=size "$MNT" | tail -1 | tr -d ' '; }

say "# P0.4 resize results"
say ""
for fs in $FSES; do
  say "## Online grow: $fs (16 GiB -> 64 GiB) under fio randwrite+verify"
  fresh "$fs" || { say "fresh failed"; continue; }
  sizes "before"; say "- df size before: $(fs_size)"
  d="$OUT/grow-$fs"; mkdir -p "$d"; fio_load "$d/fio.json"; sleep 8
  "$REC_BIN" resize --id "$DEVID" --bytes $((64 << 30)) --backing "$IMG" | tee -a "$R"
  sizes "after UPDATE_SIZE (no remount)"
  say "- fs grow: $(grow_fs $fs | tr '\n' ' ')"
  say "- df size after: $(fs_size)"
  wait "$FPID"; say "- fio exit code: $(cat $d/fio.json.rc); errors: $(jq -c '[.jobs[].total_err]' $d/fio.json 2>/dev/null)"
  finish "$fs"; say ""
done

say "## ext4 special cases"
fresh ext4 || exit 1
say "### grow immediately after kill -9 + recovery"
d="$OUT/grow-after-kill"; mkdir -p "$d"; fio_load "$d/fio.json"; sleep 6
kill -9 "$DPID"; wait "$DPID" 2>/dev/null
start_daemon --recover || say "recovery failed"
"$REC_BIN" resize --id "$DEVID" --bytes $((64 << 30)) --backing "$IMG" | tee -a "$R"
sizes "after resize following recovery"; say "- fs grow: $(grow_fs ext4 | tr '\n' ' ')"
wait "$FPID"; say "- fio exit code: $(cat $d/fio.json.rc); errors: $(jq -c '[.jobs[].total_err]' $d/fio.json 2>/dev/null)"
say "### no-op resize (to the current size)"
"$REC_BIN" resize --id "$DEVID" --bytes $((64 << 30)) | tee -a "$R"; sizes "after no-op"
say "### non-multiple of the 64 KiB chunk: 64 GiB + 12 KiB (3 x 4 KiB)"
"$REC_BIN" resize --id "$DEVID" --bytes $(((64 << 30) + 12288)) --backing "$IMG" | tee -a "$R"; sizes "after +12 KiB"
say "### not a multiple of the 4096-byte logical block: 64 GiB + 12 KiB + 512 B (raw command, driver behaviour)"
"$REC_BIN" resize --id "$DEVID" --bytes $(((64 << 30) + 12288 + 512)) --backing "$IMG" | tee -a "$R"; sizes "after +512 B"
umount "$MNT"; rc=$(check_fs ext4 "$DEV"); say "- fsck after unmount: rc=$rc (note: fs still 64 GiB, device is larger)"
say "### shrink (idle, unmounted device): 64 GiB -> 8 GiB, then back"
"$REC_BIN" resize --id "$DEVID" --bytes $((8 << 30)) | tee -a "$R"; sizes "after shrink"
say "- mount attempt on the shrunk device (fs is 64 GiB): $(mount -o nosuid,nodev "$DEV" "$MNT" 2>&1 | tr '\n' ' ') rc=$?"
umount "$MNT" 2>/dev/null
"$REC_BIN" resize --id "$DEVID" --bytes $((64 << 30)) | tee -a "$R"; sizes "after grow back"
rc=$(check_fs ext4 "$DEV"); say "- fsck after shrink+grow-back: rc=$rc"
stop_daemon_clean; del_dev; DEVID=""

say ""; say "## Write-cache visibility (sysfs after ADD_DEV)"
for variant in default no-volatile-cache; do
  DEVID=""; rm -f "$IMG"; fallocate -l 1G "$IMG"
  if [ $variant = default ]; then start_daemon; else start_daemon --no-volatile-cache; fi
  say "### $variant"
  for a in write_cache fua rotational discard_granularity max_sectors_kb max_hw_sectors_kb logical_block_size physical_block_size optimal_io_size minimum_io_size; do
    say "- $a = $(cat /sys/block/ublkb$DEVID/queue/$a)"
  done
  grep -E '^PARAMS after-add' "$DLOG" | head -1 | sed 's/^/- /' | tee -a "$R"
  stop_daemon_clean; del_dev; DEVID=""
done
rm -f "$IMG"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
