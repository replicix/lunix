#!/usr/bin/env python3 -I
"""Aggregate results into small CSVs (OUT dir) and SVG figures.  usage: analyze.py OUT_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, json, collections, math
import numpy as np
from scipy import stats
import taxonomy as T, common as C

OUT = sys.argv[1]; os.makedirs(OUT, exist_ok=True)
BUCKETS = [(0, 1 << 10, '<1KiB'), (1 << 10, 4 << 10, '1-4KiB'), (4 << 10, 16 << 10, '4-16KiB'), (16 << 10, 64 << 10, '16-64KiB'),
           (64 << 10, 256 << 10, '64-256KiB'), (256 << 10, 4 << 20, '256KiB-4MiB'), (4 << 20, 1 << 62, '>4MiB')]
rng = np.random.default_rng(42)
rows = C.load_manifest(); fold_of = C.assign_folds(rows); rid = {r['id']: i for i, r in enumerate(rows)}

def load():
    cols = collections.defaultdict(list); meta = []
    for s in ['f0', 'f1', 'f2', 'f3', 'full']:
        p = os.path.join(C.WORK, 'results', f'{s}.npz')
        if not os.path.exists(p): continue
        z = np.load(p, allow_pickle=True); n = len(z['id'])
        for k in z.files:
            if k.startswith('M.'): continue
            cols[k].append(z[k])
        for k in set(cols) - set(z.files):
            if len(cols[k]) < len(cols['id']): cols[k].append(np.full(n, -1))
        meta += [(s, 'new' if s == 'full' else 'main')] * n
    D = {}
    for k, v in cols.items():
        try: D[k] = np.concatenate(v)
        except Exception: pass
    ids = D['id']; D['set'] = np.array([m[0] for m in meta]); D['eco'] = np.array([m[1] for m in meta])
    R = [rows[rid[i]] for i in ids]
    D['type'] = np.array([r['type'] for r in R]); D['project'] = np.array([r['project'] for r in R]); D['size'] = np.array([r['size'] for r in R])
    D['policy'] = np.array([r['policy'] for r in R]); D['ext'] = np.array([r['ext'] for r in R])
    return D

D = load(); N = len(D['id'])
BUCKET = np.array([next(b[2] for b in BUCKETS if b[0] <= s < b[1]) for s in D['size']])
TEXT = np.array([t in T.TEXT_TYPES for t in D['type']]); BINT = np.array([t in T.BIN_TYPES for t in D['type']])
GROUPS = {'text': TEXT, 'binary': BINT, 'all-compressible': TEXT | BINT}

def boot_gain(a, b, proj, B=1000):
    """gain = 1 - sum(b)/sum(a); 95% CI by resampling projects."""
    up = np.unique(proj); inv = np.searchsorted(up, proj)
    A = np.bincount(inv, a, len(up)); Bs = np.bincount(inv, b, len(up))
    if len(up) < 2: return 1 - Bs.sum() / A.sum(), np.nan, np.nan
    ix = rng.integers(0, len(up), (B, len(up))); g = 1 - Bs[ix].sum(1) / np.maximum(A[ix].sum(1), 1)
    return 1 - Bs.sum() / A.sum(), np.percentile(g, 2.5), np.percentile(g, 97.5)

def cmp_rows(name, base_key, var_key, mask, bucket_list=True, extra=None):
    out = []
    for bl in ([None] + [b[2] for b in BUCKETS] if bucket_list else [None]):
        m = mask & (D[base_key] >= 0) & (D[var_key] >= 0)
        if bl: m = m & (BUCKET == bl)
        if m.sum() < 5: continue
        a = D[base_key][m].astype(float); b = D[var_key][m].astype(float)
        g, lo, hi = boot_gain(a, b, D['project'][m])
        pf = 1 - b / a
        out.append(dict(name=name, bucket=bl or 'all', n_files=int(m.sum()), n_projects=len(np.unique(D['project'][m])), bytes_base=int(a.sum()), bytes_var=int(b.sum()),
                        gain_bytes_pct=round(100 * g, 2), ci_lo=round(100 * lo, 2), ci_hi=round(100 * hi, 2), gain_files_mean_pct=round(100 * pf.mean(), 2), gain_files_median_pct=round(100 * np.median(pf), 2),
                        frac_files_better=round(float((b < a).mean()), 3), frac_files_worse=round(float((b > a).mean()), 3), **(extra or {})))
    return out

def write(fn, rs):
    if not rs: return
    keys = list(dict.fromkeys(k for r in rs for k in r))
    with open(os.path.join(OUT, fn), 'w', newline='') as f:
        w = csv.DictWriter(f, keys); w.writeheader(); w.writerows(rs)

# ---- 1. corpus tables
tm = collections.defaultdict(lambda: dict(files=0, bytes=0, projects=set(), perm=0, copyleft=0, excluded=0, sizes=[]))
for r in rows:
    d = tm[r['type']]; d['files'] += 1; d['bytes'] += r['size']; d['projects'].add(r['project']); d[r['policy']] += r['size']; d['sizes'].append(r['size'])
write('corpus_types.csv', [dict(type=t, family=T.FAMILY.get(t, ''), files=d['files'], bytes=d['bytes'], projects=len(d['projects']), p10=int(np.percentile(d['sizes'], 10)), median=int(np.median(d['sizes'])),
      p90=int(np.percentile(d['sizes'], 90)), max=max(d['sizes']), perm_bytes=d['perm'], copyleft_bytes=d['copyleft'], excluded_bytes=d['excluded'],
      perm_projects=len({r['project'] for r in rows if r['type'] == t and r['policy'] == 'perm'})) for t, d in sorted(tm.items())])
pm = collections.defaultdict(lambda: dict(files=0, bytes=0, spdx=collections.Counter()))
for r in rows: pm[r['project']]['files'] += 1; pm[r['project']]['bytes'] += r['size']; pm[r['project']]['spdx'][r['file_spdx']] += r['size']; pm[r['project']]['eco'] = r['eco']
lock = {}
for fn in ('sources.lock.new.tsv', 'sources.lock.extra.tsv'):
    p = os.path.join(C.CORPUS, fn)
    if os.path.exists(p):
        for l in open(p):
            q = l.rstrip('\n').split('\t')
            if len(q) >= 4: lock[q[0]] = q
write('corpus_projects.csv', [dict(project=p, url=lock.get(p, ['', ''])[1], commit_or_hash=(lock.get(p, ['', '', ''])[2] or '')[:40], licence_main=pm[p]['spdx'].most_common(1)[0][0], eco=pm[p]['eco'], fold=fold_of[p],
      files=pm[p]['files'], bytes=pm[p]['bytes']) for p in sorted(pm)])
# size distribution per type and bucket (file counts and bytes)
sd = []
for t in T.ALL_TYPES:
    fs = [r['size'] for r in rows if r['type'] == t]
    if not fs: continue
    d = dict(type=t, files=len(fs))
    for lo, hi, nm in BUCKETS: d[nm + '_files'] = sum(lo <= s < hi for s in fs)
    for lo, hi, nm in BUCKETS: d[nm + '_bytes'] = sum(s for s in fs if lo <= s < hi)
    sd.append(d)
write('size_distribution.csv', sd)

# ---- 2. main comparisons at zstd:3 (and all levels)
VARS = [('gt', 'generic text'), ('ga', 'generic all'), ('fam', 'family'), ('typ', 'type'), ('ext', 'ext+type')]
res = []
for gname, gm in GROUPS.items():
    for eco in ('main', 'new'):
        m0 = gm & (D['eco'] == eco)
        for arm in 'PC':
            for kind, kn in VARS:
                for L in (1, 3, 9, 19):
                    res += cmp_rows(f'{arm}.{kind}.z{L}', f'z{L}', f'{arm}.{kind}.z{L}', m0, extra=dict(group=gname, eco=eco, arm=arm, kind=kn, level=L, vs=f'nodict z{L}'))
        for L in (1, 9, 19):
            res += cmp_rows(f'nodict.z{L}_vs_z3', 'z3', f'z{L}', m0, extra=dict(group=gname, eco=eco, arm='-', kind='nodict', level=L, vs='nodict z3'))
        for ref in ('br11', 'xz9e'):
            res += cmp_rows(f'{ref}_vs_z3', 'z3', ref, m0, extra=dict(group=gname, eco=eco, arm='-', kind=ref, level=0, vs='nodict z3'))
        # dictionary at z3 against higher level without dictionary
        for L in (9, 19):
            res += cmp_rows(f'C.typ.z3_vs_nodict.z{L}', f'z{L}', 'C.typ.z3', m0, extra=dict(group=gname, eco=eco, arm='C', kind='type', level=3, vs=f'nodict z{L}'))
        res += cmp_rows('raw_vs_z3', 'raw', 'z3', m0, extra=dict(group=gname, eco=eco, arm='-', kind='nodict', level=3, vs='raw'))
write('gains_overall.csv', res)

# ---- 2b. size ranges used by the decision rules: 1-64 KiB and < 64 KiB (text, main and new)
rg = []
for gname, gm in (('text', TEXT), ('binary', BINT)):
    for eco in ('main', 'new'):
        for lab, lo, hi in (('1-64KiB', 1024, 65536), ('<64KiB', 0, 65536)):
            m0 = gm & (D['eco'] == eco) & (D['size'] >= lo) & (D['size'] < hi)
            for arm in 'PC':
                for kind, kn in VARS:
                    r = cmp_rows(f'{arm}.{kind}.z3', 'z3', f'{arm}.{kind}.z3', m0, False, extra=dict(group=gname, eco=eco, arm=arm, kind=kn, level=3, range=lab))
                    rg += r
            for L in (9, 19):
                rg += cmp_rows(f'z{L}', 'z3', f'z{L}', m0, False, extra=dict(group=gname, eco=eco, arm='-', kind='nodict', level=L, range=lab))
write('gains_size_ranges.csv', rg)

# ---- 3. per type table at z3 (main folds), arms P and C
pt = []
for t in T.ALL_TYPES:
    m0 = (D['type'] == t) & (D['eco'] == 'main')
    if m0.sum() < 5: continue
    row = dict(type=t, n_files=int(m0.sum()), n_projects=len(np.unique(D['project'][m0])), bytes_nodict_z3=int(D['z3'][m0].sum()), ratio_z3=round(D['raw'][m0].sum() / D['z3'][m0].sum(), 3))
    for arm in 'PC':
        for kind, _ in VARS:
            r = cmp_rows('x', 'z3', f'{arm}.{kind}.z3', m0, False)
            if r: row[f'{arm}.{kind}'] = r[0]['gain_bytes_pct']; row[f'{arm}.{kind}.ci'] = f"{r[0]['ci_lo']}..{r[0]['ci_hi']}"
    for L in (9, 19):
        r = cmp_rows('x', 'z3', f'z{L}', m0, False); row[f'nodict_z{L}'] = r[0]['gain_bytes_pct'] if r else ''
    for key in ('P.typ.name', 'C.typ.name'):
        if key in D: row[key] = collections.Counter(D[key][m0]).most_common(1)[0][0]
    pt.append(row)
write('gains_by_type_z3.csv', pt)
# per type, objects < 64 KiB only (the decision-relevant regime), main folds and new ecosystem
for eco, fn in (('main', 'gains_by_type_lt64k.csv'), ('new', 'gains_by_type_lt64k_new_ecosystem.csv')):
    rr = []
    for t in T.ALL_TYPES:
        m0 = (D['type'] == t) & (D['eco'] == eco) & (D['size'] < 65536)
        if m0.sum() < 5: continue
        row = dict(type=t, n_files=int(m0.sum()), n_projects=len(np.unique(D['project'][m0])), median_size=int(np.median(D['size'][m0])), ratio_z3=round(D['raw'][m0].sum() / D['z3'][m0].sum(), 3))
        for arm in 'PC':
            for kind, _ in VARS:
                r = cmp_rows('x', 'z3', f'{arm}.{kind}.z3', m0, False)
                if r: row[f'{arm}.{kind}'] = r[0]['gain_bytes_pct']; row[f'{arm}.{kind}.ci'] = f"{r[0]['ci_lo']}..{r[0]['ci_hi']}"
        for L in (9, 19):
            r = cmp_rows('x', 'z3', f'z{L}', m0, False); row[f'nodict_z{L}'] = r[0]['gain_bytes_pct'] if r else ''
        r = cmp_rows('x', 'z9', 'C.typ.z3', m0, False); row['C.typ.z3_vs_z9'] = r[0]['gain_bytes_pct'] if r else ''
        rr.append(row)
    write(fn, rr)
# new ecosystem per type
pn = []
for t in T.ALL_TYPES:
    m0 = (D['type'] == t) & (D['eco'] == 'new')
    if m0.sum() < 5: continue
    row = dict(type=t, n_files=int(m0.sum()), n_projects=len(np.unique(D['project'][m0])))
    for arm in 'PC':
        for kind in ('gt', 'fam', 'typ'):
            r = cmp_rows('x', 'z3', f'{arm}.{kind}.z3', m0, False)
            if r: row[f'{arm}.{kind}'] = r[0]['gain_bytes_pct']
    pn.append(row)
write('gains_new_ecosystem_by_type_z3.csv', pn)

# ---- 4. paired Wilcoxon + Holm: granularity steps and copyleft A/B, per type (z3, arm P unless stated)
tests = []
def paired(name, ka, kb, m0, tag):
    m = m0 & (D[ka] >= 0) & (D[kb] >= 0)
    if m.sum() < 20: return
    a = D[ka][m].astype(float); b = D[kb][m].astype(float); d = a - b
    if np.all(d == 0): p = 1.0
    else:
        try: p = stats.wilcoxon(d, zero_method='zsplit', alternative='two-sided').pvalue
        except Exception: p = 1.0
    g, lo, hi = boot_gain(a, b, D['project'][m])
    tests.append(dict(comparison=tag, type=name, n_files=int(m.sum()), n_projects=len(np.unique(D['project'][m])), gain_of_B_over_A_bytes_pct=round(100 * g, 2), ci_lo=round(100 * lo, 2), ci_hi=round(100 * hi, 2),
                      frac_B_smaller=round(float((b < a).mean()), 3), frac_B_larger=round(float((b > a).mean()), 3), p_raw=p))
for t in T.TEXT_TYPES + T.BIN_TYPES:
    m0 = (D['type'] == t) & (D['eco'] == 'main') & (D['size'] < 65536)     # decision rules R2/R4 are about objects < 64 KiB
    paired(t, 'P.gt.z3', 'P.fam.z3', m0, 'P: family vs generic') if t in T.TEXT_TYPES else None
    paired(t, 'P.fam.z3', 'P.typ.z3', m0, 'P: type vs family')
    paired(t, 'P.typ.z3', 'P.ext.z3', m0, 'P: ext vs type')
    paired(t, 'z3', 'P.typ.z3', m0, 'P: type dict vs none')
    paired(t, 'P.typ.z3', 'C.typ.z3', m0, 'copyleft A/B: perm+copyleft vs perm-only (type dict)')
    paired(t, 'P.fam.z3', 'C.fam.z3', m0, 'copyleft A/B: perm+copyleft vs perm-only (family dict)')
    paired(t, 'z9', 'C.typ.z3', m0, 'C: type dict z3 vs nodict z9')
ps = np.array([x['p_raw'] for x in tests]); order = np.argsort(ps); holm = np.empty(len(ps)); run = 0
for rank, i in enumerate(order):
    run = max(run, min(1.0, (len(ps) - rank) * ps[i])); holm[i] = run
for x, h in zip(tests, holm): x['p_holm'] = float(h); x['p_raw'] = float(x['p_raw']); x['significant_0.05'] = bool(h < 0.05)
write('paired_tests.csv', tests)

# ---- 5. dictionary-vs-level by bucket (text, main), arm C type dict
lv = []
for bl in [b[2] for b in BUCKETS]:
    m0 = TEXT & (D['eco'] == 'main') & (BUCKET == bl)
    if m0.sum() < 5: continue
    d = dict(bucket=bl, n_files=int(m0.sum()))
    for nm, k in [('nodict_z1', 'z1'), ('nodict_z9', 'z9'), ('nodict_z19', 'z19'), ('br11', 'br11'), ('xz9e', 'xz9e'), ('C.typ_z1', 'C.typ.z1'), ('C.typ_z3', 'C.typ.z3'), ('C.typ_z9', 'C.typ.z9'), ('C.typ_z19', 'C.typ.z19'),
                  ('C.fam_z3', 'C.fam.z3'), ('C.gt_z3', 'C.gt.z3'), ('P.typ_z3', 'P.typ.z3')]:
        if k not in D: continue
        m = m0 & (D['z3'] >= 0) & (D[k] >= 0)
        d[nm] = round(100 * (1 - D[k][m].sum() / D['z3'][m].sum()), 2) if m.sum() else ''
    lv.append(d)
write('level_vs_dict_text.csv', lv)

# ---- 6. matrix, oracle (M.*) from per-set files
mo = []
for s in ['f0', 'f1', 'f2', 'f3', 'full']:
    p = os.path.join(C.WORK, 'results', f'{s}.npz')
    if not os.path.exists(p): continue
    z = np.load(p, allow_pickle=True); mk = [k.replace('M.generic:', 'M.generic_') for k in z.files if k.startswith('M.')]; zf = {k.replace('M.generic:', 'M.generic_'): k for k in z.files if k.startswith('M.')}
    if not mk: continue
    Mx = np.stack([z[zf[k]] for k in mk], 1); ok = (Mx >= 0).all(1); z3 = z['z3']; ids = z['id']
    for i in np.where(ok)[0]:
        r = rows[rid[ids[i]]]; best = Mx[i].min(); bj = int(Mx[i].argmin())
        mo.append((s, r['project'], r['type'], r['size'], int(z3[i]), int(best), mk[bj][2:], int(Mx[i][mk.index('M.type:' + r['type'])]) if 'M.type:' + r['type'] in mk else -1,
                   int(Mx[i][mk.index('M.generic_text')]) if 'M.generic_text' in mk else -1))
if mo:
    with open(os.path.join(OUT, 'oracle_choice_by_type.csv'), 'w', newline='') as f:
        w = csv.writer(f); w.writerow(['type', 'n_files', 'oracle_gain_pct_vs_nodict', 'own_type_dict_gain_pct', 'generic_text_gain_pct', 'oracle_picks_own_type_frac', 'oracle_picks_none_frac', 'top_oracle_choices'])
        byt = collections.defaultdict(list)
        for x in mo: byt[x[2]].append(x)
        for t, xs in sorted(byt.items()):
            a = sum(x[4] for x in xs); ob = sum(min(x[5], x[4]) for x in xs)
            own = sum(x[7] if x[7] >= 0 else x[4] for x in xs); gt_ = sum(x[8] if x[8] >= 0 else x[4] for x in xs)
            c = collections.Counter('none' if x[4] <= x[5] else x[6] for x in xs)
            w.writerow([t, len(xs), round(100 * (1 - ob / a), 2), round(100 * (1 - own / a), 2), round(100 * (1 - gt_ / a), 2), round(sum(x[6] == 'type:' + t for x in xs if x[4] > x[5]) / len(xs), 3), round(c['none'] / len(xs), 3),
                        ';'.join(f'{k}={v}' for k, v in c.most_common(4))])
# ---- 7. ratio rows for figures
json.dump({'n_files': int(N)}, open(os.path.join(OUT, 'analyze_meta.json'), 'w'))
print('analyze done', N)
