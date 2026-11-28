#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 3(a) and 3(c): random-time kill -9 loops and the QUIESCE handover loop.
#
#   MODE=kill      kill -9 of lunixd at random times (INT_MIN..INT_MAX s) under fio verify (backlog) plus two
#                  fsyncers and a heartbeat; restart delays 0 / 1 / 10 s and double kills (a second kill inside the
#                  recovery window). Default: ext4 100 cycles (PHASES_EXT4), xfs 50 cycles (PHASES_XFS).
#   MODE=quiesce   QUIESCE handover x20 via SIGUSR2 (the daemon quiesces every device and exits by itself; a new
#                  daemon recovers them); it is never killed.
#   FS=ext4|xfs  CHUNK=65536|16384  VOLMODE=integrity|encrypted
# Per phase: mount, load, cycles, stop the load, umount, guest fsck, remount, verify acknowledged fsyncs, umount,
# detach, `lunix fsck --deep`, attach. After every cycle the load must still be alive (fio ends at the first verify
# failure, fsyncer on the first I/O error). Stall times: stall.py joins the heartbeat gaps with the cycles
# (p50/p99/max); the daemon's own recovery figure is the `stall_ms` it logs.
#
# Runtime: kill, ext4: about 100 cycles x (12.5 s + restart) + delays, about 40 min; xfs half that; quiesce 20 cycles
# about 10 min. Disk: a 32 GiB volume, the store stays below 15 GiB (GC on). The script cap is stated below.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd LUNIX=<abs>/lunix \
#       TESTD=<abs>/lunix-ublk-testd FS=ext4 MODE=kill bash tests/exit/crash-random.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${FS:=ext4}" "${MODE:=kill}" "${CHUNK:=65536}" "${VOLMODE:=integrity}" "${VOL_GIB:=32}" "${SEED:=7}"
: "${INT_MIN:=5}" "${INT_MAX:=20}" "${FIO_SIZE:=1073741824}" "${FIO_JOBS:=4}"
if [ "$FS" = ext4 ]; then : "${PHASES:=immediate:50:0 delay1s:25:1 delay10s:10:10 double:15:0}"; else : "${PHASES:=immediate:25:0 delay1s:12:1 delay10s:5:10 double:8:0}"; fi
[ "$MODE" = quiesce ] && PHASES="handover:${CYCLES:-20}:0"
export SCRIPT_CAP="${SCRIPT_CAP:-14400}"
. "$here/lib.sh"; . "$here/crashlib.sh"
harness_init "m15-crash-$MODE-$FS"
say "# M15 crash loop MODE=$MODE FS=$FS chunk=$CHUNK $VOLMODE ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus)"
say "- phases: $PHASES; kills every $INT_MIN-$INT_MAX s; script cap $SCRIPT_CAP s"
VOL="crash$FS"
cell_begin "$VOL" "$VOL_GIB" "$CHUNK" "$VOLMODE"
mkfs_fs "$FS" "$CUR_DEV" || wedged "mkfs failed"
echo "phase,cycles,fio_rc,fio_errors,fsck_rc,verify_rc,lunix_fsck_rc,notes" > "$OUT/phases.csv"
RC_ALL=0
now() { date +%s%N; }

