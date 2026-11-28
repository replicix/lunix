#!/bin/bash
# launcher on the host (detached): the root run of tools/remote.sh, without the ssh session; BLOCKS selects the blocks
base=$(cd ../.. && pwd); lock=$base/root.lock; left=$base/leftovers.txt
[ -s "$left" ] && { echo "leftovers"; exit 3; }
exec 9>"$lock"; flock -w 3600 9 || exit 4
echo "root: lock held by $$ for c5def" > "$lock.holder"
bash spikes/common/ublk-leftovers.sh || exit 5
export LUNIX_ROOT_TESTS=1
T=$(cd ../target && pwd)/release; S=$(cd ../scratch && pwd)
if [ -n "$BLOCKS" ]; then export BLOCKS; else unset BLOCKS; fi
sudo -n timeout -s TERM -k 180 60000 env ${BLOCKS:+BLOCKS="$BLOCKS"} LUNIXD=$T/lunixd LUNIX=$T/lunix TESTD=$T/lunix-ublk-testd LUNIX_SPIKE_DIR=$S RESDIR=$S/c5def-results bash tests/perf/c5def.sh
rc=$?
[ $rc -eq 137 ] && echo 'wedge' > "$left"
bash spikes/common/ublk-leftovers.sh || rc=$?
echo "exit $rc"
