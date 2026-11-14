#!/bin/bash
# Queue B1 (agent B): database images, load + quiesce + Lunix-view prep, sequential, CPUs 12-13.  Run from ~/dictval with codeB.
cd ~/dictval; export DICTVAL_TRACK=$HOME/dictval/work/tracked-b.txt
C="taskset -c 12-13 python3 -I codeB"
step() { echo "[$(date +%T)] $*"; "$@"; }
n=1
for ds in se-askubuntu geonames tlc-2024q1 se-superuser se-serverfault; do
  if [ ! -e work/stage-$ds/.done ]; then
    rm -rf work/stage-$ds
    step $C/dbconv.py $ds dl work/stage-$ds && touch work/stage-$ds/.done || { echo "FAILED conv $ds"; n=$((n+3)); continue; }
  fi
  for eng in pg my sq; do
    if [ -e out/db-$ds-$eng.json ] && [ -e work/db-$ds-$eng-65536.cls ]; then n=$((n+1)); continue; fi
    if step $C/dbload.py $ds $eng $n; then
      step $C/corpus.py prep db-$ds-$eng work/db-$ds-$eng.raw db $eng || echo "FAILED prep $ds $eng"
    else echo "FAILED load $ds $eng"; fi
    n=$((n+1)); df -h ~ | tail -1
  done
  rm -rf work/stage-$ds
done
echo "[$(date +%T)] queue B1 done"
