#!/usr/bin/env python3
"""Analysis of the validation study under Amendment A1 (PREREGISTERED.md sections 1, 4 and A1).  Test host only; per-chunk and
per-object inputs never leave it.  The first analysis is analyze_v1.py.
usage (from ~/dictval):  nice -n 19 ionice -c3 taskset -c 14-15 python3 -I ana/analyze.py ana/a1 [B]
inputs   work/res/stored-*.npz, zdstored-*.npz (raw devices); work/res/<db>-<cs>.db.bin, .pvcover.bin, zd-<db>-<cs>.npz, cv-<db>-<tag>.db.bin
         (databases); work/res/d5-<corpus>.npz (Constellation file objects); ana/timing.csv (= raw/timing.csv), ana/timing-db.csv
         (= raw/timing-db.csv, one volume-wide cell per database image and size); <out>/families.csv (= raw/families-a1.csv)
outputs  <out>/tests.csv, desc.csv, meta.json (aggregates only)
Definitions: S = 1 - stored(A)/stored(B) over the population (bytes-weighted).  Resampling and pairing unit: the 1 MiB origin block (Lunix
views; `shift` 4 = 16 MiB blocks, printed next to it, A1.6) or the file (Constellation views; `shift` 4 = 16 MiB extent of a file).
B = 10,000 paired block-bootstrap resamples seeded from 20261112; LB = BCa lower end; N = LB - 0.1 pp x delta-cores; cap: compress and
decompress throughput of A >= 0.8 x the counterfactual (A1.1).  Counterfactual (section 1.1): the best plain level behind the probe that
passes the cap against zstd-1; Lunix raw devices: zstd-1 or zstd-3 (protocol text); database images: zstd-1, -2 or -3 from the image's own
cell (A1.2), ranked by the cell's stored bytes.  zstd-2 has no per-chunk measurement: where it is the counterfactual the test runs against
zstd-1 and S, LB and N are rescaled by the cell's zstd-2/zstd-1 byte ratio (columns *_cf); cap and cores are against zstd-2 exactly."""
import sys, os, csv, json, hashlib, time
import numpy as np
from scipy import stats
ROOT = os.path.expanduser('~/dictval'); WORK = os.path.join(ROOT, 'work'); RES = os.path.join(WORK, 'res'); ANA = os.path.join(ROOT, 'ana')
OUT = sys.argv[1]; B = int(sys.argv[2]) if len(sys.argv) > 2 else 10000
MASTER = 20261112; LAMBDA = 0.1; CAP = 0.8; HALFMAX = 1.5; MINCLS = 200
RAW = ['host-ext4', 'host-xfs', 'host-btrfs', 'vm-ubuntu', 'vm-debian', 'vm-fedora']
DSETS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; ENG = ['pg', 'my', 'sq']
DB = ['db-%s-%s' % (d, e) for d in DSETS for e in ENG]; CTL = ['db-ctl-pgbench-pg', 'db-ctl-sysbench-my']
D5C = ['vm-ubuntu', 'vm-debian', 'vm-fedora', 'host-files']
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]); MAXV = 0xFFFFFFFF
LAB = {0: 'unmapped', 5: 'unmapped', 1: 'data', 2: 'dir', 3: 'meta', 4: 'journal', 6: 'dbpage', 7: 'wal', 8: 'dbother', 9: 'otherpart', 10: 'ptable', 255: 'mixed'}
LABV = np.array([LAB.get(i, 'unmapped') for i in range(256)])
NMV = np.array([{6: 'dbpage', 7: 'wal', 8: 'dbother'}.get(i, 'other') for i in range(256)])
def seed(*p): return int.from_bytes(hashlib.sha256((str(MASTER) + '|' + '|'.join(map(str, p))).encode()).digest()[:8], 'little')
def log(*a): print(time.strftime('%H:%M:%S'), *a, flush=True)
# ---------------------------------------------------------------- timing, counterfactual, cap
TM, TMS = {}, {}
for fn, pre in (('timing.csv', ''), ('timing-db.csv', 'vol:')):
    for r in csv.DictReader(open(os.path.join(ANA, fn))):
        TM[(pre + r['cell'], r['arm'], int(r['level']), r['op'])] = float(r['median'])
        if r['stored_bytes']: TMS[(pre + r['cell'], r['arm'], int(r['level']))] = float(r['stored_bytes'])
