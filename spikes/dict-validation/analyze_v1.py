#!/usr/bin/env python3
"""Final analysis of the validation study (PREREGISTERED.md sections 1, 4).  Test host only, per-chunk inputs never leave it.
usage (from ~/dictval):  taskset -c 14-15 python3 -I ana/analyze.py ana/out [B]
inputs   work/res/stored-*.npz, zdstored-*.npz (raw devices); work/res/<db>-<cs>.db.bin, zd-<db>-<cs>.npz, cv-<db>-<tag>.db.bin (databases);
         ana/timing.csv (= raw/timing.csv), ana/out/families.csv (= raw/families.csv, written before this script first ran)
outputs  out/tests.csv      every family test (1 MiB and 16 MiB blocks): S, percentile and BCa 95 % interval, LB, Wilcoxon p, rank-biserial,
                            Holm p within the family, cap ratios, delta-cores, N, pass flags, for the strict and the lenient reading
         out/desc.csv       point estimates and BCa intervals of every arm against the counterfactual, per population and class
         out/votes.csv      votes and verdicts per decision, reading and chunk size
         out/meta.json      counterfactual choices, timing-cell mapping, B, seeds
Definitions: S = 1 - stored(A)/stored(B) over the population (bytes-weighted); unit of resampling and pairing = 1 MiB origin block
(Lunix views; >> 4 for the 16 MiB sensitivity) or file (Constellation views); B = 10,000 block-bootstrap resamples seeded from 20261112;
LB = BCa lower end; N = LB - 0.1 pp x delta-cores; cap: compress and decompress throughput of A >= 0.8 x comparator."""
import sys, os, csv, json, hashlib, time
import numpy as np
from scipy import stats
ROOT = os.path.expanduser('~/dictval'); WORK = os.path.join(ROOT, 'work'); RES = os.path.join(WORK, 'res'); ANA = os.path.join(ROOT, 'ana')
OUT = sys.argv[1]; B = int(sys.argv[2]) if len(sys.argv) > 2 else 10000
MASTER = 20261112; LAMBDA = 0.1; CAP = 0.8; HALFMAX = 1.5; MINCLS = 200
RAW = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu', 'vm-debian', 'vm-fedora']; ELIG = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu']
DSETS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; ENG = ['pg', 'my', 'sq']
DB = ['db-%s-%s' % (d, e) for d in DSETS for e in ENG]; CTL = ['db-ctl-pgbench-pg', 'db-ctl-sysbench-my']
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]); MAXV = 0xFFFFFFFF
LAB = {0: 'unmapped', 5: 'unmapped', 1: 'data', 2: 'dir', 3: 'meta', 4: 'journal', 6: 'dbpage', 7: 'wal', 8: 'dbother', 9: 'otherpart', 10: 'ptable', 255: 'mixed'}
LABV = np.array([LAB.get(i, 'unmapped') for i in range(256)])
NMV = np.array([{6: 'dbpage', 7: 'wal', 8: 'dbother'}.get(i, 'other') for i in range(256)])
def seed(*p): return int.from_bytes(hashlib.sha256((str(MASTER) + '|' + '|'.join(map(str, p))).encode()).digest()[:8], 'little')
def log(*a): print(time.strftime('%H:%M:%S'), *a, flush=True)
# ---------------------------------------------------------------- timing, counterfactual, cap
TM = {}
for r in csv.DictReader(open(os.path.join(ANA, 'timing.csv'))): TM[(r['cell'], r['arm'], int(r['level']), r['op'])] = float(r['median'])
TARGET = {'lunix': 5.0, 'constellation': 1.0}
def tarm(arm):
    """study arm -> (timing arm, level).  CF arms behind the probe are z1/z3 on Lunix cells."""
    a, L = (arm.split('@') + ['0'])[:2]; L = int(L)
    if a in ('z1', 'z3', 'z2', 'z4', 'z1off', 'z3off'): return a, int(a[1])
    a = {'pv-s1': 'pv', 'pv-s2': 'pv', 'pv-s3': 'pv', 'pv': 'pv', 'pvc-s1': 'pv-class', 'pvclass-s1': 'pv-class', 'dbd': 'db', 'dbs': 'db'}.get(a, a)
    return a, L
