#!/usr/bin/env python3
"""Arm drivers for the database images.  usage:
  dbarms.py lunix <name>     per-volume trainings (pv, seeds 1-3, 16 and 64 KiB) + one measurement pass with all dictionaries (tag `db`)
  dbarms.py cv <name>        Constellation view: plain pass, per-volume whole-object trainings (seeds 1-3), measurement pass (tag `db`)"""
import sys, os, subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, corpus
D = os.path.join(lib.WORK, 'dicts'); H = os.path.dirname(os.path.abspath(__file__))
def parse(name):
    p = name.split('-'); eng = p[-1]; ds = '-'.join(p[1:-1]); return ds, eng
def fam(ds): return 'se' if ds.startswith('se-') else 'tlc' if ds.startswith('tlc') else 'ctl' if ds.startswith('ctl') else ds
def py(*a): subprocess.run(['python3', '-I'] + [str(x) for x in a], check=True)
def dbd_dbs(ds, eng, cs):
    d1 = os.path.join(D, 'dbd-%s-%s-%d.dict' % (eng, fam(ds), cs)); d2 = os.path.join(D, 'dbs-%s-%s-%d.dict' % (eng, ds, cs))
    return d1, (d2 if os.path.exists(d2) else d1)
def lunix(name):
    ds, eng = parse(name)
    for cs in (16384, 65536):
        for s in (1, 2, 3):
            if not os.path.exists(os.path.join(D, 'pv-%s-%d-s%d-all.dict' % (name, cs, s))): py(os.path.join(H, 'train.py'), 'pv', name, cs, s)
        d1, d2 = dbd_dbs(ds, eng, cs); dl = ['dbd=' + d1, 'dbs=' + d2]
        for s in (1, 2, 3):
            for c, tag in (('all', ''), ('meta', 'm'), ('text', 't'), ('bin', 'b')):
                if c != 'all' and s != 1: continue
                p = os.path.join(D, 'pv-%s-%d-s%d-%s.dict' % (name, cs, s, c))
                if os.path.exists(p): dl.append('pv%d%s=%s' % (s, tag, p))
        corpus.measure(name, cs, 'db', '1,3', dl); print(name, cs, 'lunix arms done', flush=True)
def cv(name):
    ds, eng = parse(name); d1, d2 = dbd_dbs(ds, eng, 16384)
    if not os.path.exists(os.path.join(lib.RES if hasattr(lib, 'RES') else os.path.join(lib.WORK, 'res'), 'cv-%s-4m.plain.bin' % name)): py(os.path.join(H, 'dbview.py'), 'plain', name)
    for s in (1, 2, 3): py(os.path.join(H, 'dbview.py'), 'pvtrain', name, s)
    py(os.path.join(H, 'dbview.py'), 'arms', name, 'db', 'dbd=' + d1, 'dbs=' + d2, *['pv%d=%s' % (s, os.path.join(D, 'cvpv-%s-{sz}-s%d.dict' % (name, s))) for s in (1, 2, 3)])
if __name__ == '__main__': {'lunix': lunix, 'cv': cv}[sys.argv[1]](sys.argv[2])
