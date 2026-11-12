#!/usr/bin/env python3 -I
"""Chunk-size sensitivity (1, 4, 16 MiB) for files over 1 MiB.  usage: chunk_sens_summary.py OUT_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, json, csv
import numpy as np
import common as C
out = sys.argv[1]; tot = {}; seen4 = set()
for s in ['f0', 'f1', 'f2', 'f3', 'full']:
    j = json.load(open(os.path.join(C.WORK, 'results', f'chunksens-{s}.json')))
    z = np.load(os.path.join(C.WORK, 'results', f'{s}.npz'), allow_pickle=True); ids = {i: k for k, i in enumerate(z['id'])}
    for cs, d in j.items():
        for fid, v in d.items():
            for key in ('z3', 'C.typ.z3', 'z9', 'z19', 'nchunks'):
                tot.setdefault((cs, key), 0); tot[(cs, key)] += v.get(key, 0)
            k = ids[fid]
            if (s, fid) in seen4: continue
            seen4.add((s, fid))
            for key in ('z3', 'C.typ.z3', 'z9', 'z19', 'nchunks'):
                tot.setdefault(('4', key), 0); tot[('4', key)] += int(z[key][k])
rows = []
for cs in ('1', '4', '16'):
    a = tot[(cs, 'z3')]
    rows.append(dict(chunk_MiB=cs, objects=tot[(cs, 'nchunks')], stored_zstd3=a, stored_zstd9=tot[(cs, 'z9')], stored_zstd19=tot[(cs, 'z19')], stored_zstd3_typedict=tot[(cs, 'C.typ.z3')],
                     typedict_gain_pct=round(100 * (1 - tot[(cs, 'C.typ.z3')] / a), 3)))
with open(os.path.join(out, 'chunk_size_sensitivity.csv'), 'w', newline='') as f:
    w = csv.DictWriter(f, list(rows[0])); w.writeheader(); w.writerows(rows)
print(rows)
