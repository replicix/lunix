#!/usr/bin/env python3
"""zdicts arms (`ft`, `oracle`) for the raw-device corpora.  Run from ~/dictval under `taskset -c 14-15 python3 -I code/zd.py ...`.
zdicts is release v0.0.1 (commit 3b8645ef) built into ~/dictval/zdtool (helper `zdtool`, see zdtool/src/main.rs).
  zd.py sel <name> <cs>      ft selection: zdtool sel (select_by_content on the first 4 KiB of every chunk) -> work/<name>-<cs>.ftsel.u8
  zd.py measure <name> <cs>  zmeasure2 with all 33 shipped dictionaries at zstd-1 and zstd-3, behind the probe (-P, no plain pass) -> work/res/<name>-<cs>.zd.bin
  zd.py agg <name> <cs>      ft@L, oracle-zd@L (best of the 33, or none), oracle@L (also the block set bs-dist, or none) -> out/zdarms-<name>-<cs>.csv,
                             out/ftsel-<name>-<cs>.json (selection counts), work/res/zdstored-<name>-<cs>.npz (per-chunk stored bytes, stays on the host)
The DB set does not exist yet: `oracle` here excludes it (deviation, recorded in SUMMARY.md)."""
import sys, os, json, csv, subprocess
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, corpus, run
W, RES, ZD = lib.WORK, os.path.join(lib.WORK, 'res'), os.path.join(lib.WORK, 'zd')
TOOL = os.path.join(lib.ROOT, 'zdtool', 'target', 'release', 'zdtool')
def manifest():
    return [l.rstrip('\n').split('\t') for l in open(os.path.join(ZD, 'manifest.tsv'))]
def sel(name, cs):
    base = os.path.join(W, '%s-%d' % (name, cs)); img = open(base + '.img').read().strip()
    subprocess.run(['taskset', '-c', os.environ.get('DICTVAL_CPUS', '14-15'), TOOL, 'sel', base + '.recs', img, base + '.ftsel.u8'], check=True)
def measure(name, cs):
    d = ['d%02d=%s' % (int(i), os.path.join(ZD, '%s.dict' % i)) for i, _, _, _ in manifest()]
    corpus.measure(name, cs, 'zd', '1,3', d, flags=['-P', '-n'])
def agg(name, cs):
    base = os.path.join(W, '%s-%d' % (name, cs)); rec = np.fromfile(base + '.recs', dtype=run.REC); n = len(rec); ln = rec['len'].astype(np.int64)
    lab = np.fromfile(base + '.cls', dtype=np.uint8); labn = np.array([run.LAB[int(x)] for x in lab])
    pl = run.cols('plain', name, cs); probe = pl['probe'].astype(np.int64); praw = probe * 100 >= 94 * np.minimum(ln, 4096)
    zd = run.cols('zd', name, cs); fsel = np.fromfile(base + '.ftsel.u8', dtype=np.uint8); assert len(fsel) == n
    st = np.load(os.path.join(RES, 'stored-%s-%d.npz' % (name, cs))); sarms = list(st['arms']); nd = len(manifest())
    def stored(v): return np.where(praw, ln, np.minimum(ln, v.astype(np.int64)))
    A = {}
    for L in (1, 3):
        z = pl['z%d' % L].astype(np.int64); M = np.full((nd, n), 2**40, dtype=np.int64)
        for k in range(1, nd + 1):
            c = zd['d%02d@%d' % (k, L)].astype(np.int64); ok = c != 0xFFFFFFFF; M[k - 1][ok] = c[ok]
        assert (M[:, ~praw] < 2**40).all(), 'unevaluated chunk'
        ft = z.copy(); has = (fsel > 0) & ~praw; ft[has] = M[fsel[has] - 1, np.nonzero(has)[0]]
        A['ft@%d' % L] = stored(ft)
        ozd = np.minimum(z, M.min(axis=0)); A['oracle-zd@%d' % L] = stored(ozd)
        A['oracle@%d' % L] = np.minimum(A['oracle-zd@%d' % L], st['stored'][sarms.index('bs-dist@%d' % L)].astype(np.int64))
    rows = []; names = sorted(set(labn))
    for a, v in A.items():
        for g, gm in [('all', np.ones(n, bool))] + [(c, labn == c) for c in names]:
            if gm.any(): rows.append((name, cs, 'full', a, g, int(gm.sum()), int(ln[gm].sum()), int(v[gm].sum())))
    w = csv.writer(open(os.path.join(lib.OUT, 'zdarms-%s-%d.csv' % (name, cs)), 'w')); w.writerow(['corpus', 'chunk', 'mask', 'arm', 'class', 'chunks', 'bytes_in', 'stored']); w.writerows(rows)
    mf = manifest(); cnt = {'none': int((fsel == 0).sum())}; byt = {'none': int(ln[fsel == 0].sum())}
    for i, _, nm, _ in mf: cnt[nm] = int((fsel == int(i)).sum()); byt[nm] = int(ln[fsel == int(i)].sum())
    json.dump({'chunks': cnt, 'bytes': byt}, open(os.path.join(lib.OUT, 'ftsel-%s-%d.json' % (name, cs)), 'w'))
    keys = list(A); np.savez_compressed(os.path.join(RES, 'zdstored-%s-%d.npz' % (name, cs)), arms=np.array(keys), stored=np.stack([A[k] for k in keys]).astype(np.uint32), ft_sel=fsel)
    print(name, cs, 'zd arms', len(A), 'rows', len(rows))
if __name__ == '__main__':
    a = sys.argv[1:]; {'sel': sel, 'measure': measure, 'agg': agg}[a[0]](a[1], int(a[2]))
