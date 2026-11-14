#!/usr/bin/env python3
"""Constellation view of a database image (sections 2.3, 2.5): the files of the engine's data directory as objects, whole file if under
the object size, else chunks of that size; sizes 4 MiB, 1 MiB and the engine page size (reference).  Files are read from the image mounted
read-only (the mount is held for the whole step); no object is exported.  All-zero objects are kept in the population (the class column of
the measurement marks them) so that the analysis can report their share.
usage: dbview.py plain <name>                 build records + plain pass (z3) for all three sizes     -> work/cv-<name>-<size>.{recs,files,pcls,shares.json}, res/cv-<name>-<size>.plain.bin
       dbview.py pvtrain <name> <seed>        per-volume dictionaries (whole-object sample, fastCover, 16 KiB units) for each size -> work/dicts/cvpv-<name>-<size>-s<seed>.dict + res/cvpvsample-*.npy
       dbview.py arms <name> <tag> <dictname=path>...   measure dictionaries on all three sizes (level 3)"""
import sys, os, json, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib, label
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])
RES = os.path.join(lib.WORK, 'res'); DICTS = os.path.join(lib.WORK, 'dicts'); os.makedirs(RES, exist_ok=True); os.makedirs(DICTS, exist_ok=True)
def info(name): return json.load(open(os.path.join(lib.OUT, name + '.json')))
def sizes(name): return [('4m', 4 << 20), ('1m', 1 << 20), ('pg', info(name)['page_size'])]
def mntp(name): return os.path.join(lib.MNT, 'dbv-' + name)
def base(name, tag): return os.path.join(lib.WORK, 'cv-%s-%s' % (name, tag))
def zm(name, tag, out, dicts, levels='3', threads=2):
    cmd = [os.path.join(lib.ROOT, 'zmeasure'), '-r', base(name, tag) + '.recs', '-f', base(name, tag) + '.files', '-o', out, '-L', levels, '-t', str(threads)]
    for d in dicts: cmd += ['-D', d]
    subprocess.run(cmd, check=True, capture_output=True)
def build(name):
    i = info(name); eng = i['engine']; m = mntp(name); sub = {'pg': 'pg', 'my': 'mysql', 'sq': 'sqlite'}[eng]; files = []
    for dp, dn, fn in os.walk(os.path.join(m, sub)):
        for f in sorted(fn):
            p = os.path.join(dp, f)
            if os.path.islink(p) or not os.path.isfile(p): continue
            sz = os.path.getsize(p)
            if sz: files.append((p, sz))
    files.sort()
    for tag, S in sizes(name):
        b = base(name, tag); rec = []; pc = []
        for fid, (p, sz) in enumerate(files):
            c = label.path_class_db(p[len(m):], eng)
            for off in range(0, sz, S): rec.append((fid, min(S, sz - off), off)); pc.append(c)
        a = np.array(rec, dtype=REC); a.tofile(b + '.recs'); np.array(pc, np.uint8).tofile(b + '.pcls'); open(b + '.files', 'w').write('\n'.join(p for p, _ in files) + '\n')
        json.dump({'files': len(files), 'objects': len(rec), 'bytes': int(a['len'].sum()), 'object_size': S}, open(b + '.shares.json', 'w'))
def plain(name):
    with lib.loopmount(os.path.join(lib.WORK, name + '.raw'), mntp(name), ro=True):
        build(name)
        for tag, S in sizes(name): zm(name, tag, os.path.join(RES, 'cv-%s-%s.plain.bin' % (name, tag)), [], '3')
        print(name, 'plain done', flush=True)
def pvtrain(name, seed):
    import random
    with lib.loopmount(os.path.join(lib.WORK, name + '.raw'), mntp(name), ro=True):
        for tag, S in sizes(name):
            b = base(name, tag); rec = np.fromfile(b + '.recs', dtype=REC); files = open(b + '.files').read().split('\n')
            cls = np.fromfile(os.path.join(RES, 'cv-%s-%s.plain.bin' % (name, tag)), dtype=np.uint32).reshape(len(rec), -1)[:, 1]
            tot = int(rec['len'].sum()); want = min(max(tot // 100, 8 << 20), 64 << 20)
            elig = [k for k in range(len(rec)) if cls[k] != 0]; r = random.Random(lib.seed_for('cvpv', name, tag, seed)); r.shuffle(elig)
            idx = []; got = 0
            for k in elig:
                if got >= want: break
                idx.append(k); got += int(rec['len'][k])
            idx = np.array(sorted(idx), dtype=np.int64); np.save(os.path.join(RES, 'cvpvsample-%s-%s-s%d.npy' % (name, tag, seed)), idx)
            sp = os.path.join(DICTS, 'cvpv-%s-%s-s%d.sample' % (name, tag, seed))
            with open(sp, 'wb') as o:
                for k in idx:
                    with open(files[rec['id'][k]], 'rb') as f: f.seek(int(rec['off'][k])); o.write(f.read(int(rec['len'][k])))
            out = os.path.join(DICTS, 'cvpv-%s-%s-s%d.dict' % (name, tag, seed))
            rr = subprocess.run(['zstd', '--train-fastcover=k=1024,d=8,f=20,accel=1', '-B16384', '--maxdict=112640', '-T1', '-q', '-f', '-o', out, sp], capture_output=True, text=True)
            os.remove(sp); print(name, tag, seed, len(idx), got, 'ok' if rr.returncode == 0 else rr.stderr[-200:], flush=True)
def arms(name, tag, dicts):
    with lib.loopmount(os.path.join(lib.WORK, name + '.raw'), mntp(name), ro=True):
        for t, S in sizes(name):
            ds = [d for d in dicts if os.path.exists(d.split('=', 1)[1].replace('{sz}', t))]
            zm(name, t, os.path.join(RES, 'cv-%s-%s.%s.bin' % (name, t, tag)), [d.replace('{sz}', t) for d in ds], '3')
        print(name, 'arms', tag, 'done', flush=True)
if __name__ == '__main__':
    a = sys.argv[1:]
    if a[0] == 'plain': plain(a[1])
    elif a[0] == 'pvtrain': pvtrain(a[1], int(a[2]))
    elif a[0] == 'arms': arms(a[1], a[2], a[3:])
