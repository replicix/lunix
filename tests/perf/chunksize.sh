#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M16 Part B: the ADR-5 chunk-size benchmark. For each workload and each chunk size (16 KiB and 64 KiB), on a fresh 48 GiB
# volume (same size, `--id-len 16`, `auto` compression, unencrypted = integrity mode, journal_max 1 GiB, GC on with the
# default retention), the volume is recreated between cells and df-guard runs before every mkfs:
#
#   randmix   fio random write mix on a 20 GiB file on ext4: bssplit 4k/8k/16k/64k, QD 32, 30 min, 30 % duplicates, 50 % compressible
#   seqrand   fio sequential fill of a 16 GiB file, then a 15 min random overwrite pass
#   pgbench   PostgreSQL (version recorded), scale 1000 (about 15 GB), -c 16 -j 4 -T 1800, default full_page_writes, 8 KiB pages;
#             on ext4 and on xfs; TPS, latency percentiles from the transaction log
#   vm        three raw cloud images (IMAGES="a.raw b.raw c.raw": two adjacent releases and one other distribution, supplied by
#             the lead) copied to the volume, then in a chroot on a loop-mounted copy of each: apt-get update, upgrade and the
#             install of packages.txt (>= 200 packages), to model image churn
#
# Per cell, besides the workload's own figures (throughput, latency percentiles): status before and after (logical bytes
# written, `cold_rmw`, ops, chunk and dedup counters, the space ledger with the map bytes and the journal use), daemon CPU
# seconds over the workload (utime+stime of the process, the queue threads and workers included), resident set, then
# the final garbage collection (GC_PASSES passes GC_WAIT s apart: the grace is 60 s by default), the detached volume's
# `lunix fsck --deep` recount (live data, map pack bytes per category, dedup and compression) and the on-disk store size
# after the final GC. bench/phase1/chunksize/analyze.py turns that into the table of the plan and applies the
# advisory rule. The long cells run once (REPS=1, the noise caveat is stated); REPS_<WORKLOAD> overrides.
#
# Runtime: randmix 30 min + 10 min overhead, seqrand 25 min, pgbench init about 15 min + 30 min + GC, per cell about 1 h;
# vm about 1 h per cell: 2 chunk sizes x (1 + 1 + 2 + 1) cells = about 10 h. Disk: peak below 100 GiB (volume 48 GiB
# virtual; the store of pgbench scale 1000 is about 15 GB plus WAL and dead data before the final GC).
#   tools/remote.sh root env LUNIX_ROOT_TESTS=1 LUNIX_HOST_QUIET=1 LUNIX_SPIKE_DIR=<abs scratch> LUNIXD=... LUNIX=... TESTD=... \
#       [WORKLOADS="randmix seqrand pgbench vm"] [CHUNKS="16384 65536"] [IMAGES="..."] bash tests/perf/chunksize.sh
here="$(cd "$(dirname "$0")" && pwd)"
: "${CHUNKS:=65536 16384}" "${WORKLOADS:=randmix seqrand pgbench vm}" "${VOL_GIB:=48}" "${VOLMODE:=integrity}" "${QUEUES:=4}"
: "${DAEMON_CPUS:=0-7}" "${WL_CPUS:=8-15}" "${QCPUS:=0-3}" "${PG_SCALE:=1000}" "${PG_TIME:=1800}" "${PG_CLIENTS:=16}" "${PG_JOBS:=4}"
: "${PG_SHARED_BUFFERS:=2GB}" "${PG_PORT:=5433}" "${RM_RUNTIME:=1800}" "${RM_SIZE_GIB:=20}" "${SR_RUNTIME:=900}" "${SR_SIZE_GIB:=16}"
: "${GC_PASSES:=3}" "${GC_WAIT:=75}" "${IMAGES:=}" "${VM_STEP_CAP:=7200}" "${FS_PGBENCH:=ext4 xfs}" "${REPS:=1}" "${VOL_FS:=ext4}"
export JOURNAL_MAX="${JOURNAL_MAX:-1073741824}" DAEMON_CPUS FIO_CPUS="$WL_CPUS" QUEUES
read -ra _c <<< "$CHUNKS"; read -ra _w <<< "$WORKLOADS"
export SCRIPT_CAP="${SCRIPT_CAP:-$((${#_c[@]} * ${#_w[@]} * 12000 + 7200))}"
. "$here/lib.sh"; . "$here/perflib.sh"
harness_init m16-chunksize
BENCH="$REPO/bench/phase1/chunksize"
say "# M16 Part B ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus): workloads $WORKLOADS, chunks $CHUNKS, $VOL_GIB GiB volumes, queues $QUEUES"
say "- daemon CPUs $DAEMON_CPUS (queue threads $QCPUS), workload CPUs $WL_CPUS; script cap $SCRIPT_CAP s"
NOISY=0; quiet_check
df_guard "$LUNIX_SPIKE_DIR" "${DF_GIB:-60}" || exit 2
PGSOCK="$LUNIX_SPIKE_DIR/pgsock"
# PostgreSQL refuses to run as root and the `postgres` user cannot traverse the home directory that holds the scratch
# directory; the server and pgbench run as the owner of the scratch directory (PG_USER).
: "${PG_USER:=$(stat -c %U "$LUNIX_SPIKE_DIR")}"
[ "$PG_USER" != root ] || PG_USER=postgres   # a scratch directory made by root (dry runs under /tmp)
chunksize_pre_teardown() { # postgres and everything mounted on top of the device (the daemon serves it while this runs)
  [ -n "${PGBIN:-}" ] && [ -d "${PGDATA:-/nonexistent}" ] && runuser -u "$PG_USER" -- "$PGBIN/pg_ctl" -D "$PGDATA" -m fast -w stop
  vm_unmount_all
}
PRE_TEARDOWN=chunksize_pre_teardown
VM_MOUNTS=(); VM_LOOPS=()
vm_unmount_all() {
  local i
  for ((i = ${#VM_MOUNTS[@]} - 1; i >= 0; i--)); do umount "${VM_MOUNTS[$i]}" 2>/dev/null; done; VM_MOUNTS=()
  for i in "${VM_LOOPS[@]}"; do losetup -d "$i" 2>/dev/null; done; VM_LOOPS=()
}

# ---- workloads: run with the file system mounted at $MNT; they leave their figures in $D ------------------------
wl_randmix() { # D
  DIR="$MNT" SIZE="${RM_SIZE_GIB}g" RUNTIME=$RM_RUNTIME CPUS=$WL_CPUS SEED=11 FIO_GRACE=900 fio_run "$CELL-randmix" "$RM_RUNTIME" "$BENCH/fio/randmix.fio"; local rc=$?
  cp "$OUT/raw/fio-${CELL//\//_}-randmix.json" "$1/fio.json" 2>/dev/null; return $rc
}
wl_seqrand() {
  DIR="$MNT" SIZE="${SR_SIZE_GIB}g" RUNTIME=$SR_RUNTIME CPUS=$WL_CPUS SEED=12 FIO_GRACE=1800 fio_run "$CELL-seqrand" $((SR_RUNTIME + 600)) "$BENCH/fio/seqrand.fio"; local rc=$?
  cp "$OUT/raw/fio-${CELL//\//_}-seqrand.json" "$1/fio.json" 2>/dev/null; return $rc
}
pgb() { runuser -u "$PG_USER" -- "$PGBIN/$1" -h "$PGSOCK" -p "$PG_PORT" "${@:2}"; }
wl_pgbench() {
  local d=$1
  export LC_ALL=C.UTF-8 LANG=C.UTF-8   # the host may not have the login locale generated; the data is not locale sensitive
  [ -n "${PGBIN:-}" ] || PGBIN=$(ls -d /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -1)   # Debian layout
  [ -n "$PGBIN" ] || { [ -x /usr/bin/initdb ] && PGBIN=/usr/bin; }   # distro packages that put the server in /usr/bin
  [ -x "$PGBIN/initdb" ] || { echo "no PostgreSQL server installed" > "$d/skipped.txt"; return 3; }
  PGDATA="$MNT/pgdata"; mkdir -p "$PGSOCK" "$d/pg"; chown "$PG_USER":"$PG_USER" "$PGSOCK" "$d/pg"
  install -d -o "$PG_USER" -m 700 "$PGDATA"
  "$PGBIN/postgres" --version > "$d/pg-version.txt"
  runuser -u "$PG_USER" -- "$PGBIN/initdb" -D "$PGDATA" -A trust --locale=C.UTF-8 -E UTF8 > "$d/initdb.txt" 2>&1 || return 1
  cat >> "$PGDATA/postgresql.conf" <<PGCONF
listen_addresses = ''
unix_socket_directories = '$PGSOCK'
port = $PG_PORT
shared_buffers = $PG_SHARED_BUFFERS
max_connections = 50
logging_collector = off
PGCONF
  runuser -u "$PG_USER" -- taskset -c "$WL_CPUS" "$PGBIN/pg_ctl" -D "$PGDATA" -l "$d/pg/server.log" -w start > "$d/pg-start.txt" 2>&1 || return 1
  pgb createdb pgb || return 1
  local t0=$SECONDS
  taskset -c "$WL_CPUS" runuser -u "$PG_USER" -- "$PGBIN/pgbench" -h "$PGSOCK" -p "$PG_PORT" -i -s "$PG_SCALE" -q pgb > "$d/pgbench-init.txt" 2>&1 || return 1
  echo "init_seconds=$((SECONDS - t0))" > "$d/pg-timing.txt"
  # the measured run starts from the freshly initialised database; the daemon's counters are sampled around it
  chown "$PG_USER":"$PG_USER" "$d/pg"
  taskset -c "$WL_CPUS" timeout -k 60 $((PG_TIME + 900)) runuser -u "$PG_USER" -- "$PGBIN/pgbench" -h "$PGSOCK" -p "$PG_PORT" -c "$PG_CLIENTS" -j "$PG_JOBS" \
      -T "$PG_TIME" -P 30 -r -l --log-prefix="$d/pg/txlog" --sampling-rate=0.1 pgb > "$d/pgbench-run.txt" 2>&1; local rc=$?
  python3 -I "$BENCH/pglat.py" "$d/pg/txlog" 0.1 > "$d/pglat.json" 2>/dev/null
  runuser -u "$PG_USER" -- "$PGBIN/pg_ctl" -D "$PGDATA" -m fast -w stop > "$d/pg-stop.txt" 2>&1
  return $rc
}
# vm: the images are copied to the volume, then churned in chroots on loop-mounted copies
wl_vm() {
  local d=$1 i=0 img rc=0
  [ -n "$IMAGES" ] || { echo "IMAGES is not set (the lead supplies the three raw images)" > "$d/skipped.txt"; return 3; }
  mkdir -p "$MNT/images"
  local t0=$SECONDS
  for img in $IMAGES; do cp --sparse=always "$img" "$MNT/images/img$i.raw" || return 1; i=$((i + 1)); done
  echo "copy_seconds=$((SECONDS - t0))" > "$d/vm-timing.txt"; sync
  i=0
  for img in $IMAGES; do
    vm_churn "$d" "$i" "$img" || { rc=1; say "- vm image $i ($img): churn failed"; }
    i=$((i + 1))
  done
  return $rc
}
vm_churn() { # d i name
  local d=$1 i=$2 f="$MNT/images/img$2.raw" root="$LUNIX_SPIKE_DIR/mnt/$NAME-img$2"
  need_safe "$root"; mkdir -p "$root"
  [ "${VM_GROW_GIB:-6}" -gt 0 ] && truncate -s "+${VM_GROW_GIB:-6}G" "$f"
  local loop part k; loop=$(losetup -fP --show "$f") || return 1; VM_LOOPS+=("$loop"); register_device "$loop"
  # the partition nodes of a loop device appear a moment after losetup -P (udev): wait for them (up to 20 s) before the
  # largest ext4 or xfs partition is chosen; an image without a partition table is mounted whole
  udevadm settle --timeout=20 2>/dev/null
  for k in $(seq 1 40); do
    part=$(lsblk -nrpo NAME,FSTYPE,SIZE,TYPE -b "$loop" | awk '$4 == "part" && $2 ~ /ext4|xfs/ {print $3, $1}' | sort -nr | head -1 | cut -d' ' -f2)
    [ -n "$part" ] && break
    [ -z "$(blkid -p -o value -s PTTYPE "$loop" 2>/dev/null)" ] && break   # no partition table: the whole device
    sleep 0.5
  done
  part=${part:-$loop}
  # the cloud images' root file systems are nearly full (2-3 GiB): grow the copy on the volume (sparse, so the volume only
  # stores what is written) and its root partition (the last one, number 1) and ext4 file system before the churn
  if [ "$part" != "$loop" ] && [ "${VM_GROW_GIB:-6}" -gt 0 ]; then
    { growpart "$loop" 1 && udevadm settle --timeout=20 && e2fsck -fy "$part"; resize2fs "$part"; } > "$d/vm-img$i-grow.txt" 2>&1 || true
  fi
  # the chroot work runs in a private mount namespace (vm-chroot.sh): nothing it mounts propagates to the host's other mount
  # namespaces, and everything is gone when it exits; only then is the loop device released
  local t0=$SECONDS out rc
  out=$(unshare --mount --propagation private bash "$here/vm-chroot.sh" "$part" "$root" "$LUNIX_SPIKE_DIR/apt-cache-img$i" "$BENCH/packages.txt" "$VM_STEP_CAP" "$d/vm-img$i-apt.log"); rc=$?
  echo "$out" > "$d/vm-img$i-chroot.txt"
  grep -o 'pkgs_before=.*' <<< "$out" | cut -d= -f2 > "$d/vm-img$i-pkgs-before.txt"
  grep -o 'pkgs_after=.*' <<< "$out" | cut -d= -f2 > "$d/vm-img$i-pkgs-after.txt"
  local arc; arc=$(grep -o 'apt_rc=.*' <<< "$out" | cut -d= -f2); [ -n "$arc" ] || arc=$rc
  echo "apt_seconds=$((SECONDS - t0)) rc=$arc" >> "$d/vm-timing.txt"
  sync; vm_unmount_all
  rc=$arc; [ "${rc:-1}" = 0 ] || rc=1
  return $rc
}

# gc_idle NAME FILE: waits (at most GC_IDLE_CAP s, default 600) until the collector reports no pass in progress
# (`lunix gc --status --json`: `.state == null`); the last status is kept in FILE.
gc_idle() {
  local t=$SECONDS
  while :; do
    lx gc "$1" --status --json > "$2" 2>&1
    jq -e '.state == null' "$2" >/dev/null 2>&1 && return 0
    [ $((SECONDS - t)) -lt "${GC_IDLE_CAP:-600}" ] || { say "- WARN gc still running after ${GC_IDLE_CAP:-600} s ($1)"; return 1; }
    sleep 5
  done
}

# ---- one cell ------------------------------------------------------------------------------------------------------
run_cell() { # workload chunk fs rep
  local w=$1 chunk=$2 fs=$3 rep=$4 ck=$(($2 / 1024))
  CELL="chunk/$w/c${ck}k/$fs/rep$rep"; local vol="cs${w:0:6}${ck}${fs}$rep"; vol=${vol:0:60}
  local D; D=$(cell_dir "$CELL"); local t0=$SECONDS
  say ""; say "## $CELL"
  CUR_VOL=$vol; CUR_MODE=$VOLMODE
  cell_reset; start_lunixd || wedged "lunixd did not start"
  mkvol "$vol" $((VOL_GIB * GIB)) "$chunk" "$VOLMODE"; expect "create $vol" $? || return
  attach_pinned "$vol" "$QUEUES" "$QCPUS"; expect "attach ($QUEUES queues)" $? || return
  CUR_DEV=$(dev_of "$vol"); register_id "${CUR_DEV#/dev/ublkb}"; [ -b "$CUR_DEV" ] || wedged "no device"
  mkfs_fs "$fs" "$CUR_DEV" || { fail "$CELL: mkfs"; return; }
  mount_fs "$fs" "$CUR_DEV" "$MNT" || { fail "$CELL: mount"; return; }
  sleep 3; snap "$vol" "$D/status-0.json"
  local rss0 c0 c1; rss0=$(rss_kb "$DPID"); c0=$(cpu_secs "$DPID"); thread_cpu "$DPID" > "$D/threads-0.txt"
  local wt0=$SECONDS
  # store and GC over time (every STORE_SAMPLE s, default 60): the lead asked for it because GC can fall behind under random
  # overwrite; the cap guard stops the workload (fio, pgbench) if the store passes STORE_CAP_GIB (default 90) of the 100 GiB budget
  local vdir sp; vdir=$(vol_dir "$vol"); : > "$D/store-timeline.tsv"
  ( while sleep "${STORE_SAMPLE:-60}"; do
      sb=$(du -s --block-size=1 "$vdir" 2>/dev/null | cut -f1)
      printf '%s\t%s\t%s\t%s\n' "$((SECONDS - wt0))" "${sb:-}" "$(status_json "$vol" 2>/dev/null | jq -c '[.journal_used, .space.map, .space.cache, .stats.bytes_written]' 2>/dev/null)" \
        "$(lx gc "$vol" --status --json 2>/dev/null | jq -c '.state' 2>/dev/null)" >> "$D/store-timeline.tsv"
      if [ "${sb:-0}" -gt $((${STORE_CAP_GIB:-90} * GIB)) ]; then echo "store ${sb} B over the cap" > "$D/STORE_CAP"; pkill -TERM -x fio; pkill -TERM -x pgbench; fi
    done ) & sp=$!
  "wl_$w" "$D"; local wrc=$?
  kill "$sp" 2>/dev/null; wait "$sp" 2>/dev/null
  local wsecs=$((SECONDS - wt0))
  c1=$(cpu_secs "$DPID"); thread_cpu "$DPID" > "$D/threads-1.txt"
  local rss1 hwm; rss1=$(rss_kb "$DPID"); hwm=$(hwm_kb "$DPID")
  sync; snap "$vol" "$D/status-1.json"
  umount_fs "$MNT"; guest_fsck "$fs" "$CUR_DEV"; expect "$CELL: guest fsck clean" $? || true
  # final garbage collection and its grace, then the exact recount
  local p
  # `lunix gc NAME --json` runs a pass to its end (it waits out the grace itself); `lunix gc NAME --status --json` has
  # `state: null` when the collector is idle: wait for that before the next pass and before the status snapshot.
  for p in $(seq 1 "$GC_PASSES"); do
    lx gc "$vol" --json > "$D/gc-$p.json" 2>&1; gc_idle "$vol" "$D/gc-status-$p.json"
    [ "$p" = "$GC_PASSES" ] || sleep "$GC_WAIT"
  done
  sleep "$GC_WAIT"; snap "$vol" "$D/status-2.json"
  detach_vol "$vol"; expect "$CELL: detach" $? || true
  lunix_fsck "$vol" "$(echo "$CELL" | tr '/' '_')"; local frc=$?
  cp "$OUT/raw/fsck-$(echo "$CELL" | tr '/' '_').json" "$D/fsck.json" 2>/dev/null
  store_bytes "$vol" > "$D/store-bytes.txt"
  expect "$CELL: lunix fsck --deep clean (exit $FSCK_RC)" $frc || true
  local st=info; { [ "$wrc" = 0 ] || [ "$wrc" = 3 ]; } || st=fail; [ "$wrc" = 3 ] && st=skip
  [ "$st" = fail ] && FAIL=1
  say "- $CELL: workload rc $wrc in ${wsecs}s, daemon CPU $(awk -v a="$c0" -v b="$c1" 'BEGIN{printf "%.1f", b-a}') s"
  rec "$CELL" "$st" workload="$w" chunk="$chunk" fs="$fs" rep="$rep" dir="$D" workload_rc="$wrc" workload_secs="$wsecs" daemon_cpu_s="$(awk -v a="$c0" -v b="$c1" 'BEGIN{printf "%.3f", b-a}')" \
      rss_kib_before="$rss0" rss_kib_after="$rss1" hwm_kib="$hwm" fsck_rc="$FSCK_RC" secs="$((SECONDS - t0))" noisy_host="$NOISY"
}
for w in "${_w[@]}"; do
  reps_var="REPS_${w^^}"; reps=${!reps_var:-$REPS}
  fss="ext4"; [ "$w" = pgbench ] && fss="$FS_PGBENCH"
  for chunk in "${_c[@]}"; do for fs in $fss; do for rep in $(seq 1 "$reps"); do
    run_cell "$w" "$chunk" "$fs" "$rep"
    [ -e "$OUT/WEDGED" ] && break 5
  done; done; done
done
say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
