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
: "${CACHE_MAX:=48G}" "${SAMPLE_S:=10}" "${TRIM_MIB:=256}" "${ZERO_MIB:=1024}" "${VERIFY_GRACE:=900}"
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
  # ADR-32 (research note 2026-11-18, section 9.4): the cell runs with `cache_max` set (default 48G, 150 % of the 32 GiB exit volume;
  # run the 4 GiB-profile raw cell also with CACHE_MAX=24G so that the band is exercised). The store directory is sampled every
  # SAMPLE_S seconds and must stay under cache_max + the headroom reserve + the journal ring + the dirty commit bytes; no write may
  # return ENOSPC; fio's longest completion must be under full_timeout; the kernel log must have no hung-task line.
  lx space "$vol" --cache-max "$CACHE_MAX" --json > "$d/space-set.json" 2>&1
  local cmax hdr fto vdir; cmax=$(jq -r '.settings.cache_max // empty' "$d/space-set.json" 2>/dev/null); hdr=$(jq -r '.settings.headroom // 268435456' "$d/space-set.json" 2>/dev/null)
  fto=$(jq -r '.settings.full_timeout_ms // 30000' "$d/space-set.json" 2>/dev/null); vdir=$(vol_dir "$vol")
  [ -n "$cmax" ] || cmax=$(numfmt --from=iec "$CACHE_MAX")
  local hung0; hung0=$(dmesg | grep -ciE 'hung task|blocked for more than')
  : > "$d/store-samples.txt"
  ( while :; do du -s --block-size=1 "$vdir" 2>/dev/null | cut -f1 >> "$d/store-samples.txt"; sleep "$SAMPLE_S"; done ) </dev/null >/dev/null 2>&1 &
  SAMPLER_PID=$!
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
    # GC runs on its own (M17.B, M12c): record the collector state and the space ledger after every profile; the store of a
    # cell must stay below STORE_CAP_GIB (the plan's 100 GiB for the whole of M15) or the cell stops and fails
    status_json "$vol" > "$d/status-p$i.json" 2>/dev/null; lx gc "$vol" --status --json > "$d/gc-status-p$i.json" 2>&1
    local sbx; sbx=$(store_bytes "$vol")
    say "- p$i: store $((sbx >> 30)) GiB, space used $(jq -r '(.space.used // 0) / 1073741824 | floor' "$d/status-p$i.json" 2>/dev/null) GiB, gc $(jq -c '{state,condemned_pending,last_pass}' "$d/gc-status-p$i.json" 2>/dev/null | cut -c1-160)"
    if [ $((sbx >> 30)) -gt "${STORE_CAP_GIB:-80}" ]; then fail "$cell p$i: the store is above ${STORE_CAP_GIB:-80} GiB (GC is not keeping up)"; bad=1; notes="$notes;store-cap p$i"; break; fi
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
    # the store bound of the gate holds after a final pass and its grace: one explicit pass, FINAL_SETTLE seconds (default 120, past the
    # 60 s grace), then the collector state, the space ledger and the store size of the settled volume
    lx gc "$vol" --json > "$d/gc-final-pass.json" 2>&1
    sleep "${FINAL_SETTLE:-120}"
    lx gc "$vol" --status --json > "$d/gc-status-final.json" 2>&1
    sbs=$(store_bytes "$vol")
    status_json "$vol" > "$d/status-2.json" 2>/dev/null
    detach_vol "$vol"; expect "$cell: detach" $? || bad=1
    lunix_fsck "$vol" "$(basename "$d")-2"; expect "$cell: lunix fsck --deep clean (exit $FSCK_RC)" $? || bad=1
  fi
  local sb2; sb2=$(store_bytes "$vol")
  kill "$SAMPLER_PID" 2>/dev/null; wait "$SAMPLER_PID" 2>/dev/null
  local smax bound lat_ms hung1 enospc
  smax=$(sort -n "$d/store-samples.txt" | tail -1); smax=${smax:-0}
  # the dirty commit bytes are not in the status; the commit interval's worth of map dirt is small, 1 GiB is allowed for it and the journal ring
  bound=$((cmax + hdr + JOURNAL_MAX + (1 << 30)))
  [ "$smax" -le "$bound" ]; expect "$cell: the store directory stayed under cache_max + reserve + journal ring + 1 GiB ($((smax >> 20)) MiB of $((bound >> 20)) MiB)" $? || bad=1
  lat_ms=$(cat "$OUT"/raw/fio-"${cell//\//_}"-p*.json 2>/dev/null | sed -n '/"fio version"/,$p' | jq -s '[.[]|.jobs[]|(.read.lat_ns.max // 0), (.write.lat_ns.max // 0)] | max / 1000000 | floor' 2>/dev/null)
  lat_ms=${lat_ms:-0}
  [ "$lat_ms" -lt "$fto" ]; expect "$cell: fio's longest completion $lat_ms ms is under full_timeout ($fto ms)" $? || bad=1
  enospc=$(grep -l 'No space left' "$OUT"/raw/fio-"${cell//\//_}"-*.stdout "$OUT"/raw/fio-"${cell//\//_}"-*.json 2>/dev/null | wc -l)
  [ "$enospc" = 0 ]; expect "$cell: no write returned ENOSPC" $? || bad=1
  hung1=$(dmesg | grep -ciE 'hung task|blocked for more than')
  [ "$hung1" = "$hung0" ]; expect "$cell: no hung-task line in the kernel log" $? || bad=1
  local thr dly dmx
  thr=$(jq -r '[.. | .delayed_ms? // empty] | first // "n/a"' "$d/status-2.json" 2>/dev/null); dly=$(jq -r '[.. | .delayed_writes? // empty] | first // "n/a"' "$d/status-2.json" 2>/dev/null)
  dmx=$(jq -r '[.. | .delay_max_ms? // empty] | first // "n/a"' "$d/status-2.json" 2>/dev/null)
  local ens; ens=$(jq -r '[.. | .enospc? // empty] | first // 0' "$d/status-2.json" 2>/dev/null)
  [ "${ens:-0}" = 0 ]; expect "$cell: the gate refused no reservation for space (enospc counter $ens)" $? || bad=1
  say "- $cell: throttled (delayed_ms) $thr, delayed_writes $dly, delay_max_ms $dmx, cache_max $CACHE_MAX"
  local st=pass; [ "$bad" = 0 ] || st=fail
  rec "$cell" "$st" target="$tgt" chunk="$chunk" mode="$mode" profiles="$profiles_done/$NPROF" runtime="$RUNTIME" \
      secs="$((SECONDS - t0))" store_bytes_after_runs="$sb1" store_bytes_final="${sb2:-}" store_bytes_settled="${sbs:-}" cache_max="$CACHE_MAX" store_max_sampled="$smax" store_bound="$bound" fio_max_lat_ms="$lat_ms" throttled_ms="$thr" delayed_writes="$dly" delay_max_ms="$dmx" notes="$notes"
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