def cells(view, kind, cs, corpus):
    """timing cells that stand for a population (strict: every applicable cell, worst value used)."""
    if kind == 'raw': return ['vm-ubuntu-%d' % cs]                                   # T2: vm-ubuntu stands in for every raw-device corpus
    if view == 'constellation':
        tag = {1048576: '1m', 4194304: '4m'}.get(cs)
        if tag is None: return []
        if corpus.startswith('db-se-'): return ['db-se-askubuntu-pg-' + tag]
        if corpus.startswith('db-tlc'): return ['db-tlc-2024q1-pg-' + tag]
        return ['db-se-askubuntu-pg-' + tag, 'db-tlc-2024q1-pg-' + tag]              # geonames, controls: worst of both (strict)
    if cs == 65536: return ['vm-ubuntu-65536']                                       # no DB cell at 64 KiB: vm-ubuntu stand-in
    if corpus.startswith('db-se-'): return ['db-se-askubuntu-pg-16384']
    if corpus.startswith('db-tlc'): return ['db-tlc-2024q1-pg-16384']
    return ['db-se-askubuntu-pg-16384', 'db-tlc-2024q1-pg-16384']
def lenient_cells(view, kind, cs, corpus):
    c = cells(view, kind, cs, corpus)
    return [x for x in c if 'tlc' in x] or c if corpus.startswith('db-geonames') else c
def cost(cell, arm, op):
    a, L = tarm(arm)
    for cand in ((a, L), ('pv', L) if a in ('db', 'pv-class') else None):                # single-dictionary stand-in where a cell lacks the arm
        if cand and (cell, cand[0], cand[1], op) in TM: return TM[(cell, cand[0], cand[1], op)], cand != (a, L)
    return None, False
def capinfo(cl, view, arm, comp):
    """worst throughput ratios (compress, decompress) and the largest delta-cores of arm against comp over the cells."""
    if not cl: return None, None, None, ''
    rc, rd, dc, notes = [], [], [], set()
    for cell in cl:
        ca, sa = cost(cell, arm, 'compress'); cb, sb = cost(cell, comp, 'compress'); da, _ = cost(cell, arm, 'decompress'); db_, _ = cost(cell, comp, 'decompress')
        if None in (ca, cb, da, db_): notes.add('untimed:' + cell); continue
        if sa or sb: notes.add('stand-in arm')
        rc.append(cb / ca); rd.append(db_ / da); dc.append(max(0.0, ca - cb) * TARGET[view])
    if not rc: return None, None, None, ';'.join(sorted(notes))
    return min(rc), min(rd), max(dc), ';'.join(sorted(notes))
def cf_for(view, kind, cs, corpus):
    """Counterfactual: z3 behind the probe if it passes the cap against z1 on every cell (and stores less, checked by the caller), else z1;
    Constellation: z3 (the protocol names zstd:3; z4 passing the cap on database objects is reported as a sensitivity)."""
    if view == 'constellation': return 'z3'
    rc, rd, _, _ = capinfo(cells(view, kind, cs, corpus), view, 'z3', 'z1')
    return 'z3' if rc is not None and rc >= CAP and rd >= CAP else 'z1'
