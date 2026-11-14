#!/bin/bash
# Local: pull the aggregate CSVs of queue A from the test host into bench/dict-study/validation/raw/ (aggregates only; each file < 1 MiB).
export SSH_AUTH_SOCK=/run/user/1000/keyring/ssh
R=$(dirname "$0")/../../bench/dict-study/validation/raw; H=${DICTVAL_HOST:?set DICTVAL_HOST=user@host in the environment (never in this file)}
scp -q -o BatchMode=yes "$H:dictval/out/arms-*.csv" "$H:dictval/out/zdarms-*.csv" "$H:dictval/out/ftsel-*.json" "$H:dictval/out/shares-*.json" "$H:dictval/out/labels-[hv]*.json" "$R/"
python3 - "$R" <<'PY'
import sys, glob, json, csv, os
R = sys.argv[1]; rows = []
for f in sorted(glob.glob(R + '/shares-*.json')):
    s = json.load(open(f)); n = os.path.basename(f)[7:-5].rsplit('-', 1)
    cd = s.get('cross_dup', {}); ic = s.get('icheck') or {}
    rows.append([n[0], n[1], s['image_bytes'], s['chunks_total'], s['holes'], s['dups'], s['unique'], s['blocks_1MiB_nonhole'], s['unique_bytes'], s['probe_raw_chunks'], s['probe_raw_bytes'],
                 json.dumps({k: v for k, v in cd.items()}, separators=(',', ':')).replace(',', ';'), ic.get('sampled', ''), ic.get('disagree', '')])
w = csv.writer(open(R + '/shares.csv', 'w')); w.writerow(['corpus', 'chunk', 'image_bytes', 'chunks_total', 'holes', 'dups', 'unique', 'blocks_1MiB_nonhole', 'unique_bytes', 'probe_raw_chunks', 'probe_raw_bytes', 'cross_image_dups', 'icheck_sampled', 'icheck_disagree']); w.writerows(rows)
PY
rm -f "$R"/shares-*.json; ls -la "$R"
