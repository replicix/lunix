#!/usr/bin/env python3
"""Phase 6: dictionary training (zstd CLI 1.5.7).  Subcommands:
  bs <dist|xfs> <family> [exclfs]      block-set class dictionaries (COVER steps=16, 24 MiB per class from the pool samples)
  pv <name> <cs> <seed> [frac dsize]   per-volume dictionary + 3 class dictionaries (fastCover k=1024,d=8,f=20,accel=1) from a uniform sample;
                                       writes work/dicts/pv-<name>-<cs>-s<seed>[-<frac>-<dsize>]-{all,meta,text,bin}.dict and the sample index (.sample.npy)
  pvcover <name> <cs> <seed>           the same sample, COVER optimiser (steps=16), single dictionary
  dbset <engine> <tag> <img>...        database set (see run_db.py)
Class numbers: 1 meta, 2 text, 3 binary (cheap classifier, cls.h)."""
import sys, os, json, glob, random, subprocess, numpy as np, blake3
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
DICTS = os.path.join(lib.WORK, 'dicts'); os.makedirs(DICTS, exist_ok=True)
DSIZE = 112640; CN = {1: 'meta', 2: 'text', 3: 'bin'}
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])
def zstd_train(sample, out, unit, mode, dsize=DSIZE):
    p = '--train-cover=steps=16' if mode == 'cover' else '--train-fastcover=k=1024,d=8,f=20,accel=1'
    r = subprocess.run(['zstd', p, '-B%d' % unit, '--maxdict=%d' % dsize, '-T2', '-q', '-f', '-o', out, sample], capture_output=True, text=True)
    if r.returncode or not os.path.exists(out): return False, r.stderr[-300:]
    return True, ''
def bs(kind, fam, exclfs=None):
    views = []
    for j in glob.glob(os.path.join(lib.WORK, 'pool', '*.json')):
        v = json.load(open(j))
        if v['family'] == fam: continue                       # hold-out by distribution family
        if kind == 'xfs' and v['fs'] == exclfs: continue      # also by file system
        views.append(v)
    assert views and all(v['family'] != fam for v in views), 'hold-out violated: a training view is of the test distribution family'
    if kind == 'xfs': assert exclfs and all(v['fs'] != exclfs for v in views), 'hold-out violated: a training view has the test file system'
    tag = 'dist-%s' % fam if kind == 'dist' else 'xfs-%s-%s' % (fam, exclfs); man = {'tag': tag, 'kind': kind, 'test_family': fam, 'excl_fs': exclfs, 'views': ['%s-%s' % (v['distro'], v['view']) for v in views]}
    rng = lib.rng_for('bs', tag); hs = []
    for c in (1, 2, 3):
        pools = []
        for v in views:
            f = os.path.join(lib.WORK, 'pool', '%s-%s-%d.bin' % (v['distro'], v['view'], c)); b = open(f, 'rb').read(); pools.append([b[i:i + 16384] for i in range(0, len(b), 16384)])
        want = (24 << 20) // 16384; take = []; per = max(1, want // len(pools))
        for p in pools: rng.shuffle(p)
        for p in pools: take += p[:per]; del p[:per]
        rest = [x for p in pools for x in p]; rng.shuffle(rest); take += rest[:max(0, want - len(take))]; rng.shuffle(take)
        sp = os.path.join(DICTS, 'bs-%s-%s.sample' % (tag, CN[c])); open(sp, 'wb').write(b''.join(take))
        hs.extend(blake3.blake3(x).digest()[:16] for x in take)
        out = os.path.join(DICTS, 'bs-%s-%s.dict' % (tag, CN[c])); ok, err = zstd_train(sp, out, 16384, 'cover')
        man[CN[c]] = {'chunks': len(take), 'ok': ok, 'err': err}; os.remove(sp)
    open(os.path.join(DICTS, 'bs-%s.h16' % tag), 'wb').write(b''.join(hs))
    assert all(man[CN[c]]['ok'] for c in (1, 2, 3)), man
    json.dump(man, open(os.path.join(DICTS, 'bs-%s.json' % tag), 'w')); print(man)
def read_chunks(img, rec, idx):
    out = []
    with open(img, 'rb') as f:
        for i in idx: f.seek(int(rec['off'][i])); out.append(f.read(int(rec['len'][i])))
    return out
def pv(name, cs, seed, frac=None, dsize=DSIZE, mode='fastcover'):
    base = os.path.join(lib.WORK, '%s-%d' % (name, cs)); rec = np.fromfile(base + '.recs', dtype=REC)
    cls = np.fromfile(os.path.join(lib.WORK, 'res', '%s-%d.plain.bin' % (name, cs)), dtype=np.uint32).reshape(len(rec), -1)[:, 1] if mode != 'x' else None
    img = open(base + '.img').read().strip()
    tot = int(rec['len'].sum()); frac = 0.01 if frac is None else frac; cap = 64 << 20 if frac <= 0.01 else 1536 << 20     # product path caps at 64 MiB; the 10 % sweep cell is not capped (SUMMARY deviation)
    want = min(max(int(tot * frac), 8 << 20), cap) // cs
    sdir = os.path.join(DICTS, 'pv-%s-%d-s%d%s' % (name, cs, seed, '' if frac == 0.01 and dsize == DSIZE else '-%g-%d' % (frac, dsize // 1024)))
    sidx = os.path.join(lib.WORK, 'res', 'pvsample-%s-%d-s%d-%g.npy' % (name, cs, seed, frac))
    if os.path.exists(sidx): idx = np.load(sidx)
    else:
        full = np.flatnonzero(rec['len'] == cs); r = random.Random(lib.seed_for('pv', name, cs, seed)); idx = np.array(sorted(r.sample(list(full), min(want, len(full)))), dtype=np.int64); np.save(sidx, idx)
    data = read_chunks(img, rec, idx); res = {}
    jobs = [('all', list(range(len(idx))))]
    if mode == 'fastcover' and frac == 0.01 and dsize == DSIZE:
        for c in (1, 2, 3): jobs.append((CN[c], [k for k in range(len(idx)) if cls[idx[k]] == c]))
    for nm, ks in jobs:
        if len(ks) < 64: res[nm] = 'skipped(%d)' % len(ks); continue
        sp = sdir + '-%s.sample' % nm; open(sp, 'wb').write(b''.join(data[k] for k in ks))
        ok, err = zstd_train(sp, sdir + ('-%s.dict' % nm if mode == 'fastcover' else '-cover.dict'), cs, 'cover' if mode == 'cover' else 'fastcover', dsize); os.remove(sp)
        res[nm] = 'ok' if ok else err
    print(name, cs, seed, frac, dsize, mode, res, flush=True)
if __name__ == '__main__':
    a = sys.argv[1:]
    if a[0] == 'bs': bs(a[1], a[2], a[3] if len(a) > 3 else None)
    elif a[0] == 'pv': pv(a[1], int(a[2]), int(a[3]), float(a[4]) if len(a) > 4 else None, int(a[5]) * 1024 if len(a) > 5 else DSIZE)
    elif a[0] == 'pvcover': pv(a[1], int(a[2]), int(a[3]), None, DSIZE, 'cover')
