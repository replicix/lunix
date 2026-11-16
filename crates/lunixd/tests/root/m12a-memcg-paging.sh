#!/bin/bash
# M12a root cell P1 (final review F3): the daemon under memory pressure, buffered writes at full speed, the
# hang watchdog as the assertion.
#
# The argument behind "no mlockall" is "latency, not deadlock": a queue or flush thread that faults on a paged
# heap page waits for a free page, and the kernel's dirty limits keep free pages coming. This cell tests it:
# the daemon runs in a memory cgroup whose memory.high is the daemon's locked bytes plus HEAP_MIB, so the rest
# of its heap (overlay, maps, caches) is pushed to swap, while a writer outside the cgroup fills the page
# cache of the device with dirty pages (buffered fio, no rate limit, several jobs).
#
# Assertions (a hang is the failure; slow is not):
#   - the daemon pages: VmSwap of the daemon reaches MIN_SWAP_MIB (otherwise the cell proved nothing: FAIL
#     "not tight enough", rerun with a smaller HEAP_MIB);
#   - the hang watchdog: the device's completed-write counter advances at least once every HANG_S seconds
#     while the writers run; a longer stall is a hang (state is dumped, the run stops as WEDGED, nothing is
#     killed: W9);
#   - no queue thread of the daemon stays in uninterruptible sleep for HANG_S in a row (sampled each second);
#   - the daemon is alive at the end, its OOM kill count (memory.events oom_kill) is 0, and no log line says
#     the engine's watchdog fired;
#   - the writers finish, the volume detaches and `lunixd check` is clean.
# The latency is reported (writes completed per 10 s window, worst stall, swap, major faults, memory.events).
#
# Needs: cgroup v2 with the memory controller, and swap that is NOT on a Lunix device (never create or enable
# swap here: that is a system setting; the cell exits 77 (skipped) when there is none, 2 when swap is on a
# ublk device). Binaries are built beforehand as the normal user. Disk: the volume is VOL_GIB (4) and nothing
# collects garbage yet, so a monitor stops the writers when the volumes directory passes STORE_CAP_GIB (6);
# df-guard asks for 8. Runtime: RUN_S (300) plus start and teardown; the script cap is RUN_S + 900.
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=<abs>/lunixd \
#       TESTD=<abs>/lunix-ublk-testd bash crates/lunixd/tests/root/m12a-memcg-paging.sh
# Knobs (env): RUN_S, HEAP_MIB (heap allowance over the locked bytes, default 48), MIN_SWAP_MIB (8), HANG_S
# (60), JOBS (4), VOL_GIB, STORE_CAP_GIB, DAEMON_CPUS, FIO_CPUS.
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/m12a-lib.sh"
: "${RUN_S:=300}" "${HEAP_MIB:=48}" "${MIN_SWAP_MIB:=8}" "${HANG_S:=60}" "${JOBS:=4}" "${VOL_GIB:=4}" "${STORE_CAP_GIB:=6}"
: "${SCRIPT_CAP:=$((RUN_S + 900))}"
JOURNAL_MAX=536870912
need_root_tests
[ -f /sys/fs/cgroup/cgroup.controllers ] && grep -qw memory /sys/fs/cgroup/cgroup.controllers || skip "no cgroup v2 memory controller"
[ "$(wc -l < /proc/swaps)" -gt 1 ] || skip "no swap on this host (the daemon's heap cannot be paged)"
if grep -q '^/dev/ublk' /proc/swaps; then echo "swap on a ublk device: refusing" >&2; exit 2; fi
harness_init m12a-memcg
CG=/sys/fs/cgroup/lunix-m12a-memcg
df_guard "$LUNIX_SPIKE_DIR" "${MEMCG_GIB:-8}" || exit 2

