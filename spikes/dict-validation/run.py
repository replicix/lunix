#!/usr/bin/env python3
"""Arm driver for the raw-device corpora (queue A).  Run from ~/dictval under `taskset -c 14-15 python3 -I code/run.py ...`.
  run.py trainbs <name>        train bs-dist (and bs-xfs where the protocol runs it) for the corpus' family / file system, hold-out asserted
  run.py arms <name> <cs>      bs-dist, bs-xfs, pv (3 seeds) and pv-class (3 seeds) measured behind the probe; trains pv dictionaries if missing
  run.py sweep <name> <cs>     pv sample x dictionary-size sweep and pv-cover (host-ext4 at 16 KiB only)
  run.py agg <name> <cs>       aggregate stored bytes per arm / class / mask -> out/arms-<name>-<cs>.csv, out/shares-<name>-<cs>.json,
                               res/stored-<name>-<cs>.npz (per-chunk stored bytes; stays on the host, input for the bootstrap)
Stored bytes (section 1.1): raw if the ADR-12 probe (4 KiB LZ4) saves under 6 %, else min(zstd, chunk length).  `z1off`, `z3off` are the
plain levels without the probe.  Dictionary arms are only evaluated on chunks that pass the probe (the others are raw by definition)."""
import sys, os, json, glob, csv, subprocess
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, corpus
W, DI, RES = lib.WORK, os.path.join(lib.WORK, 'dicts'), os.path.join(lib.WORK, 'res')
CORP = {'host-ext4': ('deb', 'ext4'), 'host-xfs': ('deb', 'xfs'), 'host-btrfs': ('deb', 'btrfs'),
        'vm-ubuntu': ('deb', 'ext4'), 'vm-debian': ('deb', 'ext4'), 'vm-fedora': ('rhel', 'btrfs')}
XFS_SET = ('host-ext4', 'host-xfs', 'host-btrfs', 'vm-fedora')            # bs-xfs runs on host-* and vm-fedora (section 3)
CN = {1: 'meta', 2: 'text', 3: 'bin'}; MAXV = 0xFFFFFFFF
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])
LAB = {0: 'unmapped', 5: 'unmapped', 1: 'data', 2: 'dir', 3: 'meta', 4: 'journal', 6: 'dbpage', 7: 'wal', 8: 'dbother', 9: 'otherpart', 10: 'ptable', 255: 'mixed'}
SWEEP = [(f, d) for f in (0.001, 0.01, 0.1) for d in (16, 32, 64, 110) if (f, d) != (0.01, 110)]
def tags(name):
    fam, fs = CORP[name]; t = [('bsd', 'dist-%s' % fam, 'dist', fam, None)]
    if name in XFS_SET: t.append(('bsx', 'xfs-%s-%s' % (fam, fs), 'xfs', fam, fs))
    return t
def check_holdout(name, tag, kind, fam, fs):
    """Hold-out rules enforced by assertion (section 4.3)."""
    m = json.load(open(os.path.join(DI, 'bs-%s.json' % tag))); cfam, cfs = CORP[name]
    assert m['test_family'] == cfam == fam, 'training manifest is for another test family'
    pool = {os.path.basename(j)[:-5]: json.load(open(j)) for j in glob.glob(os.path.join(W, 'pool', '*.json'))}
    seen = set()
    for v in m['views']:
        pv = pool[v]; seen.add(pv['family'])
        assert pv['family'] != cfam, 'bs trained on the test distribution family: %s' % v
        if kind == 'xfs': assert pv['fs'] != cfs, 'bs-xfs trained on the test file system: %s' % v
        assert not v.startswith(name), 'training view is the test image'
    assert m['views'], 'no training views'
    return m
def trainbs(name):
    for pre, tag, kind, fam, fs in tags(name):
        if os.path.exists(os.path.join(DI, 'bs-%s.json' % tag)): continue
        subprocess.run(['python3', '-I', os.path.join(lib.ROOT, 'code', 'train.py'), 'bs', kind, fam] + ([fs] if fs else []), check=True)
    for pre, tag, kind, fam, fs in tags(name): check_holdout(name, tag, kind, fam, fs)
def ensure_pv(name, cs, seed, frac=None, dsz=None, cover=False):
    t = os.path.join(lib.ROOT, 'code', 'train.py')
    if cover: subprocess.run(['python3', '-I', t, 'pvcover', name, str(cs), str(seed)], check=True); return
    subprocess.run(['python3', '-I', t, 'pv', name, str(cs), str(seed)] + ([str(frac), str(dsz)] if frac else []), check=True)
def pvfile(name, cs, seed, part, frac=0.01, dsz=110):
    base = 'pv-%s-%d-s%d' % (name, cs, seed)
    if (frac, dsz) != (0.01, 110): base += '-%g-%d' % (frac, dsz)
    return os.path.join(DI, '%s-%s.dict' % (base, part))
