#!/usr/bin/env python3
"""zdicts arms `ft` and `oracle` for a database image (Lunix view), zdicts v0.0.1 (commit 3b8645ef) only.
usage: dbzd.py <name> <cs>
  `zdtoolB sel` = select_by_content on the first 4 KiB of each chunk (0 = no dictionary); every one of the 33 shipped dictionaries is measured on every
  chunk that passes the ADR-12 probe (zmeasure -P -n), levels 1 and 3; result work/res/zd-<name>-<cs>.npz keeps, per level, `ft` (size under the selected
  dictionary, MAXV where none selected or probe-raw) and `orc` (minimum over the 33 dictionaries, MAXV for probe-raw) plus `sel` and the per-dictionary
  win counts; the 33-column bin is deleted.  dbagg.py combines them with plain, `dbd` and the raw guard."""
import sys, os, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, corpus
name, cs = sys.argv[1], int(sys.argv[2]); RES = os.path.join(lib.WORK, 'res'); MAXV = 0xFFFFFFFF
ZD = os.path.join(lib.WORK, 'zdB'); base = os.path.join(lib.WORK, '%s-%d' % (name, cs)); img = open(base + '.img').read().strip()
out = os.path.join(RES, 'zd-%s-%d.npz' % (name, cs))
if os.path.exists(out): sys.exit(0)
ids = [l.split('\t') for l in open(os.path.join(ZD, 'manifest.tsv')).read().split('\n') if l]; assert len(ids) == 33
sp = os.path.join(lib.WORK, 'zdsel-%s-%d.u8' % (name, cs))
subprocess.run([os.path.join(lib.ROOT, 'zdtoolB', 'target', 'release', 'zdtool'), 'sel', base + '.recs', img, sp], check=True)
sel = np.fromfile(sp, dtype=np.uint8); os.remove(sp)
corpus.measure(name, cs, 'zdtmp', '1,3', ['zd%02d=%s' % (int(i[0]), os.path.join(ZD, i[0] + '.dict')) for i in ids], flags=['-P', '-n'])
b = os.path.join(RES, '%s-%d.zdtmp.bin' % (name, cs)); cols = open(b + '.cols').read().split(); a = np.fromfile(b, dtype=np.uint32).reshape(-1, len(cols)); assert len(a) == len(sel)
res = {'sel': sel}
for L in (1, 3):
    M = np.stack([a[:, cols.index('zd%02d@%d' % (k, L))] for k in range(1, 34)], axis=1)
    ev = M[:, 0] != MAXV; assert ((M != MAXV).all(axis=1) == ev).all()
    orc = np.where(ev, M.min(axis=1), MAXV).astype(np.uint32)
    pick = np.where(sel > 0, M[np.arange(len(sel)), np.maximum(sel.astype(np.int64) - 1, 0)], MAXV).astype(np.uint32)
    res['orc%d' % L] = orc; res['ft%d' % L] = pick; res['win%d' % L] = np.bincount(M.argmin(axis=1)[ev], minlength=33)
np.savez_compressed(out, **res); os.remove(b); os.remove(b + '.cols'); print(name, cs, 'zd done', int((sel > 0).sum()), 'selected of', len(sel), flush=True)