def target(view): return 5.0 if view.startswith('lunix') else 1.0                     # ingest targets of section 1.1
def tarm(arm):
    a, L = (arm.split('@') + ['0'])[:2]; L = int(L)
    if a in ('z1', 'z2', 'z3', 'z4', 'z1off', 'z3off'): return a, int(a[1])
    a = {'pv-s1': 'pv', 'pv-s2': 'pv', 'pv-s3': 'pv', 'pvc-s1': 'pv-class', 'pvcover-s1': 'pv-cover', 'pv-cover': 'pv-cover', 'dbd': 'db', 'dbs': 'db'}.get(a, a)
    return a, L
def cells(view, kind, cs, corpus, pop=None):
    """timing cells for a population.  Database images: the image's own volume-wide cell (A1.2)."""
    if view == 'constellation-files': return ['vm-ubuntu-files-lt64k' if pop == 'lt64k' else 'vm-ubuntu-files']
    if kind == 'raw': return ['vm-ubuntu-%d' % cs]                                   # T2: vm-ubuntu stands in for every raw-device corpus
    if view == 'constellation':
        tag = {1048576: '1m', 4194304: '4m'}.get(cs)
        if tag is None: return []
        if corpus.startswith('db-se-'): return ['db-se-askubuntu-pg-' + tag]
        if corpus.startswith('db-tlc'): return ['db-tlc-2024q1-pg-' + tag]
        return ['db-se-askubuntu-pg-' + tag, 'db-tlc-2024q1-pg-' + tag]
    if corpus in DB: return ['vol:%s-%d' % (corpus, cs)]
    if cs == 65536: return ['vm-ubuntu-65536']                                       # controls only
    return ['db-se-askubuntu-pg-16384', 'db-tlc-2024q1-pg-16384']
def cost(cell, arm, op):
    a, L = tarm(arm)
    for cand in ((a, L), ('pv', L) if a in ('db', 'pv-class', 'pv-cover') else None):  # single-dictionary stand-in where a cell lacks the arm
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
        rc.append(cb / ca); rd.append(db_ / da); dc.append(max(0.0, ca - cb) * target(view))
    if not rc: return None, None, None, ';'.join(sorted(notes))
    return min(rc), min(rd), max(dc), ';'.join(sorted(notes))
def cf_for(view, kind, cs, corpus):
    """(protocol counterfactual, measured arm the tests run against, zstd-2/zstd-1 byte ratio of the cell or None)."""
    if view != 'lunix': return 'z3', 'z3', None
    cl = cells(view, kind, cs, corpus)
    levels = ('z1', 'z2', 'z3') if corpus in DB else ('z1', 'z3')                 # protocol: Lunix zstd-1 or zstd-3; A1.2: zstd-2 for DB images
    ok = []
    for z in levels:
        if z == 'z1': ok.append(z); continue
        rc, rd, _, _ = capinfo(cl, view, z, 'z1')
        if rc is not None and rc >= CAP and rd >= CAP: ok.append(z)
    if corpus in DB:
        cell = cl[0]; best = min(ok, key=lambda z: TMS[(cell, z, int(z[1]))])
        return best, ('z1' if best == 'z2' else best), (TMS[(cell, 'z2', 2)] / TMS[(cell, 'z1', 1)] if best == 'z2' else None)
    best = 'z3' if 'z3' in ok else 'z1'
    return best, best, None
