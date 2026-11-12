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
