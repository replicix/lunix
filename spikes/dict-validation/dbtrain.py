#!/usr/bin/env python3
"""Database-set dictionaries (arm `db`, section 3, hold-out rules of 4.3).  COVER steps=16, 110 KiB, 24 MiB of `dbpage` chunks equalised over the training images.
usage: dbtrain.py all [threads]      train every dictionary below (skips existing)
  dbd-<eng>-<fam>-<cs>   schema-disjoint: training images = same engine, schema family != <fam>   (primary)
  dbs-<eng>-<ds>-<cs>    same-schema: training images = same engine, every other dataset (reported only; equals dbd for geonames and tlc)
  fam 'ctl' (controls): trained on all real images of the engine.  cs = 16384 and 65536 (training unit = chunk size).
Constellation objects use the 16384 dictionaries.  Assertions: no test image is a training image; dbd excludes the test family."""
import sys, os, json, random, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])
DICTS = os.path.join(lib.WORK, 'dicts'); os.makedirs(DICTS, exist_ok=True)
DS = ['se-askubuntu', 'se-superuser', 'se-serverfault', 'geonames', 'tlc-2024q1']; ENG = ['pg', 'my', 'sq']
def fam(ds): return 'se' if ds.startswith('se-') else 'tlc' if ds.startswith('tlc') else ds
def img(ds, eng): return 'db-%s-%s' % (ds, eng)
def train(out, train_imgs, test_imgs, cs, label, threads):
    assert not (set(train_imgs) & set(test_imgs)), 'hold-out violated'
    if os.path.exists(out): return
    rng = lib.rng_for('dbset', label, cs); want = (24 << 20) // cs; per = want // len(train_imgs); pools = []
    for n in train_imgs:
        rec = np.fromfile(os.path.join(lib.WORK, '%s-%d.recs' % (n, cs)), dtype=REC); cls = np.fromfile(os.path.join(lib.WORK, '%s-%d.cls' % (n, cs)), dtype=np.uint8)
        idx = np.flatnonzero((cls == 6) & (rec['len'] == cs)); rng.shuffle(idx); pools.append((n, rec, idx))
    take = []; left = []
    for n, rec, idx in pools:
        take += [(n, rec, int(k)) for k in idx[:per]]; left += [(n, rec, int(k)) for k in idx[per:]]
    rng.shuffle(left); take += left[:max(0, want - len(take))]; rng.shuffle(take)
    sp = out + '.sample'; fh = {}
    with open(sp, 'wb') as o:
        for n, rec, k in take:
            if n not in fh: fh[n] = open(open(os.path.join(lib.WORK, '%s-%d.img' % (n, cs))).read().strip(), 'rb')
            fh[n].seek(int(rec['off'][k])); o.write(fh[n].read(cs))
    for f in fh.values(): f.close()
    r = subprocess.run(['zstd', '--train-cover=steps=16', '-B%d' % cs, '--maxdict=112640', '-T%d' % threads, '-q', '-f', '-o', out, sp], capture_output=True, text=True); os.remove(sp)
    ok = r.returncode == 0 and os.path.exists(out)
    json.dump({'label': label, 'cs': cs, 'train': train_imgs, 'test': test_imgs, 'chunks': len(take), 'ok': ok, 'err': r.stderr[-300:]}, open(out + '.json', 'w'))
    print(label, cs, len(take), 'ok' if ok else r.stderr[-200:], flush=True)
if __name__ == '__main__':
    th = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    for cs in (16384, 65536):
        for eng in ENG:
            for f in ('se', 'geonames', 'tlc'):
                tr = [img(d, eng) for d in DS if fam(d) != f]; te = [img(d, eng) for d in DS if fam(d) == f]
                assert all(fam(d) != f for d in DS if img(d, eng) in tr)
                train(os.path.join(DICTS, 'dbd-%s-%s-%d.dict' % (eng, f, cs)), tr, te, cs, 'dbd-%s-%s' % (eng, f), th)
            for d in DS[:3]:
                train(os.path.join(DICTS, 'dbs-%s-%s-%d.dict' % (eng, d, cs)), [img(x, eng) for x in DS if x != d], [img(d, eng)], cs, 'dbs-%s-%s' % (eng, d), th)
            train(os.path.join(DICTS, 'dbd-%s-ctl-%d.dict' % (eng, cs)), [img(d, eng) for d in DS], [], cs, 'dbd-%s-ctl' % eng, th)
