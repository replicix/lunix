#!/bin/bash
# Shared harness of the M12a lifecycle and crash scripts (source this file; root only).
# The caller sets NAME (state prefix), CFG_EXTRA (extra lines of lunixd.toml), and calls harness_init.
# Same safety rules as m12a-smoke.sh (plan W1-W9): lunixd never sends a stop command; a delete is only
# `lunix-ublk-testd del --force` of a device recorded by this run whose daemon is gone (W1a); every failure
# path stops fio, makes sure a daemon serves the device before the unmount, detaches, and only then lets the
# daemon exit; a stopping daemon is never killed; no LUNIX_CRASH_AT is ever set; a wedged step stops the run.
here="$(cd "$(dirname "$0")" && pwd)"
ublk_root="$here/../../../lunix-ublk/tests/root"
. "$ublk_root/lib.sh"

harness_init() { # NAME
  NAME=$1
  require_root; need_root_tests
  : "${LUNIXD:?LUNIXD is not set}" "${TESTD:?TESTD is not set}"
  : "${OUT:=$LUNIX_SPIKE_DIR/$NAME-out}" "${DAEMON_CPUS:=2-5}" "${FIO_CPUS:=6-9}"
  ST="$LUNIX_SPIKE_DIR/$NAME-state"; VOLS="$LUNIX_SPIKE_DIR/$NAME-vols"; MNT="$LUNIX_SPIKE_DIR/$NAME-mnt"
  need_safe "$OUT"; need_safe "$ST"; need_safe "$VOLS"
  rm -rf "$OUT"; mkdir -p "$OUT"
  bash "$here/../../../../spikes/common/ublk-leftovers.sh" > "$OUT/leftovers-start.txt" || { cat "$OUT/leftovers-start.txt"; echo "not clean at start"; exit 2; }
  : > "$CREATED_DEVICES"
  script_cap "${SCRIPT_CAP:-2400}"
  lsmod | grep -q '^ublk_drv ' || { modprobe ublk_drv && add_cleanup 'rmmod ublk_drv'; }
  rm -rf "$ST" "$VOLS"; mkdir -p "$ST" "$VOLS"
  SOCK="$ST/ctl.sock"
  R="$OUT/results.md"; : > "$R"
  FAIL=0; DPID=""; FIO_PIDS=(); _dn=0; _cn=0
  write_config
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
journal_max = ${JOURNAL_MAX:-268435456}
commit_interval_ms = 5000
log_level = "info"
${CFG_EXTRA:-}
CFG
}
say() { echo "$*" | tee -a "$R"; }
pass() { say "- PASS $*"; }
fail() { say "- FAIL $*"; FAIL=1; }
expect() { if [ "$2" = 0 ]; then pass "$1"; else fail "$1"; fi; }
wedged() { echo "WEDGED/STOP: $*" | tee -a "$OUT/WEDGED"; exit 9; }