# ---------------------------------------------------------------- loaders: arms (per-chunk stored bytes), unit, length, class, masks
def load_raw(c, cs):
    st = np.load(os.path.join(RES, 'stored-%s-%d.npz' % (c, cs))); zd = np.load(os.path.join(RES, 'zdstored-%s-%d.npz' % (c, cs)))
    A = {str(k): st['stored'][i].astype(np.int64) for i, k in enumerate(st['arms'])}
    for i, k in enumerate(zd['arms']): A[str(k)] = zd['stored'][i].astype(np.int64)
    n = len(st['length']); M = {'full': np.ones(n, bool)}
    for s in (1, 2, 3):
        m = np.ones(n, bool); m[st['sample_smp-s%d-f0.01' % s]] = False; M['pv-s%d' % s] = m
    for f in ('0.001', '0.1'):
        if 'sample_smp-s1-f' + f not in st: continue
        m = np.ones(n, bool); m[st['sample_smp-s1-f' + f]] = False; M['sw-f' + f] = m
    man = {int(l.split('\t')[0]): l.split('\t')[2] for l in open(os.path.join(WORK, 'zd', 'manifest.tsv')) if l.strip()}
    sel = zd['ft_sel']; lab = LABV[st['label']]
    typ = np.where(lab == 'data', np.array(['data:' + man.get(int(x), 'none') if x else 'data:none' for x in range(256)])[sel], '')
    return dict(arms=A, unit=st['block'].astype(np.int64), length=st['length'].astype(np.int64), cls=lab, typ=typ, masks=M, kind='raw', view='lunix')
def colsof(p):
    names = open(p + '.cols').read().split(); a = np.fromfile(p, dtype=np.uint32).reshape(-1, len(names)); return {k: a[:, i].astype(np.int64) for i, k in enumerate(names)}
def load_dbl(name, cs):
    b = os.path.join(WORK, '%s-%d' % (name, cs)); rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64)
    C = colsof(os.path.join(RES, '%s-%d.db.bin' % (name, cs))); ZD = np.load(os.path.join(RES, 'zd-%s-%d.npz' % (name, cs)))
    praw = C['probe'] >= 0.94 * np.minimum(L, 4096); beh = lambda z: np.where(praw, L, np.minimum(z, L))     # as dbagg.py
    A = {'none': L, 'z1off': np.minimum(C['z1'], L), 'z3off': np.minimum(C['z3'], L), 'z1': beh(C['z1']), 'z3': beh(C['z3'])}
    for lv in (1, 3):
        zl = np.minimum(C['z%d' % lv], L); ft = ZD['ft%d' % lv].astype(np.int64); orc = ZD['orc%d' % lv].astype(np.int64)
        A['ft@%d' % lv] = np.where(praw, L, np.where(ft == MAXV, zl, np.minimum(ft, L)))
        A['oracle@%d' % lv] = np.where(praw, L, np.minimum(np.minimum(zl, np.minimum(orc, L)), np.minimum(C['dbd@%d' % lv], L)))
        for d in ('dbd', 'dbs'): A['%s@%d' % (d, lv)] = beh(C['%s@%d' % (d, lv)])
        for s in (1, 2, 3): A['pv-s%d@%d' % (s, lv)] = beh(C['pv%d@%d' % (s, lv)])
        cc = C['class']; z = C['z%d' % lv]
        pz = np.where(cc == 1, C.get('pv1m@%d' % lv, z), np.where(cc == 2, C.get('pv1t@%d' % lv, z), np.where(cc == 3, C.get('pv1b@%d' % lv, z), z)))
        A['pvc-s1@%d' % lv] = beh(pz)
    n = len(L); M = {'full': np.ones(n, bool)}
    for s in (1, 2, 3):
        m = np.ones(n, bool); m[np.load(os.path.join(RES, 'pvsample-%s-%d-s%d-0.01.npy' % (name, cs, s)))] = False; M['pv-s%d' % s] = m
    lab = LABV[np.fromfile(b + '.cls', dtype=np.uint8)]
    return dict(arms=A, unit=(rec['off'] >> 20).astype(np.int64), length=L, cls=lab, typ=None, masks=M, kind='db', view='lunix')
