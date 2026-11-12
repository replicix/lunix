#!/bin/bash
# P0.5 driver (root, scratch only): ext4 / xfs / btrfs images on loop devices (direct-io on),
# plus a directory on the scratch disk's own file system. Env: FSES, SECS, OUT, QUICK, IMG_GIB, RATES (offered rec/s list), ONLY_PRIMARY.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"
require_root
: "${OUT:=$here/out}" "${FSES:=ext4 xfs btrfs btrfs-nodatacow root-dir ext4-noatime xfs-noatime btrfs-noatime}" "${IMG_GIB:=12}" "${SECS:=3}" "${CPU:=0}"
export RATES ONLY_PRIMARY BIN="$here/target/release/journal-bench" SECS CPU QUICK
mkdir -p "$OUT"; R="$OUT/results.jsonl"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
{ echo "write_cache: $(cat /sys/block/nvme0n1/queue/write_cache 2>/dev/null || echo not-exposed)"; \
  echo "root fs: $(findmnt -no FSTYPE /)"; } > "$OUT/hw.txt"
LOOP=""; IMG=""; MNT="$LUNIX_SPIKE_DIR/mnt/jb"; need_safe "$MNT"
teardown() { for _i in 1 2 3; do umount "$MNT" 2>/dev/null && break; sleep 1; done; mountpoint -q "$MNT" && umount -l "$MNT"; [ -n "$LOOP" ] && losetup -d "$LOOP" 2>/dev/null; [ -n "$IMG" ] && rm -f "$IMG"; LOOP=""; IMG=""; }
add_cleanup teardown
prep() { # dir  -> preallocated zero-filled 8 GiB journal file (btrfs-nodatacow: +C first)
  local dir=$1 nocow=$2; rm -f "$dir/journal.dat"
  : > "$dir/journal.dat"; [ "$nocow" = 1 ] && chattr +C "$dir/journal.dat"
  fallocate -l 8G "$dir/journal.dat"
  dd if=/dev/zero of="$dir/journal.dat" bs=4M count=2048 oflag=direct conv=notrunc status=none
  sync -f "$dir/journal.dat"
}
for cfg in $FSES; do
  fs=${cfg%%-*}; opts="nosuid,nodev"; nocow=0; full=1
  case "$cfg" in
    *-noatime) opts="$opts,noatime"; full=0;;
    btrfs-nodatacow) nocow=1; full=0;;
  esac
  [ -n "$ONLY_PRIMARY" ] && full=0
  echo "== $cfg $(date +%T)"
  if [ "$cfg" = root-dir ]; then
    dir="$LUNIX_SPIKE_DIR/jroot"; mkdir -p "$dir"; need_safe "$dir"
  else
    IMG="$LUNIX_SPIKE_DIR/jb-$cfg.img"; need_safe "$IMG"; rm -f "$IMG"; truncate -s "${IMG_GIB}G" "$IMG"
    LOOP=$(losetup --direct-io=on -f --show "$IMG") || { echo "losetup failed"; continue; }
    register_device "$LOOP"; safe_path "$LOOP" || exit 99
    case $fs in ext4) mkfs.ext4 -q -F "$LOOP";; xfs) mkfs.xfs -q -f "$LOOP";; btrfs) mkfs.btrfs -q -f "$LOOP" >/dev/null;; esac
    mkdir -p "$MNT"; mount -o "$opts" "$LOOP" "$MNT" || { echo "mount failed"; teardown; continue; }
    dir="$MNT/j"; mkdir -p "$dir"
    [ $nocow = 1 ] && chattr +C "$dir"
  fi
  prep "$dir" $nocow
  FULL=$full python3 "$here/matrix.py" "$dir" "$cfg" "$R"
  rm -f "$dir"/journal.dat "$dir"/growing.dat
  teardown
done
rm -rf "$LUNIX_SPIKE_DIR/jroot"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
