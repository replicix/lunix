#!/bin/bash
# Shared helpers for the Phase 0 privileged spikes (source this file).
# Scratch dir and run dir are fixed here; every destructive step goes through
# safe_path (CONVENTIONS ublk safety rule 2).
: "${LUNIX_SPIKE_DIR:=/var/tmp/lunix-spike}"
: "${RUN_DIR:=$LUNIX_SPIKE_DIR/run}"
mkdir -p "$LUNIX_SPIKE_DIR" "$RUN_DIR" 2>/dev/null
CREATED_DEVICES="$RUN_DIR/created-devices"
touch "$CREATED_DEVICES" 2>/dev/null

require_root() { [ "$(id -u)" = 0 ] || { echo "need root (run via sudo)" >&2; exit 1; }; }
skip() { echo "SKIP: $*" >&2; exit 77; }
register_device() { echo "$1" >> "$CREATED_DEVICES"; }

# safe_path P: succeeds only if P resolves under $LUNIX_SPIKE_DIR or is listed
# in created-devices (device nodes are compared unresolved and resolved).
safe_path() {
  local p="$1" r
  r=$(readlink -m -- "$p" 2>/dev/null) || return 1
  case "$r" in "$LUNIX_SPIKE_DIR"/*|"$LUNIX_SPIKE_DIR") return 0 ;; esac
  grep -qxF -- "$p" "$CREATED_DEVICES" 2>/dev/null && return 0
  grep -qxF -- "$r" "$CREATED_DEVICES" 2>/dev/null && return 0
  echo "safe_path: refusing $p" >&2
  return 1
}
need_safe() { safe_path "$1" || exit 99; }

# Cleanup actions run in reverse order on EXIT/INT/TERM. Each action is a shell
# string. Typical order of registration: loaded modules first, devices later.
_CLEANUPS=()
add_cleanup() { _CLEANUPS+=("$1"); }
cleanup() {
  trap - EXIT INT TERM
  local i
  for ((i=${#_CLEANUPS[@]}-1; i>=0; i--)); do
    eval "${_CLEANUPS[$i]}" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT INT TERM

# ---- additions for the M10 harness (testd) ---------------------------------
# The root wrapper runs scripts under `sudo -n`, which clears the environment:
# pass LUNIX_SPIKE_DIR, TESTD and LUNIX_ROOT_TESTS with `env` on the command line.
: "${LUNIX_MIN_FREE_GIB:=120}"
# df_guard PATH GIB: refuse unless the file system of PATH keeps LUNIX_MIN_FREE_GIB GiB free after GIB more are used
# (the host-side twin of `tools/remote.sh df-guard`, which the driver also calls before the root run).
df_guard() {
  local avail; avail=$(df -B1G --output=avail "$1" | tail -n 1 | tr -d ' ')
  [[ "$avail" =~ ^[0-9]+$ ]] || { echo "df_guard: cannot read free space of $1" >&2; return 1; }
  if [ $((avail - $2)) -lt "$LUNIX_MIN_FREE_GIB" ]; then
    echo "df_guard: refusing: $avail GiB free, $2 GiB wanted, $LUNIX_MIN_FREE_GIB GiB must stay free" >&2; return 1
  fi
  echo "df_guard: ok ($avail GiB free, $2 GiB wanted)"
}
need_root_tests() { [ "${LUNIX_ROOT_TESTS:-}" = 1 ] || skip "LUNIX_ROOT_TESTS=1 is not set (run through tools/remote.sh root)"; }

# script_cap SECONDS: the script's own cap (W8 asks for one next to the wrapper's). When it fires the
# script gets TERM, whose trap runs the teardown (recover, unmount, detach, delete).
script_cap() {
  ( sleep "$1"; echo "script cap of $1 s reached: sending TERM to the script" >&2; kill -TERM $$ ) &
  CAP_PID=$!
  add_cleanup 'kill $CAP_PID 2>/dev/null'
}
