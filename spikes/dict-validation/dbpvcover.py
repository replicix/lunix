#!/usr/bin/env python3
"""Follow-up M1 (Amendment A1): pv-cover on the 15 database images at 16 and 64 KiB.  Test host, from ~/dictval, `taskset -c 12-15`, `python3 -I codeC/dbpvcover.py ...`.
  train <name> <cs>     COVER optimiser (--train-cover=steps=16), seed-1 1 % sample of `pv` (same sample file work/res/pvsample-*-s1-0.01.npy), 110 KiB;
                        dictionary work/dicts/pv-<name>-<cs>-s1-cover.dict (an existing one is reused: the sample and the trainer are deterministic)
  measure <name> <cs>   zmeasure2 -P -n at levels 1 and 3 with that dictionary only (behind the ADR-12 probe, plain pass skipped);
                        per-chunk result work/res/<name>-<cs>.pvcover.bin (+ .cols: probe class z1 pvcover1@1 z3 pvcover1@3; same u32 layout as <name>-<cs>.db.bin,
                        dictionary column `pvcover1@L`, 0xFFFFFFFF = not evaluated because the probe sent the chunk raw)  -- stays on the host
  agg <outdir>          out/db_pvcover_arms.csv, same columns as db_lunix_arms.csv, pop `pv-s1` (chunks outside the 1 % sample), classes all/dbpage/wal/dbother/other,
                        arms none, z1, z3, probe, z3-probe, pv@1, pv@3 (recomputed from <name>-<cs>.db.bin) and pv-cover@1, pv-cover@3
Stored bytes = raw if the probe saves under 6 %, else min(zstd, chunk length)."""
import sys, os, csv, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]); RES = os.path.join(lib.WORK, 'res'); D = os.path.join(lib.WORK, 'dicts'); H = os.path.dirname(os.path.abspath(__file__))
DS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; NAMES = ['db-%s-%s' % (d, e) for d in DS for e in ('pg', 'my', 'sq')]
MX = 0xFFFFFFFF; NM = {6: 'dbpage', 7: 'wal', 8: 'dbother'}
def dpath(name, cs): return os.path.join(D, 'pv-%s-%d-s1-cover.dict' % (name, cs))
def train(name, cs):
    if os.path.exists(dpath(name, cs)): print(name, cs, 'dictionary exists', flush=True); return
    subprocess.run(['python3', '-I', os.path.join(H, 'train.py'), 'pvcover', name, str(cs), '1'], check=True)
    assert os.path.exists(dpath(name, cs)), 'training produced no dictionary'
def measure(name, cs):
    base = os.path.join(lib.WORK, '%s-%d' % (name, cs)); img = open(base + '.img').read().strip(); fl = base + '.files'; open(fl, 'w').write(img + '\n')
    out = os.path.join(RES, '%s-%d.pvcover.bin' % (name, cs))
    cmd = ['taskset', '-c', os.environ.get('DICTVAL_CPUS', '12-15'), os.path.join(lib.ROOT, 'zmeasure2'), '-P', '-n', '-r', base + '.recs', '-f', fl, '-o', out, '-L', '1,3', '-t', '4', '-D', 'pvcover1=' + dpath(name, cs)]
    subprocess.run(cmd, check=True, capture_output=True)
    cols = open(out + '.cols').read().split(); assert cols == ['probe', 'class', 'z1', 'pvcover1@1', 'z3', 'pvcover1@3'], cols
    print(name, cs, 'measured', flush=True)
def tab(p):
    c = open(p + '.cols').read().split(); a = np.fromfile(p, dtype=np.uint32).reshape(-1, len(c)); return {k: a[:, i].astype(np.int64) for i, k in enumerate(c)}
def agg(outdir):
    w = csv.writer(open(os.path.join(outdir, 'db_pvcover_arms.csv'), 'w', newline='')); w.writerow(['corpus', 'view', 'size', 'pop', 'class', 'arm', 'n', 'raw_bytes', 'stored_bytes'])
    for name in NAMES:
        for cs in (16384, 65536):
            b = os.path.join(lib.WORK, '%s-%d' % (name, cs)); rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64)
            cls = np.array([NM.get(int(x), 'other') for x in np.fromfile(b + '.cls', dtype=np.uint8)])
            C = tab(os.path.join(RES, '%s-%d.db.bin' % (name, cs))); P = tab(os.path.join(RES, '%s-%d.pvcover.bin' % (name, cs)))
            assert len(L) == len(C['probe']) == len(P['probe']) and (C['probe'] == P['probe']).all(), 'probe columns differ'
            praw = C['probe'] >= 0.94 * np.minimum(L, 4096); beh = lambda z: np.where(praw, L, np.minimum(z, L))
            idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s1-0.01.npy' % (name, cs))); m = np.ones(len(L), bool); m[idx] = False
            arms = {'none': L, 'z1': np.minimum(C['z1'], L), 'z3': np.minimum(C['z3'], L), 'probe': beh(C['z1']), 'z3-probe': beh(C['z3'])}
            for lv in (1, 3):
                arms['pv@%d' % lv] = beh(C['pv1@%d' % lv]); c = P['pvcover1@%d' % lv]; assert (c[~praw] != MX).all(), 'unevaluated chunk'; assert (c[praw] == MX).all()
                arms['pv-cover@%d' % lv] = beh(c)
            for cl in ('all', 'dbpage', 'wal', 'dbother', 'other'):
                mm = m if cl == 'all' else m & (cls == cl)
                for a, v in arms.items(): w.writerow([name, 'lunix', cs, 'pv-s1', cl, a, int(mm.sum()), int(L[mm].sum()), int(v[mm].sum())])
    print('agg done')
if __name__ == '__main__':
    a = sys.argv[1:]
    if a[0] == 'train': train(a[1], int(a[2]))
    elif a[0] == 'measure': measure(a[1], int(a[2]))
    elif a[0] == 'agg': agg(a[1])
