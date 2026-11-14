#!/bin/bash
# Queue A (loop-device steps run strictly one after another): host trees, pool, VM images.  Run from ~/dictval.
cd ~/dictval; C="taskset -c 12-15 python3 -I code"
while pgrep -f "mkhost.py ext4" >/dev/null; do sleep 20; done
step() { echo "[$(date +%T)] $*"; "$@" || echo "FAILED: $*"; }
step $C/corpus.py prep host-ext4 work/host-ext4.raw
for d in alpine rocky suse arch; do step $C/pool.py $d; done
for fs in xfs btrfs; do step $C/mkhost.py $fs; step $C/corpus.py prep host-$fs work/host-$fs.raw; done
step taskset -c 12-15 qemu-img convert -O raw -S 4k dl/Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2 work/vm-fedora.raw
step $C/corpus.py prep vm-ubuntu work/ubuntu-26.raw
step $C/corpus.py prep vm-debian work/debian-13-genericcloud-amd64.raw
step $C/corpus.py prep vm-fedora work/vm-fedora.raw
echo "[$(date +%T)] queue A done"
