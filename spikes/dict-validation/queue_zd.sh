#!/bin/bash
# Queue ZD: zdicts arms (ft, oracle) for the six raw-device corpora at 16 and 64 KiB.  CPUs 14-15 only.
cd ~/dictval; export DICTVAL_CPUS=14-15; P="taskset -c 14-15 python3 -I code"
step() { echo "[$(date +%T)] $*"; "$@" || { echo "FAILED: $*"; }; }
while pgrep -f "[z]dtool/target/release/zdtool sel" >/dev/null; do sleep 10; done
for c in host-ext4 host-xfs host-btrfs vm-ubuntu vm-debian vm-fedora; do
  for cs in 16384 65536; do
    [ -s work/$c-$cs.ftsel.u8 ] || step $P/zd.py sel $c $cs
    step $P/zd.py measure $c $cs
    step $P/zd.py agg $c $cs
  done
done
echo "[$(date +%T)] queue ZD done"
