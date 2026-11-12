#!/bin/bash
# P0.12 step 4: lost-keeper case. Kill the keeper and the daemon together; the restarted daemon must
# refuse to recover and delete the device (EIO for the mounted ext4 and fio); it must never resume.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/../common/lib.sh"; . "$here/rec-lib.sh"
require_root
: "${OUT:=$here/out-lostkeeper}" "${IMG_GIB:=8}"
mkdir -p "$OUT"; QUEUES=1
IMG="$LUNIX_SPIKE_DIR/rec.img"; MNT="$LUNIX_SPIKE_DIR/mnt/lk"; KSOCK="$LUNIX_SPIKE_DIR/run/keeper.sock"
need_safe "$IMG"; need_safe "$MNT"; need_safe "$KSOCK"
bash "$here/../common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
rm -f "$IMG"; fallocate -l "${IMG_GIB}G" "$IMG"
add_cleanup "rm -f $IMG $KSOCK"; add_cleanup 'del_dev'; add_cleanup 'stop_daemon_clean'; add_cleanup "umount -l $MNT"
add_cleanup 'kill $KPID'
python3 "$here/keeper/keeper.py" "$KSOCK" > "$OUT/keeper.log" 2>&1 & KPID=$!
for i in $(seq 1 50); do grep -q ready "$OUT/keeper.log" && break; sleep 0.1; done
R="$OUT/result.md"; : > "$R"; say() { echo "$*" | tee -a "$R"; }
start_daemon --sink mem --keeper "$KSOCK" || exit 1
make_fs ext4 "$DEV"; mount_fs ext4 "$DEV" "$MNT"
(cd "$MNT" && timeout -k 10 120 fio --name=m --directory="$MNT" --rw=randrw --rwmixwrite=70 --bs=4k --iodepth=16 --numjobs=2 --size=512m \
   --ioengine=io_uring --direct=1 --verify=crc32c --verify_backlog=64 --verify_fatal=1 --norandommap --time_based --runtime=40 \
   --cpus_allowed="$FIO_CPUS" --output-format=json --output="$OUT/fio.json" > "$OUT/fio.stdout" 2>&1; echo $? > "$OUT/fio.rc") &
FPID=$!
python3 "$here/fsyncer.py" write "$MNT/fs1.dat" "$OUT/ack1" > "$OUT/fsyncer1.out" 2>&1 & S1=$!
sleep 8
say "# P0.12 lost-keeper test"
say "- before: keeper log: $(tr '\n' ';' < "$OUT/keeper.log")"
T0=$(now_ns)
kill -9 "$KPID" "$DPID"; wait "$DPID" 2>/dev/null
say "- killed keeper and daemon together"
taskset -c "$DAEMON_CPUS" "$REC_BIN" run --backing "$IMG" --queues 1 --cpus "$DAEMON_CPUS" --dev-id "$DEVID" --recover \
   --sink mem --keeper "$KSOCK" > "$OUT/daemon.restart.log" 2>&1
RC=$?
say "- restarted daemon exit code: $RC (3 = refused); output: $(grep -E 'REFUSED' "$OUT/daemon.restart.log" | tr '\n' ';')"
say "- time from kill to refusal: $(( ($(now_ns) - T0) / 1000000 )) ms"
sleep 1
say "- /dev/ublkb$DEVID present after refusal: $([ -b "$DEV" ] && echo YES || echo no); /sys/class/ublk-char entries: $(ls /sys/class/ublk-char 2>/dev/null | wc -l)"
wait "$FPID"; FRC=$(cat "$OUT/fio.rc" 2>/dev/null || echo NA)
wait "$S1"; S1RC=$?
say "- fio exit code: $FRC; fio json errors: $(jq -c '[.jobs[] | {error, total_err}]' "$OUT/fio.json" 2>/dev/null || echo none); fio stdout tail: $(grep -iE 'error|Input/output|verify' "$OUT/fio.stdout" | head -3 | tr '\n' ';')"
say "- fsyncer exit code: $S1RC ($(cat "$OUT/fsyncer1.out" | head -1))"
say "- ext4 view: mounted=$(grep -c "$MNT" /proc/mounts); touch/write on the mount: $(timeout 10 bash -c "echo x > $MNT/after && sync -f $MNT" 2>&1 | head -1) (rc=$?)"
say "- kernel log lines mentioning ublkb (count): $(dmesg 2>/dev/null | grep -c ublkb); sample: $(dmesg 2>/dev/null | grep ublkb | tail -3 | sed 's/^\[[^]]*\] //' | tr '\n' ';')"
umount -l "$MNT" 2>/dev/null
DEVID=""; DPID=""
say "- verdict: $([ "$RC" = 3 ] && [ ! -b "$DEV" ] && echo 'PASS: device refused and deleted; I/O failed' || echo 'FAIL: device resumed or was not deleted')"
bash "$here/../common/ublk-leftovers.sh" | tee "$OUT/leftovers-end.txt"