# ---------------------------------------------------------------- loaders
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
    u = st['block'].astype(np.int64)
    return dict(arms=A, unit=u, unit16=u >> 4, length=st['length'].astype(np.int64), cls=lab, typ=typ, masks=M, kind='raw', view='lunix')
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
    pc = os.path.join(RES, '%s-%d.pvcover.bin' % (name, cs))
    if os.path.exists(pc):                                                         # A1.3: pv-cover (seed-1 sample, COVER steps=16, 110 KiB)
        P = colsof(pc); assert len(P['probe']) == len(L) and (P['probe'] == C['probe']).all() and (P['class'] == C['class']).all()   # plain columns there are unmeasured (MAXV)
        for lv in (1, 3):
            assert (P['pvcover1@%d' % lv][~praw] != MAXV).all(); A['pvcover-s1@%d' % lv] = beh(P['pvcover1@%d' % lv])
    n = len(L); M = {'full': np.ones(n, bool)}
    for s in (1, 2, 3):
        m = np.ones(n, bool); m[np.load(os.path.join(RES, 'pvsample-%s-%d-s%d-0.01.npy' % (name, cs, s)))] = False; M['pv-s%d' % s] = m
    lab = LABV[np.fromfile(b + '.cls', dtype=np.uint8)]; u = (rec['off'] >> 20).astype(np.int64)
    return dict(arms=A, unit=u, unit16=u >> 4, length=L, cls=lab, typ=None, masks=M, kind='db', view='lunix')
def load_cv(name, tag):
    b = os.path.join(WORK, 'cv-%s-%s' % (name, tag)); rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64)
    C = colsof(os.path.join(RES, 'cv-%s-%s.db.bin' % (name, tag))); pc = NMV[np.fromfile(b + '.pcls', dtype=np.uint8)]
    z3 = C['z3@3'] if 'z3@3' in C else C['z3']
    A = {'none': L, 'z3': np.minimum(z3, L), 'dbd@3': np.minimum(C['dbd@3'], L), 'dbs@3': np.minimum(C['dbs@3'], L)}
    for s in (1, 2, 3): A['pv-s%d@3' % s] = np.minimum(C['pv%d@3' % s], L)
    n = len(L); M = {'full': np.ones(n, bool), 'all': np.ones(n, bool)}
    for s in (1, 2, 3):
        m = np.ones(n, bool); m[np.load(os.path.join(RES, 'cvpvsample-%s-%s-s%d.npy' % (name, tag, s)))] = False; M['pv-s%d' % s] = m
    sz = json.load(open(b + '.shares.json'))['object_size']; fid = rec['id'].astype(np.int64)
    return dict(arms=A, unit=fid, unit16=(fid << 24) | (rec['off'].astype(np.int64) >> 24), length=L, cls=pc, typ=None, masks=M, kind='db', view='constellation', size=sz)
def load_d5(c):
    z = np.load(os.path.join(RES, 'd5-%s.npz' % c)); L = z['len'].astype(np.int64); fid = z['fid'].astype(np.int64)
    A = {'none': L, 'z3': np.minimum(z['z3'].astype(np.int64), L), 'ft@3': np.minimum(z['ft'].astype(np.int64), L), 'oracle@3': np.minimum(z['orc'].astype(np.int64), L)}
    order = np.lexsort((np.arange(len(L)), fid)); off = np.zeros(len(L), np.int64)    # object offset inside its file (objects in record order)
    cs_ = np.cumsum(L[order]); first = np.r_[True, fid[order][1:] != fid[order][:-1]]
    start = np.maximum.accumulate(np.where(first, cs_ - L[order], 0)); off[order] = cs_ - L[order] - start
    M = {'all': np.ones(len(L), bool), 'lt64k': L < 65536}
    return dict(arms=A, unit=fid, unit16=(fid << 24) | (off >> 24), length=L, cls=np.array(['all'] * len(L)), typ=None, masks=M, kind='files', view='constellation-files',
                sel=z['sel'])
