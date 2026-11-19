#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15: crash points that only an offline repair reaches. A crashpoints build of lunix-fsck (LUNIX_FSCK_CP) is killed (SIGKILL from inside, status
# 137) at <point>:1 during `--repair --truncate-tip` (store.set_aside.*) or `--repair --delete-orphans` (store.segment_delete.*), the repair is
# started again without the setting and must finish; the volume must then pass `lunix fsck --deep` (exit 0 or 1) and attach.
#   TARGETS="store.set_aside.before_rename store.set_aside.after_rename store.set_aside.after_dirsync store.segment_delete.before_unlink ..."
# Preparation per target: a 1 GiB write on a fresh integrity volume, detach; for set_aside one byte of the tip commit is flipped (the tip no
# longer authenticates); for segment_delete a copy of a sealed segment is placed under a higher id (an orphan above the tip's high-water mark).
here="$(cd "$(dirname "$0")" && pwd)"
: "${TARGETS:=store.set_aside.before_rename store.set_aside.after_rename store.set_aside.after_dirsync store.segment_delete.before_unlink store.segment_delete.after_unlink store.segment_delete.after_dirsync}"
: "${CHUNK:=65536}" "${VOLMODE:=integrity}"
export SCRIPT_CAP="${SCRIPT_CAP:-3600}"
. "$here/lib.sh"
: "${LUNIX_FSCK_CP:?LUNIX_FSCK_CP (lunix-fsck built with --features crashpoints) is not set}"
harness_init m15-fsckrepair
say "# M15 offline repair crash points ($(date -u +%FT%TZ)): $TARGETS"
for tgt in $TARGETS; do
  cell="fsck-repair/$tgt"; vol="rep$(echo "$tgt" | tr -cd 'a-z0-9' | tail -c 24)"; d=$(cell_dir "$cell"); say ""; say "## $cell"
  cell_begin "$vol" 4 "$CHUNK" "$VOLMODE"
  taskset -c "$FIO_CPUS" fio --name=w --filename="$CUR_DEV" --rw=write --bs=1M --iodepth=8 --ioengine=io_uring --direct=1 --size=1g --refill_buffers=1 \
    --end_fsync=1 --output-format=json --output="$d/fio-write.json" > "$d/fio-write.stdout" 2>&1; expect "$cell: 1 GiB written (fio rc $?)" $? || true
  detach_vol "$vol"; vd=$(vol_dir "$vol")
  case "$tgt" in
    store.set_aside.*) f=$(ls "$vd"/state/commits/* | sort | tail -1); python3 -I -c 'import sys; p,o=sys.argv[1],int(sys.argv[2]); f=open(p,"r+b"); f.seek(o); b=f.read(1); f.seek(o); f.write(bytes([b[0]^1])); f.flush()' "$f" $(( $(stat -c %s "$f") / 2 )); args=(--repair --truncate-tip) ;;
    *) s=$(ls -S "$vd"/segments/*.seg | head -1); cp "$s" "$vd/segments/ffffffffffff0001.seg"; args=(--repair --delete-orphans) ;;
  esac
  KOPT=(); [ "$VOLMODE" = encrypted ] && KOPT=(--key-file "$KEYS/enc.key")
  env LUNIX_CRASH_AT="$tgt:1" "$LUNIX_FSCK_CP" "${args[@]}" "${KOPT[@]}" "$vd" > "$d/repair1.txt" 2>&1; r1=$?
  say "- first repair with the crash setting: exit $r1"
  reached=0; [ "$r1" = 137 ] && reached=1
  "$LUNIX_FSCK_CP" "${args[@]}" "${KOPT[@]}" "$vd" > "$d/repair2.txt" 2>&1; r2=$?
  [ "$r2" -le 1 ]; expect "$cell: the repair run again without the crash setting finishes (exit $r2)" $? || true
  "$LUNIX_FSCK_CP" --deep --json "${KOPT[@]}" "$vd" > "$d/fsck-after.json" 2>&1; r3=$?
  [ "$r3" -le 1 ]; expect "$cell: fsck --deep after the repair clean (exit $r3)" $? || true
  attach_vol "$vol"; expect "$cell: the volume attaches after the repair" $? || true; detach_vol "$vol" >/dev/null 2>&1
  st=pass; [ "$r2" -le 1 ] && [ "$r3" -le 1 ] || st=fail; [ "$reached" = 1 ] || st=unreached
  [ "$st" = unreached ] && say "- UNREACHED $cell: the repair never executed the point (exit $r1)"
  rec "$cell" "$st" point="$tgt" kind=fsck-repair crashed="$reached" first_exit="$r1" repair_exit="$r2" fsck_after="$r3"
  [ "$st" = fail ] && FAIL=1
  [ -e "$OUT/WEDGED" ] && break
done
say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
