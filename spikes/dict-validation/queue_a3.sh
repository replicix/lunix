#!/bin/bash
# Queue A, second part (after pool alpine/suse/arch): rocky pool rerun, remaining corpora, then the arms.  CPUs 14-15 only.
cd ~/dictval; export DICTVAL_CPUS=14-15; P="taskset -c 14-15 python3 -I code"
step() { echo "[$(date +%T)] $*"; "$@" || { echo "FAILED: $*"; }; }
while pgrep -f "[c]ode/pool.py" >/dev/null; do sleep 10; done
rm -f work/pool-rocky*.raw work/pool/rocky-*
step $P/pool.py rocky
step $P/corpus.py prep vm-ubuntu work/ubuntu-26.raw
[ -e work/vm-fedora.raw ] || step taskset -c 14-15 qemu-img convert -O raw -S 4k dl/Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2 work/vm-fedora.raw
step $P/corpus.py prep vm-fedora work/vm-fedora.raw
for fs in xfs btrfs; do step $P/mkhost.py $fs; step $P/corpus.py prep host-$fs work/host-$fs.raw; done
for c in host-ext4 host-xfs host-btrfs vm-ubuntu vm-debian vm-fedora; do
  step $P/run.py trainbs $c
  for cs in 16384 65536; do step $P/run.py arms $c $cs; step $P/run.py agg $c $cs; done
done
step $P/run.py sweep host-ext4 16384
step $P/run.py agg host-ext4 16384
echo "[$(date +%T)] queue A3 done"
