#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# Shared harness of the M15 exit scripts (source this file; root only, through `tools/remote.sh root`).
#
# Safety rules kept here (plan W1-W9, CONVENTIONS "Safety rules for ublk work"):
#  * every path that is mkfs'd, mounted or written as a block device resolves under $LUNIX_SPIKE_DIR or is a
#    device recorded in created-devices (safe_path); mounts are only under $LUNIX_SPIKE_DIR/mnt, always
#    nosuid,nodev;
#  * df_guard before every mkfs; the volume is deleted and recreated between cells;
#  * no stop command is ever sent: a device is deleted only by `lunix-ublk-testd del --force` (W1a) and only when
#    it was recorded by this run and its daemon is gone; a stopping daemon is never killed (W4); a daemon that is
#    killed on purpose is killed with KILL by the test (that is the crash test itself), never by teardown;
#  * every fio run is bounded (--runtime or --io_size, W3) under an outer `timeout -k`;
#  * teardown order: stop fio, make sure a daemon serves the device, umount, detach, stop the daemon, only then
#    delete devices without a daemon; a wedged step ends the run (W9), nothing is retried.
#
# Environment (all but the first four have defaults):
#   LUNIXD   lunixd binary (plain release)        LUNIX   lunix CLI binary      TESTD  lunix-ublk-testd binary
#   LUNIX_FSCK  optional lunix-fsck binary (used instead of `lunix fsck` when set)
#   LUNIXD_CP   lunixd built with --features crashpoints (crash scripts only)
#   DAEMON_CPUS (12-13)  FIO_CPUS (14-15)  QUEUES (2)  DEPTH (daemon default)  JOURNAL_MAX (536870912)
#   OUT      result directory (default $LUNIX_SPIKE_DIR/<name>-out)
#
# shellcheck disable=SC2034  # variables are used by the scripts that source this file
_lib_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$_lib_here/../.." && pwd)"
# shellcheck source=../../crates/lunix-ublk/tests/root/lib.sh
. "$REPO/crates/lunix-ublk/tests/root/lib.sh"

GIB=$((1 << 30))
: "${DAEMON_CPUS:=12-13}" "${FIO_CPUS:=14-15}" "${QUEUES:=2}" "${JOURNAL_MAX:=536870912}"
: "${COMMIT_INTERVAL_MS:=5000}" "${ORPHAN_CODES:=orphan crash_leftover leftover}"

# harden_ok: 0 when the daemon can lock memory (root has CAP_IPC_LOCK; kept for symmetry with the M12a scripts)
harness_init() { # NAME [state-extra]
  NAME=$1
  require_root; need_root_tests
  : "${LUNIXD:?LUNIXD is not set}" "${LUNIX:?LUNIX is not set}" "${TESTD:?TESTD is not set}"
  : "${OUT:=$LUNIX_SPIKE_DIR/$NAME-out}"
  ST="$LUNIX_SPIKE_DIR/$NAME-state"; VOLS="$LUNIX_SPIKE_DIR/$NAME-vols"
  MNT="$LUNIX_SPIKE_DIR/mnt/$NAME"; KEYS="$LUNIX_SPIKE_DIR/$NAME-keys"
  need_safe "$OUT"; need_safe "$ST"; need_safe "$VOLS"; need_safe "$MNT"; need_safe "$KEYS"
  rm -rf "$OUT"; mkdir -p "$OUT/raw" "$MNT" "$KEYS"
  bash "$REPO/spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; echo "not clean at start"; exit 2; }
  : > "$CREATED_DEVICES"
  script_cap "${SCRIPT_CAP:-3600}"
  lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
  rm -rf "$ST" "$VOLS"; mkdir -p "$ST" "$VOLS"
  SOCK="$ST/ctl.sock"
  R="$OUT/results.md"; : > "$R"; RES="$OUT/results.jsonl"; : > "$RES"
  FAIL=0; DPID=""; FIO_PIDS=(); _dn=0; _cn=0; CUR_VOL=""; CUR_DEV=""; CUR_MODE=integrity; CUR_CHUNK=65536
  # a 32-byte test key for the encrypted cells, made here, only for this run
  head -c 32 /dev/urandom > "$KEYS/enc.key"; chmod 600 "$KEYS/enc.key"
  write_config
  hostinfo > "$OUT/hostinfo.md"
  trap 'teardown >> "$OUT/teardown.log" 2>&1; cleanup' EXIT INT TERM
}
write_config() {
  cat > "$ST/lunixd.toml" <<CFG
state_dir = "$ST/state"
socket = "$SOCK"
dev_dir = "$ST/dev"
device = "ublk"
harden = true
cpus = "$DAEMON_CPUS"
journal_max = $JOURNAL_MAX
commit_interval_ms = $COMMIT_INTERVAL_MS
log_level = "info"
${CFG_EXTRA:-}
CFG
}

