#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 1: fio verify on the raw device and on ext4 and xfs files, chunk sizes 16 KiB and 64 KiB, encrypted and
# integrity (unencrypted) volumes.
#
# One CELL = (target, chunk size, mode) on a fresh 32 GiB volume (deleted and recreated between cells). Per cell:
#   for each PROFILE (queue depth:jobs) one fio job group of PER_PROFILE_MIB (4 GiB; the plan allows 12 GiB per
#   group) with bssplit 4k-1M, crc32c verify with state save, a psync companion that mixes trim into its own
#   region (trim_percentage, trim_verify_zero), and trimcheck.py running discard and write-zeroes (blkdiscard -z)
#   checks on a third region, all for RUNTIME seconds (600);
#   then umount, guest fsck, detach, `lunix fsck --deep`; attach again, `fio --verify_only` of every profile from
#   its saved state; detach, `lunix fsck --deep` again.
# Gate: zero fio errors, zero verify failures, trimcheck clean, both fscks clean (exit 0, or 1 with only orphans).
#
# Runtime: cells x profiles x (RUNTIME + verify) ~ 12 cells x 6 x 12 min = about 14 h; run slices with CHUNKS,
# MODES, TARGETS (one invocation per slice); the script prints the cap it needs for LUNIX_ROOT_TIMEOUT.
# Disk: the store of a cell (about 15 GiB written before dedup and compression) plus 32 GiB of guest file system
# metadata at most; df-guard asks for MKFS_GIB (default 40) before every mkfs, the whole run peaks below 100 GiB.
#
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd LUNIX=<abs>/lunix \
#       TESTD=<abs>/lunix-ublk-testd [CHUNKS="16384 65536"] [MODES="integrity encrypted"] [TARGETS="raw ext4 xfs"] \
#       bash tests/exit/fio-verify.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${CHUNKS:=16384 65536}" "${MODES:=integrity encrypted}" "${TARGETS:=raw ext4 xfs}"
: "${PROFILES:=1:1 32:1 128:1 1:8 32:8 128:8}" "${RUNTIME:=600}" "${VOL_GIB:=32}" "${PER_PROFILE_MIB:=4096}"
: "${TRIM_MIB:=256}" "${ZERO_MIB:=1024}" "${VERIFY_GRACE:=900}"
read -ra _c <<< "$CHUNKS"; read -ra _m <<< "$MODES"; read -ra _t <<< "$TARGETS"; read -ra PROF <<< "$PROFILES"
NCELLS=$((${#_c[@]} * ${#_m[@]} * ${#_t[@]}))
CELL_SECS=$((${#PROF[@]} * (RUNTIME + VERIFY_GRACE) + 3600))
export SCRIPT_CAP="${SCRIPT_CAP:-$((NCELLS * CELL_SECS))}" FIO_GRACE="$VERIFY_GRACE"
. "$here/lib.sh"
harness_init m15-fio
JOBDIR="$REPO/bench/phase1/exit/fio"
say "# M15 fio verify ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus; $NCELLS cells, profiles: $PROFILES, runtime $RUNTIME s)"
say "- script cap $SCRIPT_CAP s: run through the wrapper with LUNIX_ROOT_TIMEOUT >= $((SCRIPT_CAP + 600))"
MIB=$((1 << 20))
NPROF=${#PROF[@]}
NEED_MIB=$((NPROF * (PER_PROFILE_MIB + TRIM_MIB) + ZERO_MIB))
[ "$NEED_MIB" -le $((VOL_GIB * 1024 - 1024)) ] || { say "profiles do not fit the ${VOL_GIB} GiB volume ($NEED_MIB MiB)"; exit 2; }
[ "$PER_PROFILE_MIB" -le 12288 ] || { say "PER_PROFILE_MIB above the plan's 12 GiB per job group"; exit 2; }

# fio_env TARGET I QD JOBS: exports the variables of the job files
fio_env() {
  local i=$2 jobs=$4
  export SEED=$((1000 + i)) QD=$3 JOBS=$jobs RUNTIME LOOPS=1000000 VONLY=0
  export PER_JOB=$((PER_PROFILE_MIB * MIB / jobs)) TRIM_SIZE=$((TRIM_MIB * MIB))
  if [ "$1" = raw ]; then
    export DEV="$CUR_DEV" BASE=$((i * PER_PROFILE_MIB * MIB)) TRIM_BASE=$(((NPROF * PER_PROFILE_MIB + i * TRIM_MIB) * MIB))
  else
    export DIR="$MNT/p$i"; mkdir -p "$DIR"
  fi
}
job_file() { if [ "$1" = raw ]; then echo "$JOBDIR/verify-raw.fio"; else echo "$JOBDIR/verify-fs.fio"; fi; }

run_cell() { # target chunk mode
  local tgt=$1 chunk=$2 mode=$3
  local cell="fio-verify/$tgt/c$((chunk / 1024))k/$mode" vol="v$((chunk / 1024))$tgt$mode"
  vol=${vol:0:60}
  local d; d=$(cell_dir "$cell"); local t0=$SECONDS bad=0 notes=""
  say ""; say "## cell $cell"
  cell_begin "$vol" "$VOL_GIB" "$chunk" "$mode"
  if [ "$tgt" != raw ]; then
    mkfs_fs "$tgt" "$CUR_DEV" || { fail "$cell: mkfs"; rec "$cell" fail why=mkfs; return; }
    mount_fs "$tgt" "$CUR_DEV" "$MNT" || { fail "$cell: mount"; rec "$cell" fail why=mount; return; }
  fi
  local i=0 p qd jobs
  for p in "${PROF[@]}"; do
    qd=${p%%:*}; jobs=${p##*:}
    fio_env "$tgt" "$i" "$qd" "$jobs"
    mkdir -p "$d/state-$i"
    # trimcheck region: raw = the tail of the device, file = a file in the profile directory
    local zb=$(((NPROF * (PER_PROFILE_MIB + TRIM_MIB)) * MIB)) tc="$d/trimcheck-$i.json"
    if [ "$tgt" = raw ]; then
      python3 -I "$here/trimcheck.py" raw "$CUR_DEV" "$zb" $((ZERO_MIB * MIB)) "$RUNTIME" "$tc" "$((7 + i))" & TC_PID=$!
    else
      python3 -I "$here/trimcheck.py" file "$DIR" $((ZERO_MIB * MIB)) "$RUNTIME" "$tc" "$((7 + i))" & TC_PID=$!
    fi
    say "- profile $i: QD $qd, $jobs job(s), $PER_PROFILE_MIB MiB, $RUNTIME s"
    local job; job=$(job_file "$tgt")
    fio_run "$cell-p$i" "$RUNTIME" "$job" "--aux-path=$d/state-$i"; local frc=$?
    wait "$TC_PID"; local trc=$?
    local jf="$OUT/raw/fio-${cell//\//_}-p$i.json" errs; errs=$(fio_errors "$jf")
    [ "$frc" = 0 ] && [ "$errs" = 0 ]; expect "$cell p$i: fio rc $frc, job errors $errs (zero verify failures)" $? || bad=1
    [ "$trc" = 0 ]; expect "$cell p$i: trimcheck (discard, write-zeroes) rc $trc" $? || bad=1
    [ "$frc" = 0 ] && [ "$errs" = 0 ] && [ "$trc" = 0 ] || bad=1
    if [ "$bad" != 0 ]; then notes="$notes;p$i"; break; fi
    i=$((i + 1))
  done
  local profiles_done=$i
  # phase 2: stop, check, detach, attach, verify again
  if [ "$tgt" != raw ]; then
    umount_fs "$MNT"; guest_fsck "$tgt" "$CUR_DEV"; expect "$cell: guest fsck clean after the runs" $? || bad=1
  fi
  status_json "$vol" > "$d/status-1.json" 2>/dev/null
  cell_end "$vol" "$(basename "$d")-1" || bad=1
  local sb1; sb1=$(cat "$OUT/raw/store-bytes-$(basename "$d")-1.txt")
  if [ "$bad" = 0 ]; then
    attach_vol "$vol"; expect "$cell: attach again" $? || bad=1
    CUR_DEV=$(dev_of "$vol"); register_id "${CUR_DEV#/dev/ublkb}"
    [ "$tgt" = raw ] || mount_fs "$tgt" "$CUR_DEV" "$MNT" || bad=1
    i=0
    for p in "${PROF[@]:0:$profiles_done}"; do
      qd=${p%%:*}; jobs=${p##*:}
      fio_env "$tgt" "$i" "$qd" "$jobs"
      local job; job=$(job_file "$tgt")
      VONLY=1 LOOPS=1 fio_run "$cell-v$i" "$RUNTIME" "$job" "--aux-path=$d/state-$i" --section=main; local vrc=$?
      local vjf="$OUT/raw/fio-${cell//\//_}-v$i.json" verrs; verrs=$(fio_errors "$vjf")
      [ "$vrc" = 0 ] && [ "$verrs" = 0 ]; expect "$cell p$i: verify_only after detach/attach: rc $vrc, errors $verrs" $? || bad=1
      i=$((i + 1))
    done
    [ "$tgt" = raw ] || { umount_fs "$MNT"; guest_fsck "$tgt" "$CUR_DEV"; expect "$cell: guest fsck clean after verify_only" $? || bad=1; }
    status_json "$vol" > "$d/status-2.json" 2>/dev/null
    detach_vol "$vol"; expect "$cell: detach" $? || bad=1
    lunix_fsck "$vol" "$(basename "$d")-2"; expect "$cell: lunix fsck --deep clean (exit $FSCK_RC)" $? || bad=1
  fi
  local sb2; sb2=$(store_bytes "$vol")
  local st=pass; [ "$bad" = 0 ] || st=fail
  rec "$cell" "$st" target="$tgt" chunk="$chunk" mode="$mode" profiles="$profiles_done/$NPROF" runtime="$RUNTIME" \
      secs="$((SECONDS - t0))" store_bytes_after_runs="$sb1" store_bytes_final="${sb2:-}" notes="$notes"
  [ "$bad" = 0 ] || FAIL=1
}

for chunk in "${_c[@]}"; do for mode in "${_m[@]}"; do for tgt in "${_t[@]}"; do
  run_cell "$tgt" "$chunk" "$mode"
  [ -e "$OUT/WEDGED" ] && break 3
done; done; done
say ""; say "## end"
[ "$(wc -l < "$RES")" = "$NCELLS" ]; expect "every cell produced a result record ($(wc -l < "$RES") of $NCELLS)" $? || FAIL=1
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
