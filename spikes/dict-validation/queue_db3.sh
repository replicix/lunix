#!/bin/bash
# Queue B3 (database agent): zdicts ft/oracle arms, aggregation, sweep, file-system sensitivity. Waits for queue B2. CPUs 12-13.
cd ~/dictval; export DICTVAL_TRACK=$HOME/dictval/work/tracked-b.txt DICTVAL_CPUS=12-13
C="taskset -c 12-13 python3 -I codeB"
step() { echo "[$(date +%T)] $*"; "$@"; }
while ! grep -q "queue B2 done" logsB/queue_db2.log; do sleep 30; done
DS="se-askubuntu se-superuser se-serverfault geonames tlc-2024q1"
ALL=""; for ds in $DS; do for e in pg my sq; do ALL="$ALL db-$ds-$e"; done; done; ALL="$ALL db-ctl-pgbench-pg db-ctl-sysbench-my"
for n in $ALL; do for cs in 16384 65536; do step $C/dbzd.py $n $cs || echo "FAILED zd $n $cs"; done; done
mkdir -p out/agg; step $C/dbagg.py out/agg $ALL || echo "FAILED dbagg"
step $C/dbsens.py sweep || echo "FAILED sweep"
rm -rf work/stage-se-askubuntu; step $C/dbconv.py se-askubuntu dl work/stage-se-askubuntu || echo "FAILED conv"
n=40
for fsn in xfs btrfs; do
  step $C/dbload.py se-askubuntu pg $n $fsn && step $C/dbsens.py fs $fsn || echo "FAILED fs $fsn"
  n=$((n+1)); df -h ~ | tail -1
done
rm -rf work/stage-*
echo "[$(date +%T)] queue B3 done"