# hostinfo: the benchmark-hygiene header (CONVENTIONS): CPU, kernel (release number only), memory, governor, tools.
# No host name, address, user, instance type or path of the remote home is printed.
hostinfo() {
  echo "- date: $(date -u +%FT%TZ)"
  echo "- cpu: $(lscpu | sed -n 's/^Model name: *//p' | head -1), $(nproc) cpus online, SMT: $(cat /sys/devices/system/cpu/smt/active 2>/dev/null || echo unknown)"
  echo "- kernel: $(uname -r | cut -d- -f1), distro: $(. /etc/os-release && echo "$NAME $VERSION_ID")"
  echo "- memory: $(awk '/MemTotal/{printf "%.0f GiB", $2/1048576}' /proc/meminfo)"
  echo "- governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo 'not exposed (virtual machine, no cpufreq)')"
  echo "- io_uring_disabled: $(cat /proc/sys/kernel/io_uring_disabled 2>/dev/null || echo unknown)"
  echo "- fio: $(fio --version 2>&1 | head -1), e2fsprogs: $(e2fsck -V 2>&1 | head -1), xfsprogs: $(mkfs.xfs -V 2>&1)"
  echo "- lunixd: ${LUNIXD:-unset} $("${LUNIXD:-/nonexistent}" --version 2>&1 | head -1)"
  echo "- cpu sets: daemon ${DAEMON_CPUS:-}, fio ${FIO_CPUS:-}; queues ${QUEUES:-}; journal_max ${JOURNAL_MAX:-}"
  echo "- git revision: ${LUNIX_GIT_REV:-uncommitted or unknown (the host copy has no .git)}"
}

# ---- reporting -------------------------------------------------------------------------------------------------
say() { echo "$*" | tee -a "$R"; }
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; return 1; fi; }
wedged() { echo "WEDGED/STOP: $*" | tee -a "$OUT/WEDGED"; exit 9; }
now_ns() { date +%s%N; }
# rec CELL STATUS [key=value ...]: one JSON line in results.jsonl (status: pass, fail, skip, unreached, info).
# Values are strings; tools/exit/summarize.py (bench/phase1/exit/summarize.py) converts the numbers it needs.
rec() {
  local cell=$1 status=$2; shift 2
  local args=() kv
  for kv in "$@"; do args+=(--arg "${kv%%=*}" "${kv#*=}"); done
  jq -nc --arg cell "$cell" --arg status "$status" --arg t "$(date -u +%FT%TZ)" "${args[@]}" \
    '{cell:$cell,status:$status,t:$t} + $ARGS.named' >> "$RES"
}
# cell_dir CELL: raw artifacts of a cell (fio JSON, fsck JSON, status JSON)
cell_dir() { local d="$OUT/raw/${1//\//_}"; mkdir -p "$d"; echo "$d"; }

