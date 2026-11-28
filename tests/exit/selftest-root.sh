#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# Root self-test of the M15 building blocks against a LOOP device (no Lunix, no ublk): the fio verify job files
# (verify, state save, verify_only after a "re-attach", trim mixed in), trimcheck.py in raw and file mode, the
# guest file systems, and the xfstests configuration generator. Disk: about 1 GiB sparse, df-guard asks for 2.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> bash tests/exit/selftest-root.sh
here="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$here/../.." && pwd)"
. "$REPO/crates/lunix-ublk/tests/root/lib.sh"
require_root; need_root_tests
: "${OUT:=$LUNIX_SPIKE_DIR/m15-selftest-out}" "${FIO_CPUS:=14-15}"
D="$LUNIX_SPIKE_DIR/m15-selftest"; MNT="$LUNIX_SPIKE_DIR/mnt/m15-selftest"
need_safe "$OUT"; need_safe "$D"; need_safe "$MNT"
rm -rf "$OUT" "$D"; mkdir -p "$OUT" "$D" "$MNT"
bash "$REPO/spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; exit 2; }
: > "$CREATED_DEVICES"
script_cap 1200
df_guard "$LUNIX_SPIKE_DIR" 2 || exit 2
FAIL=0
ck() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; FAIL=1; fi; }
LOOP=""
# shellcheck disable=SC2329  # run through add_cleanup
selftest_cleanup() {
  mountpoint -q "$MNT" && umount "$MNT"
  [ -n "$LOOP" ] && losetup -d "$LOOP"
  rm -rf "$D"
}
add_cleanup selftest_cleanup
truncate -s 1G "$D/disk.img"; need_safe "$D/disk.img"
LOOP=$(losetup -f --show --direct-io=on "$D/disk.img") || exit 1
register_device "$LOOP"; need_safe "$LOOP"
FIO_PINNED() { taskset -c "$FIO_CPUS" fio "$@"; }
MB=$((1 << 20))
wsect() { awk '{print $7}' "/sys/block/${LOOP#/dev/}/stat"; }

echo "## raw: verify with trim companion, then verify_only"
export DEV="$LOOP" BASE=0 PER_JOB=$((128 * MB)) JOBS=2 QD=8 RUNTIME=15 SEED=11 LOOPS=1000000 VONLY=0 TRIM_BASE=$((256 * MB)) TRIM_SIZE=$((64 * MB))
FIO_PINNED "$REPO/bench/phase1/exit/fio/verify-raw.fio" --aux-path="$D" --output-format=json --output="$OUT/raw1.json" > "$OUT/raw1.out" 2>&1
ck "raw verify job (rc $?)" $?
[ "$(sed -n '/^{/,$p' "$OUT/raw1.json" | jq '[.jobs[]|.error]|add')" = 0 ]; ck "raw verify job: zero errors" $?
W0=$(wsect)
VONLY=1 LOOPS=1 FIO_PINNED "$REPO/bench/phase1/exit/fio/verify-raw.fio" --section=main --aux-path="$D" --output-format=json --output="$OUT/raw2.json" > "$OUT/raw2.out" 2>&1
ck "raw verify_only after the state save (rc $?)" $?
[ "$(sed -n '/^{/,$p' "$OUT/raw2.json" | jq '[.jobs[]|.read.io_bytes]|add')" = $((256 * MB)) ]; ck "raw verify_only read exactly one pass (fio reports verified bytes as write bytes too)" $?
[ "$(wsect)" = "$W0" ]; ck "raw verify_only wrote no sectors to the loop device" $?
[ "$(sed -n '/^{/,$p' "$OUT/raw2.json" | jq '[.jobs[]|.read.io_bytes]|add')" -gt 0 ]; ck "raw verify_only read the data" $?
python3 -I "$here/trimcheck.py" raw "$LOOP" $((320 * MB)) $((64 * MB)) 10 "$OUT/trim-raw.json" 5; ck "trimcheck raw (discard + write-zeroes)" $?
cat "$OUT/trim-raw.json"

echo "## ext4 on the loop device: file jobs, verify_only, trimcheck file"
mkfs.ext4 -F -q "$LOOP"; mount -t ext4 -o nosuid,nodev "$LOOP" "$MNT"; ck "mkfs and mount ext4" $?
export DIR="$MNT" PER_JOB=$((64 * MB)) TRIM_SIZE=$((32 * MB))
FIO_PINNED "$REPO/bench/phase1/exit/fio/verify-fs.fio" --aux-path="$D" --output-format=json --output="$OUT/fs1.json" > "$OUT/fs1.out" 2>&1
ck "fs verify job (rc $?)" $?
sync; W1=$(wsect)
VONLY=1 LOOPS=1 FIO_PINNED "$REPO/bench/phase1/exit/fio/verify-fs.fio" --section=main --aux-path="$D" --output-format=json --output="$OUT/fs2.json" > "$OUT/fs2.out" 2>&1
ck "fs verify_only (rc $?)" $?
[ "$(wsect)" = "$W1" ]; ck "fs verify_only wrote no sectors to the loop device" $?
python3 -I "$here/trimcheck.py" file "$MNT" $((32 * MB)) 10 "$OUT/trim-file.json" 7; ck "trimcheck file (punch hole + zero range)" $?
cat "$OUT/trim-file.json"
umount "$MNT"; e2fsck -fn "$LOOP" > "$OUT/e2fsck.txt" 2>&1; ck "guest e2fsck clean" $?
echo "overall FAIL=$FAIL"
exit "$FAIL"