kill_daemon() { # kill -9 the serving daemon (that is the test); never any other process
  local old=$DPID; kill -KILL "$old" 2>/dev/null; wait "$old" 2>/dev/null; DPID=""
}
handover() { # SIGUSR2: quiesce and exit; never killed; a daemon that does not exit in 60 s is a wedge
  local old=$DPID
  kill -USR2 "$old"
  wait_daemon_exit 60 || wedged "lunixd $old did not exit within 60 s of SIGUSR2 (quiesce); nothing killed"
  [ "$DRC" = 0 ] || { say "- handover exit status $DRC"; NOTES="$NOTES;handover-exit-$DRC"; }
}
run_phase() { # name count delay
  local name=$1 count=$2 delay=$3 d="$OUT/raw/phase-$1"; mkdir -p "$d"; NOTES=""
  say ""; say "## phase $name: $count cycles, restart delay $delay s"
  mount_fs "$FS" "$CUR_DEV" "$MNT" || wedged "mount failed"
  RANDOM=$SEED
  local ints=() total=0 i v
  for ((i = 0; i < count; i++)); do v=$((INT_MIN + RANDOM % (INT_MAX - INT_MIN + 1))); ints+=("$v"); total=$((total + v + delay + 6)); done
  local rt=$((total + 60))
  wl_start "$name" "$FS" "$rt" "$FIO_SIZE" "$FIO_JOBS"
  sleep 5
  echo "phase,cycle,t_kill_ns,t_ready_ns,daemon_ms" > "$WL_DIR/cycles.csv"
  local cyc=0 aborted="" iv tk tr
  for iv in "${ints[@]}"; do
    cyc=$((cyc + 1))
    sleep "$iv"
    wl_alive || { aborted="load ended early before cycle $cyc"; break; }
    tk=$(now)
    if [ "$MODE" = quiesce ]; then handover; else kill_daemon; fi
    if [ "$name" = double ]; then
      # second kill inside the recovery window: start a recovery, kill it shortly after
      env -u LUNIX_CRASH_AT taskset -c "$DAEMON_CPUS" "$LUNIXD" --config "$ST/lunixd.toml" > "$OUT/lunixd.dk$cyc.log" 2>&1 &
      local kp=$!; sleep "0.$((RANDOM % 4 + 1))"; kill -KILL "$kp" 2>/dev/null; wait "$kp" 2>/dev/null
    fi
    [ "$delay" != 0 ] && sleep "$delay"
    start_lunixd || { aborted="lunixd did not start at cycle $cyc"; break; }
    wait_recovered "$DLOG" 120 || { aborted="no recovery logged at cycle $cyc"; break; }
    tr=$(now)
    echo "$name,$cyc,$tk,$tr,$(daemon_ms "$DLOG")" >> "$WL_DIR/cycles.csv"
    sleep 2
    wl_alive || { aborted="load ended after the restart of cycle $cyc (see raw/fio-wl-$name.*)"; break; }
  done
  [ -n "$aborted" ] && { say "- ABORTED: $aborted"; echo "$aborted" > "$d/aborted.txt"; }
  daemon_alive || start_lunixd || wedged "no daemon after the cycles"
  # the load runs to its own end (rt): wait for it with a bound
  wl_finish "$FS" $((rt + 600))
  local lrc=0
  phase_detach_check "$FS" "$name" || lrc=1
  local fl=$FSCK_RC
  wl_ok && [ -z "$aborted" ] && [ "$lrc" = 0 ]; local ok=$?
  expect "$name: $cyc cycles, fio rc $WL_FIO_RC errors $WL_FIO_ERRS, guest fsck $WL_FSCK_RC, acknowledged fsyncs verify $WL_VERIFY_RC, lunix fsck $fl" $ok || RC_ALL=1
  echo "$name,$cyc,$WL_FIO_RC,$WL_FIO_ERRS,$WL_FSCK_RC,$WL_VERIFY_RC,$fl,$aborted$NOTES" >> "$OUT/phases.csv"
  rec "crash-$MODE/$FS/c$((CHUNK / 1024))k/$VOLMODE/$name" "$([ $ok = 0 ] && echo pass || echo fail)" cycles="$cyc" fio_rc="$WL_FIO_RC" \
      fio_errors="$WL_FIO_ERRS" guest_fsck="$WL_FSCK_RC" fsyncer_verify="$WL_VERIFY_RC" lunix_fsck="$fl" aborted="$aborted" notes="$NOTES"
}
for ph in $PHASES; do IFS=: read -r n c dl <<< "$ph"; run_phase "$n" "$c" "$dl"; done
SD=(); for ph in $PHASES; do SD+=("$OUT/raw/${ph%%:*}"); done
python3 -I "$ROOTDIR/stall.py" "${SD[@]}" > "$OUT/stall-table.md" 2>/dev/null
# stall.py reads <dir>/cycles.csv and hb.csv: the workload directories are raw/<phase name>
say ""; say "## stall times"; tee -a "$R" < "$OUT/stall-table.md"
grep -h 'stall_ms=' "$OUT"/lunixd.*.log | grep -o 'stall_ms=[0-9]*' | cut -d= -f2 | sort -n > "$OUT/daemon-stall-ms.txt"
say "- daemon-logged recovery stall_ms (sorted) in daemon-stall-ms.txt: $(wc -l < "$OUT/daemon-stall-ms.txt") values"
cell_end "$VOL" final || RC_ALL=1
say "overall FAIL=$RC_ALL"; echo "$RC_ALL" > "$OUT/result.txt"
exit "$RC_ALL"
