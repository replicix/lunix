#!/usr/bin/env python3
"""Database sensitivity runs (sections 2.3 and 3).  usage:
  dbsens.py sweep                 se-askubuntu x PostgreSQL, 16 KiB: pv sample 0.1/1/10 % x dictionary 16/32/64/110 KiB (seed 1) and pv-cover (COVER optimiser); -> out/db_sweep.csv
  dbsens.py fs <xfs|btrfs>        se-askubuntu x PostgreSQL image on another file system (image db-se-askubuntu-pg-<fs> must exist from dbload.py): chunk, label, plain, dbd, pv1; -> out/db_sens_fs_<fs>.csv
CSV columns: corpus,size,pop,class,arm,n,raw_bytes,stored_bytes.  All arms behind the ADR-12 probe; `z1`/`z3` plain behind the probe; pop = chunks outside the arm's sample."""
import sys, os, csv, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, corpus
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]); RES = os.path.join(lib.WORK, 'res'); D = os.path.join(lib.WORK, 'dicts'); H = os.path.dirname(os.path.abspath(__file__))
NM = {6: 'dbpage', 7: 'wal', 8: 'dbother'}
def py(*a): subprocess.run(['python3', '-I'] + [str(x) for x in a], check=True)
def tab(name, cs, tag):
    b = os.path.join(RES, '%s-%d.%s.bin' % (name, cs, tag)); c = open(b + '.cols').read().split(); a = np.fromfile(b, dtype=np.uint32).reshape(-1, len(c)); return {k: a[:, i].astype(np.int64) for i, k in enumerate(c)}
def rows(w, name, cs, pop, mask, L, cls, arms):
    for cl in ('all', 'dbpage', 'wal', 'dbother', 'other'):
        m = mask if cl == 'all' else mask & (np.array([NM.get(int(x), 'other') for x in cls]) == cl)
        for a, v in arms.items(): w.writerow([name, cs, pop, cl, a, int(m.sum()), int(L[m].sum()), int(v[m].sum())])
def common(name, cs):
    b = os.path.join(lib.WORK, '%s-%d' % (name, cs)); rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64); cls = np.fromfile(b + '.cls', dtype=np.uint8); return b, L, cls
def sweep():
    name, cs = 'db-se-askubuntu-pg', 16384; b, L, cls = common(name, cs); pl = tab(name, cs, 'plain'); praw = pl['probe'] >= 0.94 * np.minimum(L, 4096)
    swp = [(f, d) for f in (0.001, 0.01, 0.1) for d in (16, 32, 64, 110)]; dl = []
    for f, d in swp:
        base = 'pv-%s-%d-s1' % (name, cs) + ('' if (f, d) == (0.01, 110) else '-%g-%d' % (f, d)); p = os.path.join(D, base + '-all.dict')
        if not os.path.exists(p): py(os.path.join(H, 'train.py'), 'pv', name, cs, 1, f, d)
        dl.append('sw_f%g_d%d=%s' % (f, d, p))
    pc = os.path.join(D, 'pv-%s-%d-s1-cover.dict' % (name, cs))
    if not os.path.exists(pc): py(os.path.join(H, 'train.py'), 'pvcover', name, cs, 1)
    dl.append('swcover=' + pc); corpus.measure(name, cs, 'sweep', '1,3', dl, flags=['-P', '-n']); t = tab(name, cs, 'sweep')
    w = csv.writer(open(os.path.join(lib.OUT, 'db_sweep.csv'), 'w', newline='')); w.writerow(['corpus', 'size', 'pop', 'class', 'arm', 'n', 'raw_bytes', 'stored_bytes']); MX = 0xFFFFFFFF
    for f in (0.001, 0.01, 0.1):
        idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s1-%g.npy' % (name, cs, f))); m = np.ones(len(L), bool); m[idx] = False
        arms = {'none': L, 'z1': np.where(praw, L, np.minimum(pl['z1'], L)), 'z3': np.where(praw, L, np.minimum(pl['z3'], L))}
        for d in (16, 32, 64, 110):
            for lv in (1, 3):
                c = t['sw_f%g_d%d@%d' % (f, d, lv)]; assert (c[~praw] != MX).all(); arms['pv_f%g_d%d@%d' % (f, d, lv)] = np.where(praw, L, np.minimum(c, L))
        if f == 0.01:
            for lv in (1, 3): c = t['swcover@%d' % lv]; assert (c[~praw] != MX).all(); arms['pv-cover@%d' % lv] = np.where(praw, L, np.minimum(c, L))
        rows(w, name, cs, 'smp-f%g' % f, m, L, cls, arms)
        open(os.path.join(lib.OUT, 'db_sweep_sample_f%g.txt' % f), 'w').write('%d chunks %d bytes\n' % (len(idx), int(L[idx].sum())))
    # pv-cover is measured against the 1 % sample's hold-out; seed 1 only
def fs(kind):
    name = 'db-se-askubuntu-pg-' + kind; img = os.path.join(lib.WORK, name + '.raw'); cs_all = (16384, 65536)
    if not os.path.exists(os.path.join(lib.WORK, name + '-65536.cls')): py(os.path.join(H, 'corpus.py'), 'prep', name, img, 'db', 'pg')
    w = csv.writer(open(os.path.join(lib.OUT, 'db_sens_fs_%s.csv' % kind), 'w', newline='')); w.writerow(['corpus', 'size', 'pop', 'class', 'arm', 'n', 'raw_bytes', 'stored_bytes'])
    for cs in cs_all:
        b, L, cls = common(name, cs); pl = tab(name, cs, 'plain'); praw = pl['probe'] >= 0.94 * np.minimum(L, 4096)
        if not os.path.exists(os.path.join(D, 'pv-%s-%d-s1-all.dict' % (name, cs))): py(os.path.join(H, 'train.py'), 'pv', name, cs, 1)
        d1 = os.path.join(D, 'dbd-pg-se-%d.dict' % cs); assert os.path.exists(d1)
        # dbd-pg-se excludes the se datasets (test family), so it never saw this image's schema; the image itself is not a training image
        corpus.measure(name, cs, 'db', '1,3', ['dbd=' + d1, 'pv1=' + os.path.join(D, 'pv-%s-%d-s1-all.dict' % (name, cs))], flags=['-P', '-n'])
        t = tab(name, cs, 'db'); idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s1-0.01.npy' % (name, cs))); m = np.ones(len(L), bool); m[idx] = False
        arms = {'none': L, 'z1': np.where(praw, L, np.minimum(pl['z1'], L)), 'z3': np.where(praw, L, np.minimum(pl['z3'], L))}
        for lv in (1, 3):
            for a in ('dbd', 'pv1'): arms['%s@%d' % (a, lv)] = np.where(praw, L, np.minimum(t['%s@%d' % (a, lv)], L))
        rows(w, name, cs, 'smp-s1', m, L, cls, arms)
if __name__ == '__main__': sweep() if sys.argv[1] == 'sweep' else fs(sys.argv[2])
