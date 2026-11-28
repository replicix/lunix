#!/bin/bash
# sequential queue of detached root runs: queue.sh QUEUEFILE ; each line: NAME TIMEOUT_S env-words... -- script
# stops at the first run that ends with a wedge (EXIT 137/9) or a refusal (EXIT 3/4); status in logs/queue.<id>.status
q=$1; id=$(basename "$q" .txt); st=$HOME/lunix-p1/m15/logs/queue.$id.status
T=$HOME/lunix-p1/m15/target/release; TC=$HOME/lunix-p1/m15/target-cp/release
: > "$st"
while read -r name tmo rest; do
  [ -z "$name" ] && continue
  case "$name" in \#*) continue;; esac
  echo "$(date -u +%FT%TZ) start $name" >> "$st"
  eval "$HOME/lunix-p1/m15/run-root.sh $HOME/lunix-p1/m15/logs/$name.log $tmo env LUNIX_ROOT_TESTS=1 LUNIX_SPIKE_DIR=$HOME/lunix-p1/m15/scratch DAEMON_CPUS=${DAEMON_CPUS:-0-3} FIO_CPUS=${FIO_CPUS:-4-7} QUEUES=4 LUNIXD=$T/lunixd LUNIX=$T/lunix TESTD=$T/lunix-ublk-testd LUNIX_FSCK=$T/lunix-fsck LUNIXD_CP=$TC/lunixd XFSTESTS_DIR=$HOME/lunix-p1/m15/xfstests TARBALL=$HOME/lunix-inputs/linux-6.12.tar.xz $rest"
  rc=$(grep "^EXIT " "$HOME/lunix-p1/m15/logs/$name.log" | tail -1 | cut -d" " -f2)
  echo "$(date -u +%FT%TZ) end $name rc=$rc" >> "$st"
  case "$rc" in 3|4|9|137|5) echo "queue stopped: $name rc=$rc" >> "$st"; break;; esac
done < "$q"
echo "QUEUE DONE" >> "$st"
