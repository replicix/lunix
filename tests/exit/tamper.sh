#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 tamper cell (integrity mode): after a crash and its recovery, with the volume offline, one byte of a stored object is flipped; the
# volume is attached again and read. The read, or fsck, must refuse the damaged data with an integrity error and must NEVER return wrong bytes.
#
#   VARIANTS="segment commit map"  CHUNKS="65536 16384"  (integrity volumes only: the keyed tags are what detects it)
# KILL_AFTER=0 skips the crash (the cell then tests the tamper detection alone). Per cell: a 2 GiB sequential incompressible write (every chunk live, no garbage) with a kill -9 of the daemon after KILL_AFTER seconds and a
# recovery; the SHA-256 of every MiB of the device is taken; detach; `lunix fsck --deep` must be clean; one byte (XOR 0x01) in the middle of
# the largest segment (variant segment), of the tip commit (commit) or of the largest map pack (map) is flipped; then
#   (a) `lunix fsck --deep` must exit 2 with an authentication finding;
#   (b) attach must be refused, OR the attached device must return read errors for the damaged blocks and NO block with other bytes
#       (readcheck.py: mismatches 0).
here="$(cd "$(dirname "$0")" && pwd)"
: "${VARIANTS:=segment commit map}" "${CHUNKS:=65536 16384}" "${KILL_AFTER:=8}" "${SIZE_GIB:=2}" "${VOLMODE:=integrity}"
export SCRIPT_CAP="${SCRIPT_CAP:-3600}"
. "$here/lib.sh"
harness_init m15-tamper
say "# M15 tamper cell ($(date -u +%FT%TZ)): variants $VARIANTS, chunks $CHUNKS"
LEN=$((SIZE_GIB << 30))
flip() { # file offset: XOR one byte
  python3 -I -c 'import sys; p,o=sys.argv[1],int(sys.argv[2]); f=open(p,"r+b"); f.seek(o); b=f.read(1); f.seek(o); f.write(bytes([b[0]^1])); f.flush(); import os; os.fsync(f.fileno())' "$1" "$2"
}
run_cell() { # chunk variant
  local chunk=$1 var=$2
  local cell="tamper/c$((chunk / 1024))k/$var" vol="tam$var$((chunk / 1024))"
  local d; d=$(cell_dir "$cell"); say ""; say "## $cell"
  cell_begin "$vol" 8 "$chunk" "$VOLMODE"
  # the write, with a crash and a recovery in the middle
  taskset -c "$FIO_CPUS" timeout -k 30 900 fio --name=w --filename="$CUR_DEV" --rw=write --bs=1M --iodepth=8 --ioengine=io_uring --direct=1 --size="${SIZE_GIB}g" \
    --refill_buffers=1 --end_fsync=1 --output-format=json --output="$d/fio-write.json" > "$d/fio-write.stdout" 2>&1 &
  local fp=$!
  if [ "$KILL_AFTER" != 0 ]; then
    sleep "$KILL_AFTER"; kill -KILL "$DPID"; wait "$DPID" 2>/dev/null; DPID=""
    sleep 1; start_lunixd || wedged "restart after the kill failed"
  fi
  wait_fio "$fp" 900; local frc=$?
  [ "$frc" = 0 ] && [ "$(fio_errors "$d/fio-write.json")" = 0 ]; expect "$cell: the write finished across the kill and the recovery (fio rc $frc)" $? || return
  python3 -I "$here/readcheck.py" make "$CUR_DEV" "$LEN" "$d/ref.sha"; expect "$cell: reference hashes of $SIZE_GIB GiB taken" $? || return
  detach_vol "$vol"; expect "$cell: detach" $? || return
  lunix_fsck "$vol" "$(basename "$d")-before"; expect "$cell: fsck --deep clean before the tampering (exit $FSCK_RC)" $? || return
  local vd; vd=$(vol_dir "$vol")
  local target off
  case "$var" in
    segment) target=$(ls -S "$vd"/segments/*.seg | head -1) ;;
    commit) target=$(ls "$vd"/state/commits/* | sort | tail -1) ;;
    map) target=$(ls -S "$vd"/map/* | head -1) ;;
  esac
  local sz; sz=$(stat -c %s "$target"); off=$((sz / 2))
  say "- flipping one byte at offset $off of $(basename "$(dirname "$target")")/$(basename "$target") ($sz bytes)"
  flip "$target" "$off"
  # (a) fsck must see it
  lunix_fsck "$vol" "$(basename "$d")-after"; local frc2=$FSCK_RC
  local codes; codes=$(jq -r '[.findings[] | select(.severity == "error") | .code] | unique | join(",")' "$OUT/raw/fsck-$(basename "$d")-after.json" 2>/dev/null)
  [ "$frc2" = 2 ] && [ -n "$codes" ]; expect "$cell: fsck --deep reports an error after the flip (exit $frc2: $codes)" $? || true
  # (b) attach and read
  local arc=0 refused="no" rerr=0 rmis=0
  attach_vol "$vol"; arc=$?
  if [ "$arc" != 0 ]; then
    refused=yes; cp "$OUT/lx.$_cn.attach-$vol.txt" "$d/attach-refusal.txt" 2>/dev/null
    say "- attach refused (rc $arc): $(head -c 200 "$d/attach-refusal.txt" 2>/dev/null | tr '\n' ' ')"
  else
    local dev; dev=$(dev_of "$vol"); register_id "${dev#/dev/ublkb}"
    python3 -I "$here/readcheck.py" check "$dev" "$LEN" "$d/ref.sha" > "$d/readcheck.json"; rerr=$(jq .errors "$d/readcheck.json"); rmis=$(jq .mismatches "$d/readcheck.json")
    say "- attached; read check: $(cat "$d/readcheck.json")"
    detach_vol "$vol" >/dev/null 2>&1
  fi
  [ "$rmis" = 0 ]; expect "$cell: no block returned other bytes (mismatches $rmis)" $? || true
  { [ "$refused" = yes ] || [ "${rerr:-0}" -gt 0 ]; }; expect "$cell: the damage was refused (attach refused: $refused, read errors: ${rerr:-0})" $? || true
  rec "$cell" "$([ "$rmis" = 0 ] && { [ "$refused" = yes ] || [ "${rerr:-0}" -gt 0 ]; } && [ "$frc2" = 2 ] && echo pass || echo fail)" variant="$var" chunk="$chunk" fsck_exit="$frc2" fsck_codes="$codes" attach_refused="$refused" read_errors="${rerr:-0}" mismatches="${rmis:-0}"
}
for c in $CHUNKS; do for v in $VARIANTS; do run_cell "$c" "$v"; [ -e "$OUT/WEDGED" ] && break 2; done; done
say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