def load_cv(name, tag):
    b = os.path.join(WORK, 'cv-%s-%s' % (name, tag)); rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64)
    C = colsof(os.path.join(RES, 'cv-%s-%s.db.bin' % (name, tag))); pc = NMV[np.fromfile(b + '.pcls', dtype=np.uint8)]
    z3 = C['z3@3'] if 'z3@3' in C else C['z3']
    A = {'none': L, 'z3': np.minimum(z3, L), 'dbd@3': np.minimum(C['dbd@3'], L), 'dbs@3': np.minimum(C['dbs@3'], L)}
    for s in (1, 2, 3): A['pv-s%d@3' % s] = np.minimum(C['pv%d@3' % s], L)
    n = len(L); M = {'full': np.ones(n, bool), 'all': np.ones(n, bool)}
    for s in (1, 2, 3):
        m = np.ones(n, bool); m[np.load(os.path.join(RES, 'cvpvsample-%s-%s-s%d.npy' % (name, tag, s)))] = False; M['pv-s%d' % s] = m
    sz = json.load(open(b + '.shares.json'))['object_size']
    return dict(arms=A, unit=rec['id'].astype(np.int64), length=L, cls=pc, typ=None, masks=M, kind='db', view='constellation', size=sz)
# ---------------------------------------------------------------- bootstrap engine
class Pop:
    """block sums of every arm over one population, 10,000 paired block-bootstrap resamples (same resamples for every arm)."""
    def __init__(self, key, unit, mask, arms, shift=0):
        u = unit[mask] >> shift; uniq, inv = np.unique(u, return_inverse=True); self.nb = len(uniq); self.names = list(arms)
        self.M = np.stack([np.bincount(inv, weights=arms[a][mask].astype(np.float64), minlength=self.nb) for a in self.names], axis=1)
        self.tot = self.M.sum(axis=0); self.col = {a: i for i, a in enumerate(self.names)}
        rng = np.random.default_rng(seed('boot', key, shift)); T = []; bs = max(1, min(1000, 4_000_000 // max(1, self.nb)))
        for s in range(0, B, bs):
            n = min(bs, B - s); idx = rng.integers(0, self.nb, (n, self.nb))
            cnt = np.bincount((np.arange(n)[:, None] * self.nb + idx).ravel(), minlength=n * self.nb).reshape(n, self.nb).astype(np.float64)
            T.append(cnt @ self.M)
        self.T = np.concatenate(T)
    def test(self, a, c, wil=True):
        ia, ic = self.col[a], self.col[c]; A, C = self.tot[ia], self.tot[ic]
        th = 1 - A / C; bt = 1 - self.T[:, ia] / self.T[:, ic]
        ma, mc = self.M[:, ia], self.M[:, ic]; jk = 1 - (A - ma) / (C - mc); jm = jk.mean()
        num = ((jm - jk) ** 3).sum(); den = 6 * (((jm - jk) ** 2).sum()) ** 1.5; acc = num / den if den > 0 else 0.0
        prop = (np.sum(bt < th) + 0.5 * np.sum(bt == th)) / len(bt); prop = min(max(prop, 1 / (2 * len(bt))), 1 - 1 / (2 * len(bt))); z0 = stats.norm.ppf(prop)
        def bca(al):
            z = stats.norm.ppf(al); return np.quantile(bt, stats.norm.cdf(z0 + (z0 + z) / (1 - acc * (z0 + z))))
        r = dict(blocks=self.nb, S=th, pct_lo=np.quantile(bt, 0.025), pct_hi=np.quantile(bt, 0.975), LB=bca(0.025), bca_hi=bca(0.975))
        r['half'] = (r['bca_hi'] - r['LB']) / 2
        if wil:
            d = mc - ma; nz = d[d != 0]
            if len(nz) == 0: r['p'], r['rbc'] = 1.0, 0.0
            else:
                r['p'] = float(stats.wilcoxon(nz).pvalue) if len(nz) > 0 else 1.0
                rk = stats.rankdata(np.abs(nz)); wp, wm = rk[nz > 0].sum(), rk[nz < 0].sum(); r['rbc'] = (wp - wm) / (wp + wm)
        return r
POPS = {}
def pop(key, D, mask, arms, shift=0, cls=None):
    """one bootstrap per (population, class, block size), carrying every arm of the corpus (`arms` only checks presence)."""
    k = (key, mask, cls, shift)
    if k not in POPS:
        m = D['masks'][mask] if cls is None else D['masks'][mask] & (D['cls'] == cls) if not cls.startswith('data:') else D['masks'][mask] & (D['typ'] == cls)
        POPS[k] = Pop('%s|%s|%s' % (key, mask, cls), D['unit'], m, D['arms'], shift)
    assert all(a in POPS[k].col for a in arms), arms
    return POPS[k]
# ---------------------------------------------------------------- run
os.makedirs(OUT, exist_ok=True)
FAM = list(csv.DictReader(open(os.path.join(OUT, 'families.csv'))))
TESTS, DESC = [], []
META = {'B': B, 'master_seed': MASTER, 'lambda_pp_per_core': LAMBDA, 'cap': CAP, 'cf': {}, 'cells': {}}
def ptab(D, key, mask, cls, arms, cf, shift=0, extra=()):
    """descriptive rows: every arm against CF (and the extra comparator pairs)."""
    P = pop(key, D, mask, sorted(set(arms) | {cf} | {c for _, c in extra}), shift, cls)
    if P.nb == 0: return
    for a in arms:
        if a == cf: continue
        r = P.test(a, cf, wil=False) if P.nb >= MINCLS or cls in (None, 'all') else dict(blocks=P.nb, S=1 - P.tot[P.col[a]] / P.tot[P.col[cf]])
        DESC.append(dict(key=key, mask=mask, cls=cls or 'all', shift=shift, arm=a, comp=cf, bytes_in=int(D['length'][D['masks'][mask] & ((D['cls'] == cls) if cls and not cls.startswith('data:') else (D['typ'] == cls) if cls else True)].sum()), stored=int(P.tot[P.col[a]]), comp_stored=int(P.tot[P.col[cf]]), **r))
    for a, c in extra:
        DESC.append(dict(key=key, mask=mask, cls=cls or 'all', shift=shift, arm=a, comp=c, stored=int(P.tot[P.col[a]]), comp_stored=int(P.tot[P.col[c]]), **P.test(a, c, wil=False)))
def run_test(f, D, key, cf, shift):
    arm, comp = f['arm'], f['comparator']; size = int(f['size']); view = f['view']; kind = D['kind']; cls = None if f['class'] == 'all' else f['class']
    mask = 'all' if f['pop'] == 'all' else f['pop']
    if comp == 'CF': comp = cf
    cand = None
    if comp == 'BEST':
        shipped = ['ft@1', 'ft@3'] + (['bs-dist@1', 'bs-dist@3'] if kind == 'raw' else ['dbd@1', 'dbd@3'])
        cand = [cf] + shipped; Pc = pop(key, D, mask, sorted(set(cand + [arm, cf])), shift, cls)
        comp = min(cand, key=lambda a: Pc.tot[Pc.col[a]])                                 # fixed on the point estimate, not per resample
        lcand = shipped; lcomp = min(lcand, key=lambda a: Pc.tot[Pc.col[a]])                 # lenient: best shipped set only
    P = pop(key, D, mask, sorted(set([arm, comp, cf] + (cand or []))), shift, cls)
    r = P.test(arm, comp)
    cl = cells(view, kind, size, f['corpus']); lcl = lenient_cells(view, kind, size, f['corpus'])
    rc, rd, dcf, note = capinfo(cl, view, arm, cf)                                         # strict: cap and delta-cores against CF ...
    rc2, rd2, dcc, _ = capinfo(cl, view, arm, comp) if comp != cf else (rc, rd, dcf, '')     # ... and against the comparator arm
    row = dict(f, comp_used=comp, cf=cf, shift=shift, **r)
    if rc is None: row.update(cap_c='', cap_d='', cap_ok='', dcores='', N='', note2=note)
    else:
        capc, capd = min(rc, rc2), min(rd, rd2); dco = max(dcf, dcc)
        row.update(cap_c=capc, cap_d=capd, cap_ok=int(capc >= CAP and capd >= CAP), dcores=dco, N=100 * r['LB'] - LAMBDA * dco, note2=note)
    if comp == f['comparator'] or f['comparator'] == 'CF' or cand is None: pass
    if cand is not None:                                                                    # lenient D2 reading
        rl = P.test(arm, lcomp) if lcomp in P.col else pop(key, D, mask, sorted(set([arm, lcomp])), shift, cls).test(arm, lcomp)
        lrc, lrd, ldc, _ = capinfo(lcl, view, arm, lcomp)
        row.update(len_comp=lcomp, len_S=rl['S'], len_LB=rl['LB'], len_p=rl['p'], len_cap_c=lrc if lrc is not None else '', len_cap_d=lrd if lrd is not None else '',
                   len_cap_ok=int(lrc is not None and lrc >= CAP and lrd >= CAP), len_dcores=ldc if ldc is not None else '', len_N=(100 * rl['LB'] - LAMBDA * ldc) if ldc is not None else '')
    elif lcl != cl and rc is not None:
        lrc, lrd, ldc, _ = capinfo(lcl, view, arm, cf)
        row.update(len_comp=comp, len_S=r['S'], len_LB=r['LB'], len_p=r['p'], len_cap_c=lrc, len_cap_d=lrd, len_cap_ok=int(lrc >= CAP and lrd >= CAP), len_dcores=ldc, len_N=100 * r['LB'] - LAMBDA * ldc)
    TESTS.append(row)
CLS_RAW = ['meta', 'dir', 'journal', 'data', 'unmapped', 'mixed', 'otherpart', 'ptable']
t0 = time.time()
# ---- raw devices
for c in RAW:
    for cs in (16384, 65536):
        D = load_raw(c, cs); key = '%s-%d' % (c, cs); cf = cf_for('lunix', 'raw', cs, c)
        if D['arms']['z3'].sum() > D['arms']['z1'].sum(): cf = 'z1'
        META['cf'][key] = cf; META['cells'][key] = cells('lunix', 'raw', cs, c)
        for shift in (0, 4):
            for f in FAM:
                if f['corpus'] == c and int(f['size']) == cs and f['family'] in ('D1', 'D2', 'D2c', 'D3'): run_test(f, D, key, cf, shift)
            q1 = ['z1', 'z3', 'z1off', 'z3off', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle-zd@1', 'oracle-zd@3', 'oracle@1', 'oracle@3'] + [a for a in ('bs-xfs@1', 'bs-xfs@3') if a in D['arms']]
            ptab(D, key, 'full', None, q1, cf, shift)
            q2 = ['z1', 'z3', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle@1', 'oracle@3', 'pv-s1@1', 'pv-s1@3', 'pvc-s1@1', 'pvc-s1@3']
            ptab(D, key, 'pv-s1', None, q2, cf, shift)
            if shift: continue
            for s in (2, 3): ptab(D, key, 'pv-s%d' % s, None, ['pv-s%d@1' % s, 'pv-s%d@3' % s, 'pvc-s%d@1' % s, 'pvc-s%d@3' % s, 'z1', 'z3'], cf)
            for cl in CLS_RAW:
                if (D['cls'] == cl).any(): ptab(D, key, 'full', cl, ['z3', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle@1', 'oracle@3'] + [a for a in ('bs-xfs@1', 'bs-xfs@3') if a in D['arms']], cf)
            for t in sorted(set(D['typ'][D['typ'] != ''])):
                if (D['typ'] == t).sum() >= 50: ptab(D, key, 'full', t, ['z3', 'ft@1', 'ft@3', 'oracle-zd@3', 'oracle@3'], cf)
            if 'sw-f0.001-d16@1' in D['arms']:
                for f_ in ('0.001', '0.01', '0.1'):
                    m = 'pv-s1' if f_ == '0.01' else 'sw-f' + f_
                    sw = [a for a in D['arms'] if a.startswith('sw-f%s-' % f_)] + (['pv-s1@1', 'pv-s1@3', 'pv-cover@1', 'pv-cover@3'] if f_ == '0.01' else [])
                    ptab(D, key, m, None, sw + ['z1', 'z3'], cf, extra=[('pv-cover@1', 'pv-s1@1'), ('pv-cover@3', 'pv-s1@3')] if f_ == '0.01' else ())
        POPS.clear(); log(key, 'done', '%.0fs' % (time.time() - t0))
# ---- database images, Lunix view
for name in DB + CTL:
    for cs in (16384, 65536):
        D = load_dbl(name, cs); key = '%s-%d' % (name, cs); cf = cf_for('lunix', 'db', cs, name)
        if D['arms']['z3'].sum() > D['arms']['z1'].sum(): cf = 'z1'
        META['cf'][key] = cf; META['cells'][key] = cells('lunix', 'db', cs, name)
        for shift in ((0, 4) if name == 'db-se-askubuntu-pg' else (0,)):
            for f in FAM:
                if f['corpus'] == name and int(f['size']) == cs and f['view'] == 'lunix': run_test(f, D, key, cf, shift)
            if name in CTL:                                                              # controls: same tests, never in a family
                for L in (1, 3):
                    run_test(dict(family='ctl', test_id='ctl|%s|%d|pv@%d' % (name, cs, L), decision='D2', corpus=name, view='lunix', size=str(cs), pop='pv-s1', **{'class': 'all'}, arm='pv-s1@%d' % L, comparator='BEST', votes='0', note='control'), D, key, cf, shift)
                    if cs == 16384: run_test(dict(family='ctl', test_id='ctl|%s|%d|dbd@%d' % (name, cs, L), decision='D4-L', corpus=name, view='lunix', size=str(cs), pop='full', **{'class': 'dbpage'}, arm='dbd@%d' % L, comparator='CF', votes='0', note='control'), D, key, cf, shift)
            arms = ['z1', 'z3', 'z1off', 'z3off', 'ft@1', 'ft@3', 'dbd@1', 'dbd@3', 'dbs@1', 'dbs@3', 'oracle@1', 'oracle@3']
            ptab(D, key, 'full', None, arms, cf, shift)
            ptab(D, key, 'pv-s1', None, arms + ['pv-s1@1', 'pv-s1@3', 'pvc-s1@1', 'pvc-s1@3'], cf, shift)
            if shift: continue
            for cl in ('dbpage', 'wal', 'dbother', 'meta', 'journal'):
                if (D['cls'] == cl).any():
                    ptab(D, key, 'full', cl, arms, cf); ptab(D, key, 'pv-s1', cl, ['pv-s1@1', 'pv-s1@3', 'dbd@1', 'dbd@3', 'ft@3', 'z1', 'z3'], cf)
            for s in (2, 3): ptab(D, key, 'pv-s%d' % s, None, ['pv-s%d@1' % s, 'pv-s%d@3' % s, 'z1', 'z3'], cf)
        POPS.clear()
    log(name, 'lunix done', '%.0fs' % (time.time() - t0))
# ---- database images, Constellation view (unit = file)
for name in DB + CTL:
    for tag in ('4m', '1m', 'pg'):
        D = load_cv(name, tag); key = 'cv-%s-%s' % (name, tag); cf = 'z3'; sz = D['size']; META['cf'][key] = cf; META['cells'][key] = cells('constellation', 'db', sz, name)
        for f in FAM:
            if f['corpus'] == name and f['view'] == 'constellation' and int(f['size']) == sz: run_test(f, D, key, cf, 0)
        if name in CTL and tag in ('1m', '4m'):
            run_test(dict(family='ctl', test_id='ctl|%s|%d|dbd@3' % (name, sz), decision='D6', corpus=name, view='constellation', size=str(sz), pop='all', **{'class': 'all'}, arm='dbd@3', comparator='CF', votes='0', note='control'), D, key, cf, 0)
        ptab(D, key, 'all', None, ['dbd@3', 'dbs@3'], cf)
        ptab(D, key, 'pv-s1', None, ['pv-s1@3', 'dbd@3', 'dbs@3'], cf)
        for cl in ('dbpage', 'wal', 'dbother'):
            if (D['cls'] == cl).any(): ptab(D, key, 'all', cl, ['dbd@3', 'dbs@3'], cf)
        POPS.clear()
    log(name, 'constellation done', '%.0fs' % (time.time() - t0))
# ---------------------------------------------------------------- Holm within each family (per block size), pass flags
for shift in (0, 4):
    for fam in ('D1', 'D2', 'D2c', 'D3', 'D4', 'D6'):
        rows = [r for r in TESTS if r['family'] == fam and r['shift'] == shift]
        m = sum(1 for f in FAM if f['family'] == fam)                                  # m = the whole family, also for the 16 MiB subset
        o = sorted(rows, key=lambda r: r['p']); run = 0.0
        for i, r in enumerate(o): run = max(run, min(1.0, (m - i) * r['p'])); r['holm'] = run
        o = sorted(rows, key=lambda r: r.get('len_p', r['p'])); run = 0.0               # lenient reading: its own p where it has one
        for i, r in enumerate(o): run = max(run, min(1.0, (m - i) * r.get('len_p', r['p']))); r['len_holm'] = run
for r in TESTS:
    if r['family'] == 'ctl': r['holm'] = r['p']; r['len_holm'] = r.get('len_p', r['p'])
THR = {'D1': 3.0, 'D2': 2.0, 'D2c': 1.0, 'D3': 2.0, 'D4-L': 5.0, 'D4-C': 5.0, 'D6': 3.0}
for r in TESTS:
    thr = THR[r['decision']]; r['threshold'] = thr; r['underpowered'] = int(r['half'] * 100 > HALFMAX)
    r['pass'] = int(r['N'] != '' and r['N'] >= thr and r['holm'] < 0.05 and r['cap_ok'] == 1 and not r['underpowered'])
    if 'len_N' in r and r['len_N'] != '':
        r['len_pass'] = int(r['len_N'] >= thr and r['len_holm'] < 0.05 and r['len_cap_ok'] == 1 and not r['underpowered'])
    else: r['len_pass'] = int(r['N'] != '' and r['N'] >= thr and r['len_holm'] < 0.05 and r['cap_ok'] == 1 and not r['underpowered'])
# ---------------------------------------------------------------- write
def wcsv(path, rows):
    keys = []
    for r in rows:
        for k in r:
            if k not in keys: keys.append(k)
    with open(path, 'w', newline='') as fh:
        w = csv.DictWriter(fh, fieldnames=keys); w.writeheader()
        for r in rows: w.writerow({k: ('%.6g' % v if isinstance(v, (float, np.floating)) else v) for k, v in r.items()})
wcsv(os.path.join(OUT, 'tests.csv'), TESTS); wcsv(os.path.join(OUT, 'desc.csv'), DESC)
json.dump(META, open(os.path.join(OUT, 'meta.json'), 'w'), indent=1)
missing = {f['test_id'] for f in FAM if f['family'] != 'D5'} - {r['test_id'] for r in TESTS if r['shift'] == 0}
log('tests', len(TESTS), 'desc', len(DESC), 'family tests without a result (D5 has no measurement):', sorted(missing))