cg_remove() { # the cgroup holds only our daemon; move stragglers to the root first
  [ -d "$CG" ] || return 0
  local p; for p in $(cat "$CG/cgroup.procs" 2>/dev/null); do echo "$p" > /sys/fs/cgroup/cgroup.procs 2>/dev/null; done
  rmdir "$CG" 2>/dev/null
  true
}
add_cleanup 'cg_remove'
cg_stat() { awk -v k="$1" '$1 == k {print $2}' "$CG/$2"; }  # key file
vm() { awk -v k="$1:" '$1 == k {print $2}' "/proc/$DPID/status"; }  # KiB
wr_done() { awk '{print $5}' "/sys/block/$(basename "$DEV")/stat"; }  # completed writes

say "# M12a memory-cgroup paging cell ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus, run $RUN_S s, heap allowance $HEAP_MIB MiB)"
mkdir "$CG" || exit 2
start_lunixd || exit 1
echo "$DPID" > "$CG/cgroup.procs"; expect "the daemon runs in the memory cgroup" $?
create_vol m $((VOL_GIB << 30)); expect "create volume m ($VOL_GIB GiB)" $?
attach m 4; expect "attach m (4 queues; its locked buffers are charged to the cgroup)" $?
DEV=$(attached_dev m); [ -b "$DEV" ]; expect "device $DEV" $?
VOLDIR=$(vol_dir m)

# Touch the heap first (a short direct-write warm-up), then squeeze: memory.high = locked + HEAP_MIB,
# memory.max a little above it so an overshoot is throttled and reclaimed, not killed.
start_fio warm "$DEV" 0 512m --bs=64k --iodepth=16 --io_size=512m; wait_fio "$LAST_FIO" 300; expect "warm-up write with verify (rc $?)" $?
LCK=$(vm VmLck); HIGH=$(( LCK * 1024 + HEAP_MIB * 1048576 ))
echo "$HIGH" > "$CG/memory.high"; echo $(( HIGH + 64 * 1048576 )) > "$CG/memory.max"
echo max > "$CG/memory.swap.max"
say "- before the squeeze: VmRSS $(( $(vm VmRSS) / 1024 )) MiB, VmLck $(( LCK / 1024 )) MiB, VmSwap $(( $(vm VmSwap) / 1024 )) MiB"
say "- memory.high $(( HIGH / 1048576 )) MiB, memory.max $(( (HIGH + 64 * 1048576) / 1048576 )) MiB"

say ""; say "## P1: $JOBS buffered writers at full speed for $RUN_S s"
PIDS=()
for j in $(seq 0 $((JOBS - 1))); do
  # Buffered (direct=0), no rate limit, sequential 1 MiB writes over the job's own region, repeated for the
  # whole runtime (time_based). Outside the cgroup, so the dirty pages belong to the writer, not the daemon.
  taskset -c "$FIO_CPUS" fio --name="p1j$j" --filename="$DEV" --direct=0 --ioengine=psync --rw=write --bs=1m \
    --offset="$((j * VOL_GIB * 1024 / JOBS))m" --size="$((VOL_GIB * 1024 / JOBS))m" --time_based --runtime="$RUN_S" \
    --end_fsync=1 --output="$OUT/fio-p1j$j.txt" >/dev/null 2>&1 &
  PIDS+=($!); FIO_PIDS+=($!)
done
sleep 2
alive=0; for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive + 1)); done
[ "$alive" = "$JOBS" ]; expect "$JOBS writers running" $?

