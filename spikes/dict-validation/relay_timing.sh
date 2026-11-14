#!/bin/bash
# Follow-up M3 relay driver, runs on the LOCAL machine (copying only; nothing study-specific runs here).  Per image: draw the block sample on the test host (both chunk sizes),
# stream it compressed through $RELAY to the quiet host, time both sizes there, delete the sample on all three places.
# Environment (never written to a file): TH = test-host ssh target, QUIET_HOST = quiet host's ssh alias, QUIET_SSH_CONFIG = ssh config file
# for it, DICTVAL_RELAY = local relay directory.
export SSH_AUTH_SOCK=/run/user/1000/keyring/ssh
: "${TH:?set TH in the environment}" "${QUIET_HOST:?set QUIET_HOST in the environment}" "${QUIET_SSH_CONFIG:?set QUIET_SSH_CONFIG in the environment}" "${DICTVAL_RELAY:?set DICTVAL_RELAY in the environment}"
RELAY=$DICTVAL_RELAY; BOXSSH="ssh -F $QUIET_SSH_CONFIG -o BatchMode=yes $QUIET_HOST"; TSSH="ssh -o BatchMode=yes $TH"
mkdir -p $RELAY; LOG=$RELAY/relay.log
for ds in se-askubuntu se-superuser se-serverfault geonames tlc-2024q1; do for eng in pg my sq; do
  img=db-$ds-$eng
  n=$($BOXSSH "grep -c \"^$img-\" ~/dictval-bench/out/timing-db.tsv 2>/dev/null || true" </dev/null | tail -1)
  if [ "${n:-0}" -ge 28 ]; then echo "$(date +%T) $img already timed" >> $LOG; continue; fi
  echo "$(date +%T) $img start" >> $LOG
  if $BOXSSH "test -s ~/dictval-bench/tmdb/$img/pvcover65536.dict" </dev/null; then echo "$(date +%T) $img sample already on the quiet host" >> $LOG; else
  until $TSSH "for cs in 16384 65536; do grep -qE \"^$img \$cs (1 0.01 112640 cover|dictionary exists)\" ~/dictval/logsC/m1.log || exit 1; done" </dev/null; do sleep 60; done
  $TSSH "cd ~/dictval && rm -rf tm/$img && nice -n 19 ionice -c3 taskset -c 12-15 python3 -I tmcode/tmblocks.py $img tm/$img >> logsC/tmblocks.log && for cs in 16384 65536; do cp work/dicts/pv-$img-\$cs-s1-all.dict tm/$img/pv\$cs.dict && cp work/dicts/pv-$img-\$cs-s1-cover.dict tm/$img/pvcover\$cs.dict; done && tar c -C tm/$img sample.bin recs16384 recs65536 pv16384.dict pv65536.dict pvcover16384.dict pvcover65536.dict | nice -n 19 ionice -c3 taskset -c 12-15 zstd -6 --long=27 -T2 -q" </dev/null > $RELAY/$img.tar.zst 2>> $LOG || { echo "FAILED sample $img" >> $LOG; rm -f $RELAY/$img.tar.zst; $TSSH true </dev/null || { echo "$(date +%T) test host unreachable, stopping" >> $LOG; exit 1; }; continue; }
  echo "$(date +%T) $img relayed $(stat -c %s $RELAY/$img.tar.zst) bytes" >> $LOG
  $TSSH "rm -rf ~/dictval/tm/$img" </dev/null 2>> $LOG
  $BOXSSH "rm -rf ~/dictval-bench/tmdb/$img; mkdir -p ~/dictval-bench/tmdb/$img && zstd -d --long=27 -c -q | tar x -C ~/dictval-bench/tmdb/$img" < $RELAY/$img.tar.zst 2>> $LOG || { echo "FAILED push $img" >> $LOG; continue; }
  rm -f $RELAY/$img.tar.zst
  fi
  for cs in 16384 65536; do $BOXSSH "cd ~/dictval-bench && python3 -I timing_db.py $img $cs tmdb/$img" </dev/null >> $LOG 2>&1 || echo "FAILED timing $img $cs" >> $LOG; done
  $BOXSSH "rm -rf ~/dictval-bench/tmdb/$img" </dev/null 2>> $LOG
  echo "$(date +%T) $img done" >> $LOG
done; done
echo "$(date +%T) ALL FINISHED" >> $LOG
