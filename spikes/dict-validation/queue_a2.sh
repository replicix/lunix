#!/bin/bash
# Queue A (second executor): file-system and VM-image corpora, every arm that needs no zdicts.  Run from ~/dictval; CPUs 14-15 only.
cd ~/dictval; export DICTVAL_CPUS=14-15; P="taskset -c 14-15 python3 -I code"
step() { echo "[$(date +%T)] $*"; "$@" || { echo "FAILED: $*"; FAILED=1; }; }
mkdir -p logs
# finish what exists
step $P/corpus.py post vm-debian work/debian-13-genericcloud-amd64.raw
# pool (hold-out training images), then the other corpora
for d in alpine rocky suse arch; do [ -e work/pool/$d-native.json ] && [ -e work/pool/$d-btrfs.json ] || step $P/pool.py $d; done
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
echo "[$(date +%T)] queue A2 done"
