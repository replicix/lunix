#!/bin/bash
# Follow-up M1 queue (pv-cover on the 15 database images, 16 and 64 KiB).  Run: cd ~/dictval && nohup bash codeC/queue_fu_m1.sh > logsC/m1.log 2>&1 &
cd ~/dictval || exit 1; export DICTVAL=$HOME/dictval; mkdir -p logsC; export DICTVAL_TRACK=$HOME/dictval/logsC/tracked.txt
F=logsC/FAILED; : > logsC/m1.done
JOBS=$(for d in se-askubuntu se-superuser se-serverfault geonames tlc-2024q1; do for e in pg my sq; do for cs in 16384 65536; do echo "db-$d-$e $cs"; done; done; done)
echo "$JOBS" | xargs -P4 -L1 sh -c 'taskset -c 12-15 python3 -I codeC/dbpvcover.py train $0 $1 || echo "FAILED train $0 $1" >> logsC/FAILED'
echo "$JOBS" | while read n cs; do
  python3 -I codeC/dbpvcover.py measure $n $cs || echo "FAILED measure $n $cs" >> $F
done
python3 -I codeC/dbpvcover.py agg out || echo "FAILED agg" >> $F
echo finished > logsC/m1.done
