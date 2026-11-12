#!/bin/bash
# CONVENTIONS safety rule 5. Exit 0 only if nothing is left that is not listed
# in the current run's created-devices file.
. "$(dirname "$0")/lib.sh"
bad=0
reg() { grep -qxF -- "$1" "$CREATED_DEVICES" 2>/dev/null; }
report() { if reg "$1"; then echo "registered (this run): $1"; else echo "LEFTOVER: $2 $1"; bad=1; fi; }
for d in /dev/ublkb* /dev/ublkc*; do [ -e "$d" ] && report "$d" "ublk node"; done
for d in /sys/class/ublk-char/*; do [ -e "$d" ] && report "$d" "ublk-char"; done
for d in /dev/nullb*; do [ -e "$d" ] && report "$d" "null_blk device"; done
[ -d /sys/kernel/config/nullb ] && for d in /sys/kernel/config/nullb/*/; do [ -d "$d" ] && report "$d" "nullb configfs"; done
# Loop devices backed by files under the scratch dir (other loop devices are not ours).
while read -r name back; do
  case "$back" in "$LUNIX_SPIKE_DIR"/*) report "$name" "loop device (backing under scratch)";; esac
done < <(losetup -l -n -O NAME,BACK-FILE 2>/dev/null)
while read -r mp; do report "$mp" "mount"; done < <(awk -v d="$LUNIX_SPIKE_DIR/" 'index($2,d)==1{print $2}' /proc/mounts)
for p in $(pgrep -x -d' ' 'fio|ublk-spike|ublk-datapath|ublk-recovery|journal-bench' 2>/dev/null); do
  c=$(tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null)
  case "$c" in *ublk*|*nullb*|*loop*|*"$LUNIX_SPIKE_DIR"*|*journal-bench*) echo "LEFTOVER: process $p ${c:0:120}"; bad=1;; esac
done
for m in ublk_drv null_blk brd; do lsmod | grep -q "^$m " && echo "note: module $m loaded"; done
[ -d /run/ublksrv ] && ls /run/ublksrv 2>/dev/null | grep -q . && { echo "LEFTOVER: /run/ublksrv has $(ls /run/ublksrv | wc -l) file(s)"; bad=1; }
if [ $bad = 0 ]; then echo "ublk-leftovers: clean"; else echo "ublk-leftovers: NOT clean"; fi
exit $bad