# ---- daemon ----------------------------------------------------------------------------------------------------
# start_lunixd [BIN]: (default $DAEMON_BIN, else $LUNIXD) starts the daemon in the background (pinned), waits for the control socket. Environment of the
# caller decides about LUNIX_CRASH_AT / LUNIX_CRASH_COUNT (this function passes them on only when set in CRASH_ENV).
start_lunixd() {
  local bin=${1:-${DAEMON_BIN:-$LUNIXD}}
  _dn=$((_dn + 1)); DLOG="$OUT/lunixd.$_dn.log"
  # shellcheck disable=SC2086  # CRASH_ENV is a list of VAR=value words
  env -u LUNIX_CRASH_AT -u LUNIX_CRASH_COUNT ${CRASH_ENV:-} taskset -c "$DAEMON_CPUS" "$bin" --config "$ST/lunixd.toml" > "$DLOG" 2>&1 &
  DPID=$!
  local i
  for i in $(seq 1 1200); do
    "$LUNIXD" ctl --socket "$SOCK" '{"method":"ping"}' >/dev/null 2>&1 && break
    kill -0 "$DPID" 2>/dev/null || { echo "lunixd died at start:"; tail -20 "$DLOG"; register_all; return 9; }
    sleep 0.05
  done
  register_all
  "$LUNIXD" ctl --socket "$SOCK" '{"method":"ping"}' >/dev/null 2>&1 || wedged "lunixd $DPID not ready after 60 s"
}
# Every device the daemon announced is recorded (the leftovers check and teardown only touch recorded devices).
register_id() { grep -qxF "/dev/ublkb$1" "$CREATED_DEVICES" || { register_device "/dev/ublkb$1"; register_device "/dev/ublkc$1"; }; }
register_all() {
  local id
  for id in $(cat "$OUT"/lunixd.*.log 2>/dev/null | grep -o -E '(dev_id[=":]+ *)[0-9]+' | grep -o -E '[0-9]+$' | sort -u); do register_id "$id"; done
  if [ -n "${CUR_VOL:-}" ] && [ -e "$ST/dev/$CUR_VOL" ]; then
    id=$(readlink -f "$ST/dev/$CUR_VOL" | grep -o -E 'ublkb[0-9]+$' | grep -o -E '[0-9]+$'); [ -n "$id" ] && register_id "$id"
  fi
  return 0
}
daemon_alive() { [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null; }
stop_lunixd() { # SIGTERM = QuiesceAll then exit; a stopping daemon is never killed (W4)
  daemon_alive || { DPID=""; return 0; }
  kill -TERM "$DPID"
  local i; for i in $(seq 1 600); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$DPID" 2>/dev/null; then echo "lunixd $DPID did not exit within 60 s of SIGTERM; not killing it" >&2; return 1; fi
  wait "$DPID" 2>/dev/null; DPID=""
}
wait_daemon_exit() { # secs; sets DRC; never kills
  local i; for i in $(seq 1 $(($1 * 10))); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && return 1
  wait "$DPID" 2>/dev/null; DRC=$?; DPID=""; return 0
}
# recovered_log LOG: the log says the daemon recovered a device at start
recovered_log() { grep -q 'volume recovered at start.*recovered=true' "$1"; }

# ---- lunix CLI -------------------------------------------------------------------------------------------------
lx() { timeout -k 10 "${LX_TIMEOUT:-900}" "$LUNIX" --socket "$SOCK" "$@"; }
lx_logged() { # label args...: runs lx, keeps a copy of stdout+stderr under raw/, returns its exit code
  local label=$1; shift; _cn=$((_cn + 1))
  lx "$@" > "$OUT/lx.$_cn.$label.txt" 2>&1; local rc=$?; register_all; return $rc
}
status_json() { lx status "$1" --json; }
vol_dir() { lx volume list --json | jq -r --arg n "$1" '.[] | select(.name==$n) | .dir'; }
dev_of() { local d; d=$(status_json "$1" 2>/dev/null | jq -r '.device // empty'); [ -n "$d" ] && readlink -f "$d"; }
# key option words for a mode
key_opts() { [ "$1" = encrypted ] && echo "--key-file $KEYS/enc.key"; true; }
mkvol() { # name size_bytes chunk_bytes mode(integrity|encrypted|plain)
  local mode_opt=""
  case "$4" in encrypted) mode_opt="--encrypt --key-file $KEYS/enc.key" ;; plain) mode_opt="--plain" ;; *) mode_opt="--integrity" ;; esac
  # shellcheck disable=SC2086
  lx_logged "create-$1" volume create "$1" --size "$2" --dir "$VOLS" --chunk "$3" --compress "${COMPRESS:-auto}" --id-len 16 $mode_opt
}
attach_vol() { # name [queues]
  # shellcheck disable=SC2046
  lx_logged "attach-$1" attach "$1" --queues "${2:-$QUEUES}" $(key_opts "$CUR_MODE")
}
detach_vol() { lx_logged "detach-$1" detach "$1"; }
# set_vj NAME KEY JSON_VALUE: edits volume.json of a DETACHED volume (the settings that have no CLI flag, for
# example verify_reads); refuses when the key does not exist, so a renamed key is an error and never a silent no-op.
set_vj() {
  local f; f="$(vol_dir "$1")/volume.json"
  jq -e --arg k "$2" 'has($k)' "$f" >/dev/null || { echo "set_vj: $f has no key $2" >&2; return 1; }
  jq --arg k "$2" --argjson v "$3" '.[$k]=$v' "$f" > "$f.new" && mv "$f.new" "$f"
}

