#!/usr/bin/env python3 -I
"""Evaluate the dictionary-size and training-size sweeps (fold 0 test projects, zstd:3, objects <= 256 KiB, <= 400 files per type)."""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import os, sys, csv, glob, random, collections
from compression import zstd
import taxonomy as T, common as C
rows = C.load_manifest(); fold_of = C.assign_folds(rows)
test = [r for r in rows if fold_of[r['project']] == 0 and r['size'] <= 256 << 10]
rng = random.Random(3); byt = collections.defaultdict(list)
for r in test: byt[r['type']].append(r)
for t in byt: rng.shuffle(byt[t]); byt[t] = byt[t][:400]
def targets(name):
    if name.startswith('type:'): return byt.get(name[5:], [])
    if name == 'generic_text': return [r for t in T.TEXT_TYPES for r in byt.get(t, [])]
    if name.startswith('family:'): return [r for t, f in T.FAMILY.items() if f == name[7:] for r in byt.get(t, [])]
    return []
out = []
dirs = sorted(glob.glob(os.path.join(C.WORK, 'dicts', 'C', 'sweep-*'))) + [os.path.join(C.WORK, 'dicts', 'C', 'f0')]
cache = {}
for d in dirs:
    sw = os.path.basename(d)
    for p in sorted(glob.glob(d + '/*.zdict')):
        n0 = os.path.basename(p)[:-6]; name = n0.replace('_', ':', 1) if n0.split('_')[0] in ('type', 'family', 'ext') else n0
        if sw == 'f0' and name not in ('type:python','type:c','type:js','type:json','type:html','type:md','type:java','type:yaml','type:rust','type:xml','generic_text','family:clike','family:data','family:markup'): continue
        rs = targets(name)
        if not rs: continue
        zd = zstd.ZstdDict(open(p, 'rb').read()); a = b = 0
        for r in rs:
            data = open(r['path'], 'rb').read()
            if r['id'] not in cache: cache[r['id']] = C.stored(len(data), len(zstd.compress(data, level=3)))
            a += cache[r['id']]; b += C.stored(len(data), len(zstd.compress(data, level=3, zstd_dict=zd)))
        out.append(dict(sweep=sw, dict=name, dict_bytes=len(zd.dict_content), files=len(rs), nodict_bytes=a, dict_bytes_out=b, gain_pct=round(100 * (1 - b / a), 2)))
with open(sys.argv[1], 'w', newline='') as f:
    w = csv.DictWriter(f, list(out[0])); w.writeheader(); w.writerows(out)