def arms(name, cs):
    d = []
    for pre, tag, kind, fam, fs in tags(name):
        check_holdout(name, tag, kind, fam, fs)
        for c in (1, 2, 3): d.append('%s_%s=%s:%d' % (pre, CN[c], os.path.join(DI, 'bs-%s-%s.dict' % (tag, CN[c])), c))
    for s in (1, 2, 3):
        if not os.path.exists(pvfile(name, cs, s, 'all')): ensure_pv(name, cs, s)
        d.append('pv%d_all=%s' % (s, pvfile(name, cs, s, 'all')))
        for c in (1, 2, 3):
            p = pvfile(name, cs, s, CN[c])
            if os.path.exists(p): d.append('pvc%d_%s=%s:%d' % (s, CN[c], p, c))
    corpus.measure(name, cs, 'arms', '1,3', d, flags=['-P', '-n'])
def sweep(name, cs):
    assert (name, cs) == ('host-ext4', 16384), 'the protocol runs the sweeps on host-ext4 at 16 KiB (and on a database image, not mine)'
    d = []
    for f, ds in SWEEP:
        if not os.path.exists(pvfile(name, cs, 1, 'all', f, ds)): ensure_pv(name, cs, 1, f, ds)
        d.append('sw_f%g_d%d=%s' % (f, ds, pvfile(name, cs, 1, 'all', f, ds)))
    p = os.path.join(DI, 'pv-%s-%d-s1-cover.dict' % (name, cs))
    if not os.path.exists(p): ensure_pv(name, cs, 1, cover=True)
    d.append('swcover=' + p)
    corpus.measure(name, cs, 'sweep', '1,3', d, flags=['-P', '-n'])
def cols(tag, name, cs):
    b = os.path.join(RES, '%s-%d.%s.bin' % (name, cs, tag)); names = open(b + '.cols').read().split()
    return {n: v for n, v in zip(names, np.fromfile(b, dtype=np.uint32).reshape(-1, len(names)).T)}