hang_dump() {
  { echo "## hang dump $(date -u +%T)"; echo "completed writes: $(wr_done)"
    echo "memory.events:"; cat "$CG/memory.events"; echo "memory.pressure:"; cat "$CG/memory.pressure"
    echo "daemon threads (tid state wchan name):"
    local t; for t in /proc/$DPID/task/*; do
      echo "$(basename "$t") $(awk '{print $3}' "$t/stat" 2>/dev/null) $(cat "$t/wchan" 2>/dev/null) $(cat "$t/comm" 2>/dev/null)"
    done; } > "$OUT/hang-dump.txt" 2>&1
}
last=$(wr_done); last_t=$(date +%s); worst=0; dstate=0; win_t=$last_t; win_w=$last; HANG=0; CAP=0; samples=0
: > "$OUT/windows.txt"
while :; do
  alive=0; for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=$((alive + 1)); done
  [ "$alive" = 0 ] && break
  kill -0 "$DPID" 2>/dev/null || { fail "the daemon died during the run"; break; }
  now=$(date +%s); cur=$(wr_done)
  if [ "$cur" != "$last" ]; then
    stall=$((now - last_t)); [ "$stall" -gt "$worst" ] && worst=$stall; last=$cur; last_t=$now
  elif [ $((now - last_t)) -ge "$HANG_S" ]; then
    HANG=1; hang_dump; fail "HANG: no write completed on $DEV for $HANG_S s (see hang-dump.txt)"; break
  fi
  # Uninterruptible sleep of every daemon thread at once for HANG_S samples in a row = a daemon wedged on memory.
  n=$(for t in /proc/$DPID/task/*; do awk '{print $3}' "$t/stat" 2>/dev/null; done | grep -c '^D$')
  if [ "$n" -gt 0 ]; then dstate=$((dstate + 1)); else dstate=0; fi
  if [ "$dstate" -ge "$HANG_S" ]; then HANG=1; hang_dump; fail "HANG: $n daemon thread(s) in D state for $HANG_S s"; break; fi
  if [ $((now - win_t)) -ge 10 ]; then
    echo "$((now - win_t)) s: $((cur - win_w)) writes; VmSwap $(( $(vm VmSwap) / 1024 )) MiB; majflt $(cg_stat pgmajfault memory.stat)" >> "$OUT/windows.txt"
    win_t=$now; win_w=$cur
  fi
  samples=$((samples + 1))
  if [ $((samples % 5)) = 0 ] && [ "$(du -sk "$VOLS" | cut -f1)" -gt $((STORE_CAP_GIB * 1048576)) ]; then
    CAP=1; say "- store passed ${STORE_CAP_GIB} GiB: stopping the writers early (no GC yet)"
    for p in "${PIDS[@]}"; do kill -TERM "$p" 2>/dev/null; done
  fi
  sleep 1
done
[ "$HANG" = 0 ] || wedged "hang watchdog fired (writers and daemon left as they are)"
for p in "${PIDS[@]}"; do wait_fio "$p" 600; rc=$?; [ "$CAP" = 1 ] || expect "writer $p finished (rc $rc)" "$rc"; done
SWAPPED=$(( $(vm VmSwap) / 1024 ))
say "- worst gap between completed writes: $worst s (limit $HANG_S s); VmSwap now $SWAPPED MiB"
say "- memory.events: $(tr '\n' ' ' < "$CG/memory.events")"
say "- pgmajfault $(cg_stat pgmajfault memory.stat), pswpin $(cg_stat pswpin memory.stat 2>/dev/null), pswpout $(cg_stat pswpout memory.stat 2>/dev/null)"
sed 's/^/    window: /' "$OUT/windows.txt" | tee -a "$R" | tail -40
kill -0 "$DPID" 2>/dev/null; expect "the daemon is alive after the run" $?
[ "$(cg_stat oom_kill memory.events)" = 0 ]; expect "no OOM kill in the cgroup" $?
[ "$SWAPPED" -ge "$MIN_SWAP_MIB" ] || [ "$(cg_stat pswpout memory.stat)" -gt 0 ]
expect "the daemon's heap was paged (VmSwap $SWAPPED MiB; the cell is tight enough to mean something)" $?
! grep -q -i 'watchdog' "$DLOG"; expect "no watchdog line in the daemon log" $?
grep -h -i 'watchdog' "$DLOG" | cut -c1-300 | sed 's/^/    log: /' | tee -a "$R"

say ""; say "## end"
detach m; expect "detach m" $?
"$LUNIXD" check --dir "$VOLDIR" > "$OUT/check.txt" 2>&1; expect "lunixd check clean ($(head -1 "$OUT/check.txt"))" $?
stop_lunixd; expect "lunixd exits on SIGTERM" $?
cg_remove
du -sh "$VOLS" | tee -a "$R"
say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
