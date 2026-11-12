#!/usr/bin/env python3 -I
"""Apply the pre-registered rules per type and list the proposed shipped set.  usage: decide.py CSV_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, collections
import taxonomy as T
d = sys.argv[1]
def rd(fn): return list(csv.DictReader(open(os.path.join(d, fn))))
f = lambda x: float(x) if x not in ('', None) else None
tp = {r['type']: r for r in rd('gains_by_type_lt64k.csv')}; tn = {r['type']: r for r in rd('gains_by_type_lt64k_new_ecosystem.csv')}
pt = rd('paired_tests.csv'); sig = {(r['comparison'], r['type']): r['significant_0.05'] == 'True' for r in pt}
man = [r for r in rd('dict_manifest.csv') if r['set'] == 'full']
size = {(r['arm'], r['name']): int(r['dict_bytes']) for r in man if r['status'] == 'ok'}
rows = []; chosen = {}
for t in T.ALL_TYPES:
    if t not in tp or t in T.CONTROL_TYPES: continue
    r = tp[t]; gt, fam, typ, ext = (f(r.get('P.gt')) or 0.0, f(r.get('P.fam')) or 0.0, f(r.get('P.typ')) or 0.0, f(r.get('P.ext')) or 0.0)
    lvl, g = ('generic', gt) if t in T.TEXT_TYPES else ('none', 0.0)
    if fam - g >= 2 and sig.get(('P: family vs generic', t), False) or (t not in T.TEXT_TYPES and fam >= 2): lvl, g = 'family', fam
    if typ - g >= 2 and sig.get(('P: type vs family', t), False): lvl, g = 'type', typ
    if ext - g >= 2 and sig.get(('P: ext vs type', t), False): lvl, g = 'ext', ext
    cp = f(r.get('C.typ')) or 0.0; cl_gain = cp - typ
    needs_cl = sig.get(('copyleft A/B: perm+copyleft vs perm-only (type dict)', t), False) and cl_gain >= 1
    if g < 3: lvl = 'none (best %.1f %% < 3 %%)' % g
    n = tn.get(t); ng = f(n.get('P.typ')) if n else None
    rows.append((t, T.FAMILY.get(t), lvl, round(g, 1), round(gt, 1), round(fam, 1), round(typ, 1), round(ext, 1), '' if ng is None else round(ng, 1), 'yes (+%.1f pp)' % cl_gain if needs_cl else 'no', int(r['n_projects'])))
import json
level_of = {x[0]: x[2] for x in rows}
with open(os.path.join(d, 'shipped_set_decision.csv'), 'w', newline='') as fh:
    w = csv.writer(fh); w.writerow(['type', 'family', 'adopted_level', 'saving_pct_lt64k', 'generic', 'family_dict', 'type_dict', 'ext_dict', 'type_dict_new_ecosystem', 'copyleft_training_needed', 'test_projects']); w.writerows(rows)
print('| type | family | adopted level | saving % (<64 KiB) | generic | family | type | ext | type, new ecosystem | copyleft training needed | test projects |'); print('|' + '---|' * 11)
for x in rows: print('| ' + ' | '.join(str(v) for v in x) + ' |')
fams = collections.Counter(); tys = []; exts = []
for x in rows:
    if x[2] == 'family': fams[x[1]] += 1
    if x[2] == 'type': tys.append(x[0])
    if x[2] == 'ext': tys.append(x[0]); exts.append(x[0])
print('\nfamilies needed (member types adopting them):', dict(fams)); print('per-type dictionaries:', tys); print('types where an extension split beats the type dictionary:', exts)
print('generic users:', [x[0] for x in rows if x[2] == 'generic']); print('none:', [x[0] for x in rows if x[2].startswith('none')])
