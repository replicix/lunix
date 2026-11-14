#!/bin/bash
# Queue B: database images.  Conversions overlap queue A; loads and labelling (loop devices) wait for queue A.  Run from ~/dictval.
cd ~/dictval; C="taskset -c 12-15 python3 -I code"
step() { echo "[$(date +%T)] $*"; "$@" || echo "FAILED: $*"; }
n=1
for ds in se-askubuntu geonames tlc-2024q1 se-superuser se-serverfault; do
  if [ ! -d work/stage-$ds ] || [ ! -e work/stage-$ds/.done ]; then
    step $C/dbconv.py $ds dl work/stage-$ds && touch work/stage-$ds/.done
  fi
  while ! grep -q "queue A done" logs/queue_a.log; do sleep 30; done
  for eng in pg my sq; do
    step $C/dbload.py $ds $eng $n; n=$((n+1))
    step $C/corpus.py prep db-$ds-$eng work/db-$ds-$eng.raw db $eng
  done
  rm -rf work/stage-$ds
done
echo "[$(date +%T)] queue B done"
