#!/usr/bin/env python3 -I
"""Evaluate shipped-set candidates (generic / family / type / hybrid) on all objects by size bucket.  usage: hybrid_eval.py CSV_DIR
Hybrid = per-type level chosen by decide.py on the project-disjoint folds only (the new-ecosystem sets are untouched by that choice)."""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, json, collections
import numpy as np
import taxonomy as T, common as C
d = sys.argv[1]; rng = np.random.default_rng(3)
lvl = {r['type']: r['adopted_level'] for r in csv.DictReader(open(os.path.join(d, 'shipped_set_decision.csv')))}
BK = [(0, 1 << 10, '<1KiB'), (1 << 10, 4 << 10, '1-4KiB'), (4 << 10, 16 << 10, '4-16KiB'), (16 << 10, 64 << 10, '16-64KiB'), (64 << 10, 256 << 10, '64-256KiB'), (256 << 10, 1 << 62, '>=256KiB')]
rows = C.load_manifest(); info = {r['id']: r for r in rows}
acc = collections.defaultdict(list)     # (eco, variant) -> list of (project, size, z3, chosen)
kinds = {'generic text': 'gt', 'family': 'fam', 'type': 'typ', 'hybrid': None}
for s in ['f0', 'f1', 'f2', 'f3', 'full']:
    z = np.load(os.path.join(C.WORK, 'results', f'{s}.npz'), allow_pickle=True); eco = 'new' if s == 'full' else 'main'
    Z = {k: z[k] for k in z.files if k in ('id', 'z3') or (k.startswith('P.') and k.endswith('.z3'))}
    for i, fid in enumerate(Z['id']):
        r = info[fid]; t = r['type']
        if t not in T.TEXT_TYPES + T.BIN_TYPES: continue
        base = int(Z['z3'][i])
        for vn, k in kinds.items():
            if vn == 'hybrid':
                l = lvl.get(t, 'none'); k = 'gt' if l == 'generic' else 'fam' if l == 'family' else 'typ' if l == 'type' else 'ext' if l == 'ext' else None
            v = base if k is None else int(Z[f'P.{k}.z3'][i])
            if k is not None and v < 0: v = base
            acc[(eco, vn)].append((r['project'], r['size'], base, v))
out = []
for (eco, vn), xs in acc.items():
    P = np.array([x[0] for x in xs]); S = np.array([x[1] for x in xs]); A = np.array([x[2] for x in xs], float); B = np.array([x[3] for x in xs], float)
    for lo, hi, lab in BK + [(0, 65536, '<64KiB'), (0, 1 << 62, 'all')]:
        m = (S >= lo) & (S < hi)
        if m.sum() < 5: continue
        up, inv = np.unique(P[m], return_inverse=True); a = np.bincount(inv, A[m]); b = np.bincount(inv, B[m])
        ix = rng.integers(0, len(up), (500, len(up))); g = 1 - b[ix].sum(1) / a[ix].sum(1)
        out.append(dict(eco=eco, variant=vn, bucket=lab, objects=int(m.sum()), saving_pct=round(100 * (1 - b.sum() / a.sum()), 2), ci_lo=round(100 * np.percentile(g, 2.5), 2), ci_hi=round(100 * np.percentile(g, 97.5), 2)))
with open(os.path.join(d, 'shipped_set_candidates.csv'), 'w', newline='') as f:
    w = csv.DictWriter(f, list(out[0])); w.writeheader(); w.writerows(out)
# dictionary inventory for the hybrid (arm P, 'full' set)
man = {(r['arm'], r['name']): r for r in csv.DictReader(open(os.path.join(d, 'dict_manifest.csv'))) if r['set'] == 'full' and r['status'] == 'ok'}
need = collections.OrderedDict()
need['generic_text'] = 0
for t, l in lvl.items():
    fam = T.FAMILY.get(t)
    if l == 'family': need[f'family:{fam}'] = 0
    elif l == 'type': need[f'type:{t}'] = 0
    elif l == 'ext': need[f'type:{t}'] = 0
    elif l == 'generic': need['generic_text'] = 0
# families are also the fallback of nothing here; ext variants only for fortran
inv = []
for n in need:
    r = man.get(('P', n)); inv.append(dict(name=n, dict_id=r['dict_id'] if r else '', dict_bytes=int(r['dict_bytes']) if r else 0, train_projects=r['n_proj'] if r else '', train_bytes=r['train_bytes'] if r else '', licence_mix=r['licence_mix'] if r else ''))
with open(os.path.join(d, 'shipped_set_inventory.csv'), 'w', newline='') as f:
    w = csv.DictWriter(f, list(inv[0])); w.writeheader(); w.writerows(inv)
print(len(inv), 'dictionaries,', sum(x['dict_bytes'] for x in inv), 'bytes')