def agg(name, cs):
    base = os.path.join(W, '%s-%d' % (name, cs)); rec = np.fromfile(base + '.recs', dtype=REC); n = len(rec); ln = rec['len'].astype(np.int64)
    lab = np.fromfile(base + '.cls', dtype=np.uint8); labn = np.array([LAB[int(x)] for x in lab]); cheap = None
    pl = cols('plain', name, cs); cheap = pl['class']; probe = pl['probe'].astype(np.int64)
    psz = np.minimum(ln, 4096); praw = probe * 100 >= 94 * psz
    A = {}                                                    # arm -> (stored array, mask key)
    z = {1: pl['z1'].astype(np.int64), 3: pl['z3'].astype(np.int64)}
    A['none'] = (ln, 'full'); A['z1off'] = (np.minimum(ln, z[1]), 'full'); A['z3off'] = (np.minimum(ln, z[3]), 'full')
    for L in (1, 3): A['z%d' % L] = (np.where(praw, ln, np.minimum(ln, z[L])), 'full')
    A['probe'] = A['z1']
    def stored(zv): return np.where(praw, ln, np.minimum(ln, zv.astype(np.int64)))
    def classwise(tab, pre, L, parts):
        zv = z[L].copy()
        for c in parts:
            col = tab['%s_%s@%d' % (pre, CN[c], L)].astype(np.int64); sel = cheap == c
            assert (col[sel & ~praw] != MAXV).all(), 'unevaluated chunk in %s' % pre
            zv = np.where(sel & ~praw, col, zv)
        return stored(zv)
    masks = {'full': np.ones(n, bool)}; maskidx = {}
    tab = cols('arms', name, cs) if os.path.exists(os.path.join(RES, '%s-%d.arms.bin' % (name, cs))) else None
    if tab:
        for pre, tag, kind, fam, fs in tags(name):
            check_holdout(name, tag, kind, fam, fs)
            for L in (1, 3): A['bs-%s@%d' % ('dist' if pre == 'bsd' else 'xfs', L)] = (classwise(tab, pre, L, (1, 2, 3)), 'full')
        for s in (1, 2, 3):
            idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s%d-0.01.npy' % (name, cs, s))); mk = 'smp-s%d-f0.01' % s
            m = np.ones(n, bool); m[idx] = False; assert m.sum() == n - len(idx) and not m[idx].any(); masks[mk] = m; maskidx[mk] = idx
            for L in (1, 3):
                col = tab['pv%d_all@%d' % (s, L)].astype(np.int64); assert (col[~praw] != MAXV).all()
                A['pv-s%d@%d' % (s, L)] = (stored(np.where(praw, z[L], col)), mk)
                have = [c for c in (1, 2, 3) if 'pvc%d_%s@%d' % (s, CN[c], L) in tab]
                A['pvc-s%d@%d' % (s, L)] = (classwise(tab, 'pvc%d' % s, L, have), mk)
    sw = cols('sweep', name, cs) if os.path.exists(os.path.join(RES, '%s-%d.sweep.bin' % (name, cs))) else None
    if sw:
        for f, ds in SWEEP + [(0.01, 110)]:
            if (f, ds) == (0.01, 110): continue
            mk = 'smp-s1-f%g' % f
            if mk not in masks:
                idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s1-%g.npy' % (name, cs, f))); m = np.ones(n, bool); m[idx] = False; masks[mk] = m; maskidx[mk] = idx
            for L in (1, 3):
                col = sw['sw_f%g_d%d@%d' % (f, ds, L)].astype(np.int64); assert (col[~praw] != MAXV).all()
                A['sw-f%g-d%d@%d' % (f, ds, L)] = (stored(np.where(praw, z[L], col)), mk)
        for L in (1, 3):
            col = sw['swcover@%d' % L].astype(np.int64); assert (col[~praw] != MAXV).all(); A['pv-cover@%d' % L] = (stored(np.where(praw, z[L], col)), 'smp-s1-f0.01')
    refs = ['none', 'z1off', 'z3off', 'probe', 'z1', 'z3'] + [a for a in A if a.startswith('bs-')]
    names = sorted(set(labn))
    out = []
    mask_arms = {mk: [a for a, (v, k) in A.items() if k == mk and a not in refs] for mk in masks}
    for mk, m in masks.items():
        arms_here = refs + (mask_arms[mk] if mk != 'full' else [])
        for a in arms_here:
            v = A[a][0]
            groups = [('all', m)] + ([(c, m & (labn == c)) for c in names] if mk in ('full', 'smp-s1-f0.01') else [])
            for g, gm in groups:
                if gm.any(): out.append((name, cs, mk, a, g, int(gm.sum()), int(ln[gm].sum()), int(v[gm].sum())))
    for c in range(0, 5):                                        # cheap-class population (arm none)
        gm = cheap == c
        if gm.any(): out.append((name, cs, 'full', 'none', 'cheap-%s' % ({0: 'empty', 1: 'meta', 2: 'text', 3: 'bin', 4: 'unclassified'}[c]), int(gm.sum()), int(ln[gm].sum()), int(ln[gm].sum())))
    w = csv.writer(open(os.path.join(lib.OUT, 'arms-%s-%d.csv' % (name, cs)), 'w')); w.writerow(['corpus', 'chunk', 'mask', 'arm', 'class', 'chunks', 'bytes_in', 'stored']); w.writerows(out)
    sh = json.load(open(base + '.shares.json')); sh.update(probe_raw_chunks=int(praw.sum()), probe_raw_bytes=int(ln[praw].sum()))
    h = np.fromfile(base + '.h16', dtype='S16'); sh['cross_dup'] = {}
    for pre, tag, kind, fam, fs in tags(name):
        if not os.path.exists(os.path.join(DI, 'bs-%s.h16' % tag)): continue
        t = np.fromfile(os.path.join(DI, 'bs-%s.h16' % tag), dtype='S16'); hit = np.isin(h, t)
        sh['cross_dup'][tag] = {'train_sample_hits': int(hit.sum()), 'train_sample_hit_bytes': int(ln[hit].sum())}
    # share of the test chunks that also occur anywhere in the other-family pool images (not only in the sample)
    fams = {}
    for j in glob.glob(os.path.join(W, 'pool', '*.json')):
        v = json.load(open(j)); fams[os.path.basename(j)[:-5]] = v['family']
    other = [f for f in glob.glob(os.path.join(W, 'pool', '*.h16')) if fams.get(os.path.basename(f)[:-4]) not in (None, CORP[name][0])]
    if other:
        ph = np.unique(np.concatenate([np.fromfile(f, dtype='S16') for f in other])); hit = np.isin(h, ph)
        sh['cross_dup']['pool_other_families'] = {'chunks': int(hit.sum()), 'bytes': int(ln[hit].sum())}
    sh['icheck'] = json.load(open(os.path.join(lib.OUT, 'labels-%s.json' % name))).get('icheck')
    json.dump(sh, open(os.path.join(lib.OUT, 'shares-%s-%d.json' % (name, cs)), 'w'))
    keys = list(A); np.savez_compressed(os.path.join(RES, 'stored-%s-%d.npz' % (name, cs)), arms=np.array(keys), stored=np.stack([A[k][0] for k in keys]).astype(np.uint32),
        masks=np.array([A[k][1] for k in keys]), block=(rec['off'] >> 20).astype(np.uint32), length=rec['len'], label=lab, cheap=cheap.astype(np.uint8),
        **{'sample_' + k: v for k, v in maskidx.items()})
    print(name, cs, 'arms', len(A), 'rows', len(out))
if __name__ == '__main__':
    a = sys.argv[1:]
    if a[0] == 'trainbs': trainbs(a[1])
    elif a[0] == 'arms': arms(a[1], int(a[2]))
    elif a[0] == 'sweep': sweep(a[1], int(a[2]))
    elif a[0] == 'agg': agg(a[1], int(a[2]))