# ---------------------------------------------------------------- bootstrap engine
class Pop:
    """block sums of every arm over one population, B paired block-bootstrap resamples (same resamples for every arm)."""
    def __init__(self, key, unit, mask, arms):
        u = unit[mask]; uniq, inv = np.unique(u, return_inverse=True); self.nb = len(uniq); self.names = list(arms)
        self.M = np.stack([np.bincount(inv, weights=arms[a][mask].astype(np.float64), minlength=self.nb) for a in self.names], axis=1)
        self.tot = self.M.sum(axis=0); self.col = {a: i for i, a in enumerate(self.names)}
        rng = np.random.default_rng(seed('boot', key)); T = []; bs = max(1, min(1000, 4_000_000 // max(1, self.nb)))
        for s in range(0, B, bs):
            n = min(bs, B - s); idx = rng.integers(0, self.nb, (n, self.nb))
            cnt = np.bincount((np.arange(n)[:, None] * self.nb + idx).ravel(), minlength=n * self.nb).reshape(n, self.nb).astype(np.float64)
            T.append(cnt @ self.M)
        self.T = np.concatenate(T)
    def test(self, a, c, wil=True):
        ia, ic = self.col[a], self.col[c]; A, C = self.tot[ia], self.tot[ic]; th = 1 - A / C
        r = dict(blocks=self.nb, S=th)
        if self.nb < 2:                                                             # one resampling unit: no interval, no test (review item 8)
            r.update(pct_lo='', pct_hi='', LB='', bca_hi='', half='')
            if wil: r.update(p=1.0, rbc='')
            return r
        bt = 1 - self.T[:, ia] / self.T[:, ic]
        ma, mc = self.M[:, ia], self.M[:, ic]; jk = 1 - (A - ma) / (C - mc); jm = jk.mean()
        num = ((jm - jk) ** 3).sum(); den = 6 * (((jm - jk) ** 2).sum()) ** 1.5; acc = num / den if den > 0 else 0.0
        prop = (np.sum(bt < th) + 0.5 * np.sum(bt == th)) / len(bt); prop = min(max(prop, 1 / (2 * len(bt))), 1 - 1 / (2 * len(bt))); z0 = stats.norm.ppf(prop)
        def bca(al):
            z = stats.norm.ppf(al); return np.quantile(bt, stats.norm.cdf(z0 + (z0 + z) / (1 - acc * (z0 + z))))
        r.update(pct_lo=np.quantile(bt, 0.025), pct_hi=np.quantile(bt, 0.975), LB=bca(0.025), bca_hi=bca(0.975))
        r['half'] = (r['bca_hi'] - r['LB']) / 2
        if wil:
            d = mc - ma; nz = d[d != 0]
            if len(nz) == 0: r['p'], r['rbc'] = 1.0, 0.0
            else:
                r['p'] = float(stats.wilcoxon(nz).pvalue)
                rk = stats.rankdata(np.abs(nz)); wp, wm = rk[nz > 0].sum(), rk[nz < 0].sum(); r['rbc'] = (wp - wm) / (wp + wm)
        return r
POPS = {}
def pop(key, D, mask, arms, shift=0, cls=None):
    """one bootstrap per (population, class, unit): shift 0 = 1 MiB block or file, shift 4 = 16 MiB block or 16 MiB file extent."""
    k = (key, mask, cls, shift)
    if k not in POPS:
        m = D['masks'][mask] if cls is None else D['masks'][mask] & (D['cls'] == cls) if not cls.startswith('data:') else D['masks'][mask] & (D['typ'] == cls)
        POPS[k] = Pop('%s|%s|%s|%d' % (key, mask, cls, shift), D['unit16'] if shift else D['unit'], m, D['arms'])
    assert all(a in POPS[k].col for a in arms), arms
    return POPS[k]
# ---------------------------------------------------------------- run
os.makedirs(OUT, exist_ok=True)
FAM = list(csv.DictReader(open(os.path.join(OUT, 'families.csv'))))
TESTS, DESC = [], []
META = {'B': B, 'master_seed': MASTER, 'lambda_pp_per_core': LAMBDA, 'cap': CAP, 'cf': {}, 'cells': {}, 'units': {}}
def nf(x): return x if x != '' else None
def ptab(D, key, mask, cls, arms, cf, shift=0, extra=()):
    P = pop(key, D, mask, sorted(set(arms) | {cf} | {c for _, c in extra}), shift, cls)
    if P.nb == 0: return
    bm = D['masks'][mask] & ((D['cls'] == cls) if cls and not cls.startswith('data:') else (D['typ'] == cls) if cls else True)
    for a in arms:
        if a == cf or a not in P.col: continue
        r = P.test(a, cf, wil=False) if P.nb >= MINCLS or cls in (None, 'all') else dict(blocks=P.nb, S=1 - P.tot[P.col[a]] / P.tot[P.col[cf]])
        DESC.append(dict(key=key, mask=mask, cls=cls or 'all', shift=shift, arm=a, comp=cf, bytes_in=int(D['length'][bm].sum()), stored=int(P.tot[P.col[a]]), comp_stored=int(P.tot[P.col[cf]]), **r))
    for a, c in extra:
        if a in P.col and c in P.col:
            DESC.append(dict(key=key, mask=mask, cls=cls or 'all', shift=shift, arm=a, comp=c, stored=int(P.tot[P.col[a]]), comp_stored=int(P.tot[P.col[c]]), **P.test(a, c, wil=False)))
def run_test(f, D, key, cfi, shift):
    cfp, cfm, r21 = cfi
    arm, comp = f['arm'], f['comparator']; size = int(f['size']); view = f['view']; kind = D['kind']; cls = None if f['class'] == 'all' else f['class']
    mask = f['pop'] if f['pop'] in D['masks'] else 'full'
    if comp == 'CF': comp = cfm
    P = pop(key, D, mask, [arm, comp, cfm], shift, cls)
    r = P.test(arm, comp)
    cl = cells(view, kind, size, f['corpus'], f['pop'])
    rc, rd, dcf, note = capinfo(cl, view, arm, cfp)                            # cap and delta-cores against the counterfactual (A1.1) ...
    if f['comparator'] != 'CF':
        rc2, rd2, dcc, _ = capinfo(cl, view, arm, comp)                             # ... and, for D2c/D3, also against the named comparator
        if rc2 is not None and rc is not None: rc, rd, dcf = min(rc, rc2), min(rd, rd2), max(dcf, dcc)
    row = dict(f, comp_used=comp, cf=cfp, cf_measured=cfm, shift=shift, **r)
    LBv = nf(r['LB']); Sv = r['S']
    if f['comparator'] == 'CF' and r21 is not None:                                 # zstd-2 counterfactual: rescale by the cell's byte ratio
        row['z2_over_z1'] = r21; Sv = 1 - (1 - Sv) / r21; LBv = None if LBv is None else 1 - (1 - LBv) / r21
        row['S_cf'] = Sv; row['LB_cf'] = '' if LBv is None else LBv
    if rc is None or LBv is None: row.update(cap_c=rc if rc is not None else '', cap_d=rd if rd is not None else '', cap_ok='' if rc is None else int(rc >= CAP and rd >= CAP), dcores=dcf if dcf is not None else '', N='', note2=note)
    else: row.update(cap_c=rc, cap_d=rd, cap_ok=int(rc >= CAP and rd >= CAP), dcores=dcf, N=100 * LBv - LAMBDA * dcf, note2=note)
    TESTS.append(row)
CLS_RAW = ['meta', 'dir', 'journal', 'data', 'unmapped', 'mixed', 'otherpart', 'ptable']
t0 = time.time()
# ---- raw devices
for c in RAW:
    for cs in (16384, 65536):
        D = load_raw(c, cs); key = '%s-%d' % (c, cs); cfi = cf_for('lunix', 'raw', cs, c); cf = cfi[1]
        META['cf'][key] = cfi[0]; META['cells'][key] = cells('lunix', 'raw', cs, c)
        for shift in (0, 4):
            for f in FAM:
                if f['corpus'] == c and int(f['size']) == cs and f['family'] in ('D1', 'D2', 'D2c', 'D3'): run_test(f, D, key, cfi, shift)
            q1 = ['z1', 'z3', 'z1off', 'z3off', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle-zd@1', 'oracle-zd@3', 'oracle@1', 'oracle@3'] + [a for a in ('bs-xfs@1', 'bs-xfs@3') if a in D['arms']]
            ptab(D, key, 'full', None, q1, cf, shift)
            q2 = ['z1', 'z3', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle@1', 'oracle@3', 'pv-s1@1', 'pv-s1@3', 'pvc-s1@1', 'pvc-s1@3']
            ptab(D, key, 'pv-s1', None, q2, cf, shift)
            for cl in CLS_RAW:
                if (D['cls'] == cl).any(): ptab(D, key, 'full', cl, ['z3', 'ft@1', 'ft@3', 'bs-dist@1', 'bs-dist@3', 'oracle@1', 'oracle@3'] + [a for a in ('bs-xfs@1', 'bs-xfs@3') if a in D['arms']], cf, shift)
            if shift: continue
            for s in (2, 3): ptab(D, key, 'pv-s%d' % s, None, ['pv-s%d@1' % s, 'pv-s%d@3' % s, 'pvc-s%d@1' % s, 'pvc-s%d@3' % s, 'z1', 'z3'], cf)
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
        D = load_dbl(name, cs); key = '%s-%d' % (name, cs); cfi = cf_for('lunix', 'db', cs, name); cf = cfi[1]
        META['cf'][key] = {'protocol': cfi[0], 'measured': cf, 'z2_over_z1': cfi[2]}; META['cells'][key] = cells('lunix', 'db', cs, name)
        for shift in (0, 4):
            for f in FAM:
                if f['corpus'] == name and int(f['size']) == cs and f['view'] == 'lunix': run_test(f, D, key, cfi, shift)
            if name in CTL:                                                          # controls: same tests, never in a family
                for L in (1, 3):
                    run_test(dict(family='ctl', test_id='ctl|%s|%d|pv@%d' % (name, cs, L), decision='D2', corpus=name, view='lunix', size=str(cs), pop='pv-s1', **{'class': 'all'}, arm='pv-s1@%d' % L, comparator='CF', votes='0', note='control'), D, key, cfi, shift)
                    if cs == 16384: run_test(dict(family='ctl', test_id='ctl|%s|%d|dbd@%d' % (name, cs, L), decision='D4-L', corpus=name, view='lunix', size=str(cs), pop='full', **{'class': 'dbpage'}, arm='dbd@%d' % L, comparator='CF', votes='0', note='control'), D, key, cfi, shift)
            arms = ['z1', 'z3', 'z1off', 'z3off', 'ft@1', 'ft@3', 'dbd@1', 'dbd@3', 'dbs@1', 'dbs@3', 'oracle@1', 'oracle@3']
            cov = [a for a in ('pvcover-s1@1', 'pvcover-s1@3') if a in D['arms']]
            ptab(D, key, 'full', None, arms, cf, shift)
            ptab(D, key, 'pv-s1', None, arms + ['pv-s1@1', 'pv-s1@3', 'pvc-s1@1', 'pvc-s1@3'] + cov, cf, shift, extra=[(a, 'pv-s1@%s' % a[-1]) for a in cov])
            for cl in ('dbpage', 'wal', 'dbother', 'meta', 'journal'):
                if (D['cls'] == cl).any():
                    ptab(D, key, 'full', cl, arms, cf, shift)
                    if not shift: ptab(D, key, 'pv-s1', cl, ['pv-s1@1', 'pv-s1@3', 'dbd@1', 'dbd@3', 'ft@3', 'z1', 'z3'] + cov, cf)
            if shift: continue
            for s in (2, 3): ptab(D, key, 'pv-s%d' % s, None, ['pv-s%d@1' % s, 'pv-s%d@3' % s, 'z1', 'z3'], cf)
        POPS.clear()
    log(name, 'lunix done', '%.0fs' % (time.time() - t0))
# ---- database images, Constellation view (unit = file; shift 4 = 16 MiB extent of a file)
for name in DB + CTL:
    for tag in ('4m', '1m', 'pg'):
        D = load_cv(name, tag); key = 'cv-%s-%s' % (name, tag); cfi = ('z3', 'z3', None); cf = 'z3'; sz = D['size']
        META['cf'][key] = cf; META['cells'][key] = cells('constellation', 'db', sz, name)
        META['units'][key] = {'objects': int(len(D['length'])), 'files': int(len(np.unique(D['unit']))), 'extents16': int(len(np.unique(D['unit16'])))}
        for shift in (0, 4):
            for f in FAM:
                if f['corpus'] == name and f['view'] == 'constellation' and int(f['size']) == sz: run_test(f, D, key, cfi, shift)
            if name in CTL and tag in ('1m', '4m'):
                run_test(dict(family='ctl', test_id='ctl|%s|%d|dbd@3' % (name, sz), decision='D6', corpus=name, view='constellation', size=str(sz), pop='all', **{'class': 'all'}, arm='dbd@3', comparator='CF', votes='0', note='control'), D, key, cfi, shift)
            ptab(D, key, 'all', None, ['dbd@3', 'dbs@3'], cf, shift)
            ptab(D, key, 'pv-s1', None, ['pv-s1@3', 'dbd@3', 'dbs@3'], cf, shift)
            if not shift:
                for cl in ('dbpage', 'wal', 'dbother'):
                    if (D['cls'] == cl).any(): ptab(D, key, 'all', cl, ['dbd@3', 'dbs@3'], cf)
        POPS.clear()
    log(name, 'constellation done', '%.0fs' % (time.time() - t0))
# ---- D5: Constellation file objects of the four file corpora (unit = file; shift 4 = 16 MiB extent)
for c in D5C:
    D = load_d5(c); key = 'd5-%s' % c; cfi = ('z3', 'z3', None)
    META['cf'][key] = 'z3'; META['units'][key] = {m: {'objects': int(D['masks'][m].sum()), 'files': int(len(np.unique(D['unit'][D['masks'][m]]))), 'extents16': int(len(np.unique(D['unit16'][D['masks'][m]])))} for m in D['masks']}
    for shift in (0, 4):
        for f in FAM:
            if f['family'] == 'D5' and f['corpus'] == c: run_test(f, D, key, cfi, shift)
        for m in ('all', 'lt64k'): ptab(D, key, m, None, ['ft@3', 'oracle@3'], 'z3', shift, extra=[('oracle@3', 'ft@3')])
    POPS.clear(); log(key, 'done', '%.0fs' % (time.time() - t0))
# ---------------------------------------------------------------- Holm within each family and unit, pass flags
for shift in (0, 4):
    for fam in ('D1', 'D2', 'D2c', 'D3', 'D4', 'D5', 'D6'):
        rows = [r for r in TESTS if r['family'] == fam and r['shift'] == shift]
        m = sum(1 for f in FAM if f['family'] == fam)
        o = sorted(rows, key=lambda r: r['p']); run = 0.0
        for i, r in enumerate(o): run = max(run, min(1.0, (m - i) * r['p'])); r['holm'] = run
for r in TESTS:
    if r['family'] == 'ctl': r['holm'] = r['p']
THR = {'D1': 3.0, 'D2': 2.0, 'D2c': 1.0, 'D3': 2.0, 'D4-L': 5.0, 'D4-C': 5.0, 'D6': 3.0, 'D5': None}
for r in TESTS:
    thr = THR[r['decision']]
    if r['decision'] == 'D5': thr = 5.0 if r['pop'] == 'lt64k' else 1.0
    r['threshold'] = thr; r['underpowered'] = '' if r['half'] == '' else int(r['half'] * 100 > HALFMAX)
    r['pass'] = int(r['N'] != '' and r['N'] >= thr and r['holm'] < 0.05 and r['cap_ok'] == 1 and r['underpowered'] == 0)
    r['pass_nocap'] = int(r['N'] != '' and r['N'] >= thr and r['holm'] < 0.05 and r['underpowered'] == 0)
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
missing = {f['test_id'] for f in FAM} - {r['test_id'] for r in TESTS if r['shift'] == 0}
log('tests', len(TESTS), 'desc', len(DESC), 'family tests without a result:', sorted(missing))