start_lunixd() {
  _dn=$((_dn + 1)); DLOG="$OUT/lunixd.$_dn.log"
  env -u LUNIX_CRASH_AT taskset -c "$DAEMON_CPUS" "$LUNIXD" --config "$ST/lunixd.toml" > "$DLOG" 2>&1 &
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
ctl() { timeout -k 5 300 "$LUNIXD" ctl --socket "$SOCK" "$1"; }
register_id() { grep -qxF "/dev/ublkb$1" "$CREATED_DEVICES" || { register_device "/dev/ublkb$1"; register_device "/dev/ublkc$1"; }; }
register_all() {
  local id
  for id in $(cat "$OUT"/lunixd.*.log "$OUT"/ctl.*.json 2>/dev/null | grep -o -E '(dev_id[=":]+ *)[0-9]+' | grep -o -E '[0-9]+$' | sort -u); do register_id "$id"; done
}
ctlj() { # request -> prints the reply; keeps a copy; returns the call's exit code
  _cn=$((_cn + 1)); local f="$OUT/ctl.$_cn.json"
  ctl "$1" > "$f"; local rc=$?
  register_all; cat "$f"; return $rc
}
attached_dev() { ctl "{\"method\":\"volume_status\",\"params\":{\"name\":\"$1\"}}" | jq -r '.ok.device // empty' | xargs -r readlink -f; }
create_vol() { # name size_bytes
  ctlj "{\"method\":\"volume_create\",\"params\":{\"name\":\"$1\",\"dir\":\"$VOLS\",\"size_bytes\":$2,\"chunk_size\":65536}}" | grep -q '"volume_created"'
}
vol_dir() { ctl '{"method":"volume_list"}' | jq -r ".ok.volumes[] | select(.name==\"$1\") | .dir"; }
attach() { ctlj "{\"method\":\"attach\",\"params\":{\"name\":\"$1\",\"queues\":$2}}" | grep -q '"attached"'; }
detach() { ctlj "{\"method\":\"detach\",\"params\":{\"name\":\"$1\"}}" | grep -q '"detached"'; }

# fio on a raw device: start_fio NAME DEV OFFSET SIZE IO_SIZE_OR_RUNTIME_ARGS...  (always bounded, W3)
start_fio() { # name dev offset size extra-args...
  local n=$1 dev=$2 off=$3 size=$4; shift 4
  taskset -c "$FIO_CPUS" fio --name="$n" --filename="$dev" --direct=1 --ioengine=libaio --rw="${FIO_RW:-randwrite}" \
    --offset="$off" --size="$size" --verify=crc32c --verify_fatal=1 --do_verify=1 --verify_backlog=512 \
    --output="$OUT/fio-$n.txt" "$@" >/dev/null 2>&1 &
  FIO_PIDS+=($!); LAST_FIO=$!
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
stop_lunixd() { # SIGTERM = QuiesceAll then exit; a stopping daemon is never killed (W4)
  [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null || { DPID=""; return 0; }
  kill -TERM "$DPID"
  local i; for i in $(seq 1 600); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$DPID" 2>/dev/null; then echo "lunixd $DPID did not exit within 60 s of SIGTERM; not killing it" >&2; return 1; fi
  wait "$DPID" 2>/dev/null; DPID=""
}
wait_daemon_exit() { # secs; the daemon exits by itself after QuiesceAll; never killed
  local i; for i in $(seq 1 $(($1 * 10))); do kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
  kill -0 "$DPID" 2>/dev/null && return 1
  wait "$DPID" 2>/dev/null; DRC=$?; DPID=""; return 0
}
detach_all() {
  local n
  for n in $(ctl '{"method":"volume_list"}' | jq -r '.ok.volumes[] | select(.device != null) | .name'); do
    ctl "{\"method\":\"detach\",\"params\":{\"name\":\"$n\"}}" >> "$OUT/teardown.log" 2>&1
  done
}
teardown() {
  trap - EXIT INT TERM
  if [ -e "$OUT/WEDGED" ]; then echo "teardown: run marked WEDGED; nothing else touched" >&2; cap_stop; return 1; fi
  local d left=""
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do [ -b "$d" ] && left="$left $d"; done
  if [ -n "$left" ] && { [ -z "$DPID" ] || ! kill -0 "$DPID" 2>/dev/null; }; then
    echo "teardown: devices$left have no daemon; starting lunixd to recover them"
    start_lunixd || echo "teardown: lunixd did not start" >&2
  fi
  stop_fio   # only now: a fio on a quiesced device cannot end before a daemon serves it again
  if [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null; then detach_all; stop_lunixd || { cap_stop; return 1; }; fi
  for d in $(grep -o '/dev/ublkb[0-9]*' "$CREATED_DEVICES" 2>/dev/null | sort -u); do
    if [ -b "$d" ]; then
      echo "teardown: deleting $d (its daemon is gone)"
      timeout -k 5 75 "$TESTD" del --id "${d#/dev/ublkb}" --force || echo "teardown: delete of $d failed" >&2
    fi
  done
  rm -rf "$VOLS"
  cap_stop
  return 0
}
