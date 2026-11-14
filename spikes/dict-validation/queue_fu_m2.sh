#!/bin/bash
# Follow-up M2 queue (D5 per-object pass).  After M1 finished.  Run: cd ~/dictval && nohup bash codeC/queue_fu_m2.sh > logsC/m2.log 2>&1 &
cd ~/dictval || exit 1; export DICTVAL=$HOME/dictval; export DICTVAL_TRACK=$HOME/dictval/logsC/tracked2.txt
while [ ! -s logsC/m1.done ]; do sleep 30; done
for c in vm-debian vm-fedora vm-ubuntu host-files; do
  taskset -c 12-15 python3 -I codeC/d5files.py $c || echo "FAILED d5 $c" >> logsC/FAILED
done
echo finished > logsC/m2.done
