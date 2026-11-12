#!/usr/bin/env python3 -I
"""Selection summaries from select_dict.py output.  usage: analyze_sel.py WORK_OUT OUT_DIR"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import sys, os, csv, collections
import numpy as np
src, out = sys.argv[1], sys.argv[2]; rng = np.random.default_rng(7)
L = list(csv.DictReader(open(os.path.join(src, 'sel_loss.csv'))))
A = list(csv.DictReader(open(os.path.join(src, 'sel_acc.csv'))))
CF = list(csv.DictReader(open(os.path.join(src, 'sel_confusion_all.csv'))))
def w(fn, rs):
    if not rs: return
    with open(os.path.join(out, fn), 'w', newline='') as f:
        d = csv.DictWriter(f, list(rs[0])); d.writeheader(); d.writerows(rs)
# accuracy
acc = collections.defaultdict(lambda: [0, 0])
for r in A:
    g = 'new-ecosystem' if r['set'] == 'full' else 'project-disjoint folds'
    acc[(r['strategy'], g)][0] += int(r['n']); acc[(r['strategy'], g)][1] += int(r['correct'])
w('selection_accuracy.csv', [dict(strategy=k[0], test_set=k[1], files=v[0], accuracy_pct=round(100 * v[1] / v[0], 2)) for k, v in sorted(acc.items())])
# loss versus oracle
def boot(proj, a, b):
    up = np.unique(proj); inv = np.searchsorted(up, proj); Aa = np.bincount(inv, a, len(up)); Bb = np.bincount(inv, b, len(up))
    if len(up) < 2: return np.nan, np.nan
    ix = rng.integers(0, len(up), (500, len(up))); g = (Bb[ix].sum(1) - Aa[ix].sum(1)) / Aa[ix].sum(1) * 100
    return np.percentile(g, 2.5), np.percentile(g, 97.5)
cols = {k: np.array([r[k] for r in L]) for k in ('strategy', 'set', 'type', 'project')}
num = {k: np.array([int(r[k]) for r in L]) for k in ('size', 'z3_nodict', 'oracle', 'chosen')}
rows_lt64, rows_all = [], []
true_ = {}
for s in ('true',):
    m = cols['strategy'] == s; true_ = dict(zip(zip(cols['project'][m], cols['type'][m], num['size'][m], num['z3_nodict'][m]), num['chosen'][m]))
for strat in sorted(set(cols['strategy'])):
    for grp, cond in (('project-disjoint', cols['set'] != 'full'), ('new-ecosystem', cols['set'] == 'full')):
        for lab, sz, store in (('<=64KiB', 64 << 10, rows_lt64), ('<=256KiB', 256 << 10, rows_all)):
            m = (cols['strategy'] == strat) & cond & (num['size'] <= sz) & np.isin(cols['type'], ['png', 'jpeg', 'zip', 'gz', 'video'], invert=True)
            if m.sum() < 10: continue
            ch, orc, nod = num['chosen'][m].astype(float), num['oracle'][m].astype(float), num['z3_nodict'][m].astype(float)
            lo, hi = boot(cols['project'][m], orc, ch)
            store.append(dict(strategy=strat, test_set=grp, objects=int(m.sum()), gain_vs_no_dict_pct=round(100 * (1 - ch.sum() / nod.sum()), 2), oracle_gain_vs_no_dict_pct=round(100 * (1 - orc.sum() / nod.sum()), 2),
                              loss_vs_oracle_pct=round(100 * (ch.sum() / orc.sum() - 1), 2), ci_lo=round(lo, 2), ci_hi=round(hi, 2)))
w('selection_loss_lt64k_all.csv', rows_lt64); w('selection_loss_le256k_all.csv', rows_all)
w('selection_loss_lt64k.csv', [r for r in rows_lt64 if r['test_set'] == 'project-disjoint'])
# per-type recall and top confusions
for strat in ('nb', 'magic', 'ext0', 'ext1', 'ext2', 'nbx0', 'nbx1', 'nbx2', 'cas0', 'cas1', 'cas2'):
    cnt = collections.defaultdict(int); tot = collections.defaultdict(int)
    for r in CF:
        if r['strategy'] != strat: continue
        n = int(r['n']); tot[r['true']] += n
        if r['pred'] == r['true']: cnt[r['true']] += n
    w(f'selection_recall_{strat}.csv', [dict(type=t, files=tot[t], recall_pct=round(100 * cnt[t] / tot[t], 1)) for t in sorted(tot)])
for strat in ('nb', 'magic', 'ext0'):
    off = sorted([r for r in CF if r['strategy'] == strat and r['pred'] != r['true']], key=lambda r: -int(r['n']))[:40]
    w(f'selection_top_confusions_{strat}.csv', [dict(true=r['true'], predicted=r['pred'], files=r['n']) for r in off])
