#!/bin/bash
# Queue A, third part: relabel vm-fedora (btrfs subvolumes), host-xfs / host-btrfs, then the arms.  CPUs 14-15 only.
cd ~/dictval; export DICTVAL_CPUS=14-15; P="taskset -c 14-15 python3 -I code"
step() { echo "[$(date +%T)] $*"; "$@" || { echo "FAILED: $*"; }; }
while pgrep -f "[c]ode/mkhost.py" >/dev/null; do sleep 10; done
step $P/corpus.py prep host-xfs work/host-xfs.raw
step $P/corpus.py post vm-fedora work/vm-fedora.raw
for fs in btrfs; do step $P/mkhost.py $fs; step $P/corpus.py prep host-$fs work/host-$fs.raw; done
for c in host-ext4 host-xfs host-btrfs vm-ubuntu vm-debian vm-fedora; do
  step $P/run.py trainbs $c
  for cs in 16384 65536; do step $P/run.py arms $c $cs; step $P/run.py agg $c $cs; done
done
step $P/run.py sweep host-ext4 16384
step $P/run.py agg host-ext4 16384
echo "[$(date +%T)] queue A4 done"
