#!/bin/bash
# M12a root cell: kill -9 of a four-queue lunixd under fio, several cycles, with on_journal_damage = refuse.
# Every new daemon must recover (open) the volume: a torn tail, and records above a gap, are crash residue
# on the journal rule of this branch, and refuse must not keep a crashed volume offline.
#
# Per cycle: fio (four jobs on four regions of the raw device, rate limited, verify with backlog) runs for
# CYCLE_RUN s; after CYCLE_KILL s the daemon is killed with SIGKILL (only that daemon; no LUNIX_CRASH_AT);
# the new daemon must report "volume recovered at start ... recovered=true" and no JournalDamage refusal;
# fio must finish with verify passing across the kill. Disk: about 5.5 GiB peak (no GC yet), df-guard asks for 8.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd \
#       TESTD=<abs>/lunix-ublk-testd bash crates/lunixd/tests/root/m12a-refuse-crash.sh
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/m12a-lib.sh"
CFG_EXTRA='on_journal_damage = "refuse"'
JOURNAL_MAX=536870912
harness_init m12a-refuse
: "${CYCLES:=6}" "${CYCLE_RUN:=24}" "${CYCLE_KILL:=${KILL_AFTER:-9}}"
df_guard "$LUNIX_SPIKE_DIR" "${REFUSE_GIB:-8}" || exit 2
say "# M12a four-queue kill -9 under fio with on_journal_damage = refuse ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus, $CYCLES cycles)"
grep -q '^on_journal_damage = "refuse"' "$ST/lunixd.toml"; expect "config: on_journal_damage = refuse, journal_max $JOURNAL_MAX" $?
start_lunixd || exit 1
create_vol r $((2 << 30)); expect "create volume r (2 GiB)" $?
attach r 4; expect "attach r (4 queues)" $?
DEV=$(attached_dev r); [ -b "$DEV" ]; expect "device $DEV" $?
for c in $(seq 1 "$CYCLES"); do
  say ""; say "## cycle $c"
  PIDS=()
  for j in 0 1 2 3; do
    start_fio "c${c}j$j" "$DEV" "$((j * 512))m" 256m --bs=16k --iodepth=32 --time_based --runtime="$CYCLE_RUN" --rate=8m
    PIDS+=("$LAST_FIO")
  done
  sleep "$CYCLE_KILL"
  alive=0; for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive + 1)); done
  [ "$alive" = 4 ]; expect "cycle $c: four fio jobs running before the kill" $?
  OLD=$DPID
  kill -KILL "$OLD"; wait "$OLD" 2>/dev/null; DPID=""
  say "- kill -9 sent to lunixd $OLD"
  sleep 1
  t0=$(date +%s%N)
  start_lunixd || wedged "lunixd did not restart after the kill (cycle $c)"
  say "- restart to ready: $(( ($(date +%s%N) - t0) / 1000000 )) ms"
  if ! grep -q 'volume recovered at start.*recovered=true' "$DLOG"; then
    # A refused volume: the device is QUIESCED with fio waiting. Record the finding, then recover with wipe
    # (the refusing daemon holds no device: SIGTERM is safe), let fio finish, and tear down.
    fail "cycle $c: the new lunixd did not recover the volume under refuse"
    grep -h -E 'journal recovery stopped|not recovered at start' "$DLOG" | cut -c1-420 | sed 's/^/    log: /' | tee -a "$R"
    stop_lunixd || wedged "the refusing lunixd did not exit"
    sed -i 's/^on_journal_damage = .*/on_journal_damage = "wipe"/' "$ST/lunixd.toml"
    start_lunixd || wedged "lunixd (wipe) did not start"
    grep -q 'volume recovered at start.*recovered=true' "$DLOG"; expect "cycle $c: recovery with wipe brings the device back" $?
    for p in "${PIDS[@]}"; do wait_fio "$p" 180; expect "cycle $c: fio job finished after the wipe recovery (rc $?)" $?; done
    break
  fi
  grep -q 'volume recovered at start.*recovered=true' "$DLOG"; expect "cycle $c: the new lunixd opened and recovered the volume under refuse ($(grep -o 'stall_ms=[0-9]*' "$DLOG" | head -1))" $?
  ! grep -q -i 'journal damage\|JournalDamage\|not recovered at start' "$DLOG"; expect "cycle $c: no journal-damage refusal in the log" $?
  grep -h -E 'volume opened|journal recovery stopped|crash residue' "$DLOG" | cut -c1-420 | sed 's/^/    log: /' | tee -a "$R"
  [ "$(attached_dev r)" = "$DEV" ]; expect "cycle $c: same device after recovery" $?
  for p in "${PIDS[@]}"; do wait_fio "$p" 180; expect "cycle $c: fio job finished with verify passing (rc $?)" $?; done
done
say ""; say "## end"
detach r; expect "detach r" $?
"$LUNIXD" check --dir "$(vol_dir r)" > "$OUT/check.txt" 2>&1; expect "lunixd check clean ($(head -1 "$OUT/check.txt"))" $?
stop_lunixd; expect "lunixd exits on SIGTERM" $?
du -sh "$VOLS" | tee -a "$R"
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