# ---- fsck ------------------------------------------------------------------------------------------------------
# lunix_fsck NAME TAG [extra args]: offline fsck of a DETACHED volume, deep unless FSCK_SHALLOW=1; the JSON report is
# kept under the cell's raw directory. Returns 0 when clean (exit 0), or exit 1 with only orphan-class warnings.
lunix_fsck() {
  local name=$1 tag=$2; shift 2
  local d; d=$(vol_dir "$name"); local out="$OUT/raw/fsck-$tag.json" rc deep="--deep"
  [ "${FSCK_SHALLOW:-0}" = 1 ] && deep=""
  # shellcheck disable=SC2046
  if [ -n "${LUNIX_FSCK:-}" ]; then
    "$LUNIX_FSCK" $deep --json $(key_opts "$CUR_MODE") "$@" "$d" > "$out" 2> "$out.err"; rc=$?
  else
    # shellcheck disable=SC2046
    lx fsck "$name" --dir "$VOLS" $deep --json $(key_opts "$CUR_MODE") "$@" > "$out" 2> "$out.err"; rc=$?
  fi
  FSCK_RC=$rc
  case $rc in
    0) return 0 ;;
    1)
      local bad
      bad=$(jq -r --arg ok " $ORPHAN_CODES " '[.findings[] | select(.severity != "note") | select(($ok | contains(" " + .code + " ")) | not) | .code] | unique | join(",")' "$out" 2>/dev/null)
      [ -z "$bad" ] && return 0
      echo "fsck $tag: warnings other than orphans: $bad" >&2; return 1 ;;
    *) return 1 ;;
  esac
}
# store_bytes NAME: bytes the volume directory really occupies on the host file system (apparent sizes of sparse
# journal rings would mislead, so blocks are counted)
store_bytes() { du -s --block-size=1 "$(vol_dir "$1")" 2>/dev/null | cut -f1; }

# ---- volume lifecycle per cell ---------------------------------------------------------------------------------
# cell_begin NAME SIZE_GIB CHUNK_BYTES MODE: fresh daemon state, new volume, attached; sets CUR_VOL CUR_DEV.
# The volume is deleted and recreated between cells (the CLI has no delete: the state and volume directories are
# removed with the daemon stopped and nothing attached).
cell_begin() {
  CUR_VOL=$1; CUR_MODE=$4; CUR_CHUNK=$3
  cell_reset
  start_lunixd || wedged "lunixd did not start for cell $1"
  mkvol "$1" $(($2 * GIB)) "$3" "$4"; expect "create volume $1 (${2} GiB, chunk $3, $4)" $? || true
  attach_vol "$1"; expect "attach $1 ($QUEUES queues)" $? || true
  CUR_DEV=$(dev_of "$1"); [ -b "$CUR_DEV" ] || wedged "no block device for $1 after attach"
  register_id "${CUR_DEV#/dev/ublkb}"
  say "- device $CUR_DEV"
}
cell_reset() { # stop the daemon (nothing attached) and remove all volume and state files of the previous cell
  if daemon_alive; then stop_lunixd || wedged "daemon would not stop between cells"; fi
  rm -rf "${ST:?}/state" "${ST:?}/dev" "${VOLS:?}"; mkdir -p "$VOLS"
}
cell_end() { # NAME TAG: detach, deep fsck, record the store size, return the fsck verdict
  detach_vol "$1"; LAST_DETACH_RC=$?; expect "detach $1 (exit $LAST_DETACH_RC)" $LAST_DETACH_RC || true
  store_bytes "$1" > "$OUT/raw/store-bytes-$2.txt"
  lunix_fsck "$1" "$2"; local rc=$?
  expect "lunix fsck --deep clean after detach (exit $FSCK_RC) [$2]" $rc || true
  return $rc
}

# ---- guest file systems ----------------------------------------------------------------------------------------
mkfs_fs() { # fs dev
  need_safe "$2"
  df_guard "$LUNIX_SPIKE_DIR" "${MKFS_GIB:-40}" || return 2
  case "$1" in
    ext4) mkfs.ext4 -F -q "$2" ;;
    xfs) mkfs.xfs -f -q "$2" ;;
    *) return 2 ;;
  esac
}
mount_fs() { # fs dev mnt
  need_safe "$3"; mkdir -p "$3"
  mount -t "$1" -o nosuid,nodev "$2" "$3"
}
umount_fs() { timeout -k 10 180 umount "$1" || wedged "umount of $1 failed or timed out"; }
guest_fsck() { # fs dev: 0 clean; the device must be unmounted
  case "$1" in
    ext4) e2fsck -fn "$2" > "$OUT/raw/guest-fsck-$(basename "$2")-$_cn.txt" 2>&1 ;;
    xfs) xfs_repair -n "$2" > "$OUT/raw/guest-fsck-$(basename "$2")-$_cn.txt" 2>&1 ;;
  esac
}

