#!/bin/bash
# Queue B2 (database agent): controls, Constellation plain views, DB sets, per-volume arms. CPUs 12-13. Run from ~/dictval with codeB.
cd ~/dictval; export DICTVAL_TRACK=$HOME/dictval/work/tracked-b.txt DICTVAL_CPUS=12-13
C="taskset -c 12-13 python3 -I codeB"
step() { echo "[$(date +%T)] $*"; "$@"; }
DS="se-askubuntu se-superuser se-serverfault geonames tlc-2024q1"
if [ ! -e out/db-ctl-pgbench-pg.json ]; then step $C/dbctl.py pgbench 16 && step $C/corpus.py prep db-ctl-pgbench-pg work/db-ctl-pgbench-pg.raw db pg || echo "FAILED ctl pgbench"; fi
if [ ! -e out/db-ctl-sysbench-my.json ]; then step $C/dbctl.py sysbench 17 && step $C/corpus.py prep db-ctl-sysbench-my work/db-ctl-sysbench-my.raw db my || echo "FAILED ctl sysbench"; fi
ALL=""; for ds in $DS; do for e in pg my sq; do ALL="$ALL db-$ds-$e"; done; done; ALL="$ALL db-ctl-pgbench-pg db-ctl-sysbench-my"
for n in $ALL; do
  [ -e work/res/cv-$n-pg.plain.bin ] || step $C/dbview.py plain $n || echo "FAILED view $n"
done
step $C/dbtrain.py all 2 || echo "FAILED dbtrain"
for n in $ALL; do
  [ -e work/res/$n-65536.db.bin ] || step $C/dbarms.py lunix $n || echo "FAILED lunix $n"
  [ -e work/res/cv-$n-pg.db.bin ] || step $C/dbarms.py cv $n || echo "FAILED cv $n"
  df -h ~ | tail -1
done
echo "[$(date +%T)] queue B2 done"
