#!/bin/bash
# P0.3 step 2: feature table plus live tests (ADD_DEV acceptance, NO_AUTO_PART_SCAN,
# SAFE_STOP_DEV / TRY_STOP_DEV, STOP_DEV with a mounted fs). Root, scratch only.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-features}"
mkdir -p "$OUT"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
IMG="$LUNIX_SPIKE_DIR/feat.img"; MNT="$LUNIX_SPIKE_DIR/mnt/feat"; need_safe "$IMG"; need_safe "$MNT"
add_cleanup "rm -f $IMG"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'; add_cleanup "umount -l $MNT"
R="$OUT/features.md"; : > "$R"
say() { echo "$*" | tee -a "$R"; }

say "# Feature probe"; say ""
"$REC_BIN" features | tee -a "$R"
say ""; say "## ADD_DEV acceptance (device added with no server, then deleted)"; say ""
say "| flags tried (always with CMD_IOCTL_ENCODE) | hex | result |"; say "|---|---|---|"
probe() { # label hex
  local r; r=$("$REC_BIN" probe-add --flags "$2" 2>&1 | head -1 | sed 's/.*\(accepted\|rejected.*\)/\1/; s/id=[0-9]*//')
  say "| $1 | $2 | $r |"
}
probe "NO_AUTO_PART_SCAN" 0x40040
probe "SAFE_STOP_DEV" 0x20040
probe "USER_RECOVERY + USER_RECOVERY_FAIL_IO" 0x248
probe "USER_RECOVERY + REISSUE + FAIL_IO (both)" 0x258
probe "QUIESCE without USER_RECOVERY" 0x1040
probe "USER_RECOVERY + QUIESCE" 0x1048
probe "UPDATE_SIZE" 0x440
probe "BATCH_IO" 0x8040
probe "USER_COPY" 0xc0
probe "BATCH_IO + USER_COPY" 0x80c0
probe "PER_IO_DAEMON" 0x2040
probe "INTEGRITY without USER_COPY" 0x10040
probe "UNPRIVILEGED_DEV + USER_COPY" 0xe0
probe "without CMD_IOCTL_ENCODE (legacy opcodes)" 0x0

say ""; say "## NO_AUTO_PART_SCAN: GPT-labelled backing file"; say ""
rm -f "$IMG"; truncate -s 1G "$IMG"
# the ublk device has 4096-byte logical blocks, so write the GPT through a 4096-byte-sector loop device
lo=$(losetup -f --show -b 4096 "$IMG"); register_device "$lo"; safe_path "$lo" || exit 99
printf 'label: gpt\n,512M\n' | sfdisk -q "$lo" >/dev/null 2>&1; losetup -d "$lo"
for variant in plain no_part_scan; do
  DEVID=""; X=(); [ $variant = no_part_scan ] && X=(--extra-flags 0x40000)
  start_daemon "${X[@]}" || { say "- $variant: start failed"; continue; }
  sleep 2
  p1=/dev/ublkb${DEVID}p1
  if [ -e "$p1" ]; then a=present; else a=absent; fi
  say "- $variant: after start (2 s) $p1 is $a"
  if [ "$a" = absent ]; then
    partx -a "$DEV" 2>&1 | head -2; sleep 1
    [ -e "$p1" ] && say "- $variant: after \`partx -a\` $p1 is present" || say "- $variant: after \`partx -a\` $p1 still absent"
  fi
  say "- $variant: dev_info flags = $(grep -o 'DEVINFO after-add.*' "$DLOG" | head -1)"
  stop_daemon_clean; del_dev; DEVID=""
done

say ""; say "## SAFE_STOP_DEV: TRY_STOP_DEV with a mounted file system"; say ""
rm -f "$IMG"; fallocate -l 1G "$IMG"; DEVID=""
start_daemon || exit 1
make_fs ext4 "$DEV"; mount_fs ext4 "$DEV" "$MNT"; echo data > "$MNT/f"; sync -f "$MNT"
say "- TRY_STOP_DEV while mounted: $("$REC_BIN" try-stop --id "$DEVID")"
say "- device still present afterwards: $([ -b "$DEV" ] && echo yes || echo no); file readable: $(cat "$MNT/f" 2>&1); write works: $(echo more >> "$MNT/f" && sync -f "$MNT" && echo yes)"
umount "$MNT"
say "- TRY_STOP_DEV after umount: $("$REC_BIN" try-stop --id "$DEVID")"
for i in $(seq 1 50); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
say "- daemon exited after TRY_STOP_DEV: $(kill -0 "$DPID" 2>/dev/null && echo no || echo yes); bdev present: $([ -b "$DEV" ] && echo yes || echo no)"
stop_daemon_clean; del_dev; DEVID=""

say ""; say "## Plain STOP_DEV while a file system is mounted (observation only)"; say ""
rm -f "$IMG"; fallocate -l 1G "$IMG"
start_daemon || exit 1
make_fs ext4 "$DEV"; mount_fs ext4 "$DEV" "$MNT"; echo data > "$MNT/f"; sync -f "$MNT"
say "- STOP_DEV (raw, via timeout 20): $(timeout -k 5 20 "$REC_BIN" stop --id "$DEVID" 2>&1 | tr '\n' ' ') rc=$?"
sleep 1
say "- bdev present after STOP_DEV: $([ -b "$DEV" ] && echo yes || echo no); still mounted: $(grep -c "$MNT" /proc/mounts)"
say "- read of the mounted file: $(timeout 10 cat "$MNT/f" 2>&1 | head -1)"
say "- write+sync on the mounted fs: $(timeout 10 bash -c "echo x >> $MNT/f && sync -f $MNT" 2>&1 | head -1) rc=$?"
umount -l "$MNT" 2>/dev/null; sleep 1
stop_daemon_clean; del_dev; DEVID=""
rm -f "$IMG"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