# ---- fio -------------------------------------------------------------------------------------------------------
# fio_run LABEL RUNTIME_S JOBFILE [fio args]: bounded foreground fio (pinned, JSON output kept); returns fio's rc.
# The outer timeout is runtime + FIO_GRACE (default 900 s: the verify phase after the write phase) and signals fio only.
fio_run() {
  local label=$1 rt=$2 job=$3; shift 3
  label=${label//\//_}; local out="$OUT/raw/fio-$label.json"
  timeout -k 30 $((rt + ${FIO_GRACE:-900})) taskset -c "$FIO_CPUS" fio "$job" --output-format=json --output="$out" "$@" > "$OUT/raw/fio-$label.stdout" 2>&1
}
# fio_bg LABEL RUNTIME_S JOBFILE [fio args]: same in the background; the pid is LAST_FIO and in FIO_PIDS
fio_bg() {
  local label=$1 rt=$2 job=$3; shift 3
  label=${label//\//_}; local out="$OUT/raw/fio-$label.json"
  timeout -k 30 $((rt + ${FIO_GRACE:-900})) taskset -c "$FIO_CPUS" fio "$job" --output-format=json --output="$out" "$@" > "$OUT/raw/fio-$label.stdout" 2>&1 &
  LAST_FIO=$!; FIO_PIDS+=("$LAST_FIO")
}
wait_fio() { # pid secs -> fio's rc (a wedge when it does not end)
  local i
  for i in $(seq 1 $(($2 * 10))); do kill -0 "$1" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$1" 2>/dev/null && wedged "fio $1 did not finish within $2 s"
  wait "$1"; local rc=$?
  local p k=(); for p in "${FIO_PIDS[@]}"; do [ "$p" = "$1" ] || k+=("$p"); done; FIO_PIDS=("${k[@]}")
  return $rc
}
stop_fio() {
  local p i
  for p in "${FIO_PIDS[@]}"; do kill -TERM "$p" 2>/dev/null; done
  for p in "${FIO_PIDS[@]}"; do
    for i in $(seq 1 600); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
    kill -0 "$p" 2>/dev/null && echo "teardown: fio $p did not stop within 60 s (not killed)" >&2
  done
  FIO_PIDS=()
}
# fio_json FILE: the JSON part of a fio output file (fio may print warnings in front of it)
# A fio that was stopped with TERM can leave its "select() call in helper thread failed" message glued in front of the
# opening brace (same line), so the JSON is taken from the line that holds "fio version" onwards.
fio_json() { echo '{'; sed -n '/"fio version"/,$p' "$1"; }
# fio_errors FILE: sum of the job-level error codes in a fio JSON report (0 = every job ended without error, a verify
# failure is a nonzero job error); 999 when the report cannot be read
fio_errors() {
  local n; n=$(fio_json "$1" | jq -r 'if (.jobs | length) == 0 then 999 else ([.jobs[] | ((.error // 0) | tonumber)] | add) end' 2>/dev/null)
  echo "${n:-999}"
}

# ---- teardown --------------------------------------------------------------------------------------------------
detach_all() {
  local n
  for n in $(lx volume list --json 2>/dev/null | jq -r '.[] | select(.device != null) | .name'); do
    lx detach "$n" >> "$OUT/teardown.log" 2>&1
  done
}
teardown() {
  trap - EXIT INT TERM
  CRASH_ENV=""   # a recovery start must never carry a crash setting
  if [ -e "$OUT/WEDGED" ]; then echo "teardown: run marked WEDGED; nothing else touched" >&2; cap_stop; return 1; fi
  local d left=""
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do [ -b "$d" ] && left="$left $d"; done
  if [ -n "$left" ] && ! daemon_alive; then
    echo "teardown: devices$left have no daemon; starting lunixd to recover them"
    start_lunixd || echo "teardown: lunixd did not start" >&2
  fi
  stop_fio   # only now: a fio on a quiesced device cannot end before a daemon serves it again
  # PRE_TEARDOWN: a script's own cleanup of things mounted on top of the device (a database, chroot mounts, loop
  # devices over files on it); it runs while a daemon serves the device
  [ -n "${PRE_TEARDOWN:-}" ] && eval "$PRE_TEARDOWN" >> "$OUT/teardown.log" 2>&1
  mountpoint -q "$MNT" 2>/dev/null && timeout -k 10 180 umount "$MNT" >> "$OUT/teardown.log" 2>&1
  if daemon_alive; then detach_all; stop_lunixd || { cap_stop; return 1; }; fi
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do
    if [ -b "$d" ]; then
      echo "teardown: deleting $d (its daemon is gone)"
      timeout -k 5 75 "$TESTD" del --id "${d#/dev/ublkb}" --force || echo "teardown: delete of $d failed" >&2
    fi
  done
  rm -rf "$VOLS" "$KEYS"
  cap_stop
  bash "$REPO/spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-end.txt" 2>&1
  return 0
}
