#!/bin/bash
# detached root runner: the same lock, leftovers and timeout rules as tools/remote.sh root, started on the host
# usage: run-root.sh LOG TIMEOUT_S cmd...
base=$HOME/lunix-p1; src=$base/m15/src; lock=$base/root.lock; left=$base/leftovers.txt
log=$1; tmo=$2; shift 2
cd "$src" || exit 9
export PATH="$HOME/.cargo/bin:$PATH" LUNIX_SPIKE_DIR=$base/m15/scratch LUNIX_M=m15 LUNIX_ON_HOST=1
mkdir -p "$LUNIX_SPIKE_DIR"
exec >>"$log" 2>&1
echo "start $(date -u +%FT%TZ): $*"
if [ -s "$left" ]; then echo "previous run left devices behind (W9)"; cat "$left"; echo "EXIT 3"; exit 3; fi
exec 9>"$lock"; flock -w 3600 9 || { echo "lock busy"; echo "EXIT 4"; exit 4; }
echo "root: lock held for m15" > "$lock.holder"
bash spikes/common/ublk-leftovers.sh
export LUNIX_ROOT_TESTS=1
sudo -n timeout -s TERM -k 180 "$tmo" "$@"; rc=$?
[ $rc -eq 137 ] && echo "wedge: script outlived the grace" > "$left"
bash spikes/common/ublk-leftovers.sh || rc=$((rc ? rc : 5))
echo "EXIT $rc"
