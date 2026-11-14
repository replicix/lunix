#!/usr/bin/env python3
"""Timing samples (protocol "Timing"): a 1 GiB block-stratified in-memory sample per corpus and view, built on the test host.
Run from ~/dictval under `taskset -c 8-11 python3 -I code/tmsample.py ...`; output under ~/dictval/tm/<cell>/ (sample.bin + record files).
  tmsample.py lunix <corpus> <cs>      Lunix view: unique non-hole chunks of work/<corpus>-<cs>.recs; whole 1 MiB origin blocks are drawn
                                       systematically (equal probability, ordered by block) until about 1 GiB -> recs
  tmsample.py files <corpus>           Constellation file objects of a VM image (root partition mounted read-only): regular files,
                                       identical content once, whole file if <= 4 MiB else 4 MiB chunks; files drawn systematically in path
                                       order -> recs (objects) + names.txt (path inside the image, one per object)
  tmsample.py db <dataset-engine>      database image (data directory mounted read-only): objects at 4 MiB (recs4m), the same bytes cut at
                                       1 MiB (recs1m), and the Lunix DB-page view at 16 KiB (recs16: 16 KiB cuts of the sampled objects of
                                       dbpage files, zero cuts dropped, duplicates once)
Record files are 16-byte records {u32 0, u32 len, u64 off into sample.bin}.  Draws are systematic with a fraction f found by bisection so
that the sample is close to 1 GiB; the achieved size and unit counts are printed.  Mounts: tracked, released in finally blocks."""
import sys, os, json, struct, subprocess, re, contextlib
import hashlib
import numpy as np
ROOT = os.path.expanduser(os.environ.get('DICTVAL_ROOT', '~/dictval')); WORK = os.path.join(ROOT, 'work'); TM = os.path.join(ROOT, 'tm'); TARGET = 1 << 30
def h16(b): return hashlib.blake2b(b, digest_size=16).digest()
def sh(cmd): return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.strip()
@contextlib.contextmanager
def ro_mount(img, mnt, offset=None, size=None):
    os.makedirs(mnt, exist_ok=True); cmd = ['sudo', 'losetup', '-r', '-f', '--show']
    if offset is not None: cmd += ['-o', str(offset), '--sizelimit', str(size)]
    dev = sh(cmd + [img])
    try:
        subprocess.run(['sudo', 'mount', '-o', 'ro,noatime,noload', dev, mnt], check=True)
        try: yield mnt
        finally: subprocess.run(['sudo', 'umount', mnt], check=True)
    finally: subprocess.run(['sudo', 'losetup', '-d', dev], check=True)
def pick(sizes, target=TARGET):
    """Indices of a systematic equal-probability draw whose total size is closest to target (bisection on the fraction)."""
    n = len(sizes); sz = np.asarray(sizes, dtype=np.int64)
    if sz.sum() <= target: return np.arange(n)
    def draw(f):
        c = np.floor(np.arange(1, n + 1) * f) - np.floor(np.arange(0, n) * f); return np.nonzero(c > 0)[0]
    lo, hi, best = 0.0, 1.0, None
    for _ in range(40):
        f = (lo + hi) / 2; idx = draw(f); tot = int(sz[idx].sum())
        if best is None or abs(tot - target) < abs(best[0] - target): best = (tot, idx)
        if tot < target: lo = f
        else: hi = f
    return best[1]
def write_recs(path, recs): np.array(recs, dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])).tofile(path)
def lunix(name, cs):
    out = os.path.join(TM, '%s-%d' % (name, cs)); os.makedirs(out, exist_ok=True)
    rec = np.fromfile(os.path.join(WORK, '%s-%d.recs' % (name, cs)), dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]))
    img = open(os.path.join(WORK, '%s-%d.img' % (name, cs))).read().strip(); blk = (rec['off'] >> 20).astype(np.int64)
    ub, inv = np.unique(blk, return_inverse=True); bsz = np.bincount(inv, weights=rec['len']).astype(np.int64)
    sel = pick(bsz); keep = np.isin(inv, sel); r = rec[keep]; recs = []; o = 0
    with open(img, 'rb') as f, open(os.path.join(out, 'sample.bin'), 'wb') as w:
        for x in r:
            f.seek(int(x['off'])); b = f.read(int(x['len'])); assert len(b) == x['len']; w.write(b); recs.append((0, int(x['len']), o)); o += len(b)
    write_recs(os.path.join(out, 'recs'), recs)
    print(name, cs, 'blocks', len(sel), 'of', len(ub), 'chunks', len(recs), 'bytes', o)
def walk_files(m):
    res = []
    for dp, dn, fn in os.walk(m, onerror=lambda e: None):
        dn[:] = sorted(d for d in dn if os.path.join(dp, d) not in (os.path.join(m, x) for x in ('proc', 'sys', 'dev', 'run', 'lost+found')))
        for f in sorted(fn):
            p = os.path.join(dp, f)
            try:
                if os.path.islink(p) or not os.path.isfile(p): continue
                sz = os.path.getsize(p)
            except OSError: continue
            if sz: res.append((p, sz))
    return res
def hash_file(p):
    h = hashlib.blake2b(digest_size=16)
    with open(p, 'rb') as f:
        while True:
            b = f.read(8 << 20)
            if not b: break
            h.update(b)
    return h.digest()
def files(name):
    out = os.path.join(TM, '%s-files' % name); os.makedirs(out, exist_ok=True)
    img = open(os.path.join(WORK, '%s-16384.img' % name)).read().strip()
    parts = json.loads(sh(['sfdisk', '-J', img]))['partitiontable']['partitions']
    root = max(parts, key=lambda p: p['size'])          # the root file system is by far the largest partition
    S = 4 << 20
    with ro_mount(img, os.path.join(TM, 'mnt-' + name), root['start'] * 512, root['size'] * 512) as m:
        fl = walk_files(m); seen = set(); uniq = []; unreadable = 0
        for p, sz in fl:
            try: d = hash_file(p)
            except OSError: unreadable += 1; continue
            if d in seen: continue
            seen.add(d); uniq.append((p, sz))
        sel = pick([sz for _, sz in uniq]); recs = []; names = []; o = 0
        with open(os.path.join(out, 'sample.bin'), 'wb') as w:
            for k in sel:
                p, sz = uniq[k]
                with open(p, 'rb') as f:
                    for off in range(0, sz, S):
                        b = f.read(min(S, sz - off)); w.write(b); recs.append((0, len(b), o)); o += len(b); names.append(p[len(m) + 1:])
    write_recs(os.path.join(out, 'recs'), recs); open(os.path.join(out, 'names.txt'), 'w').write('\n'.join(names) + '\n')
    sm = np.array([r[1] for r in recs]); print(name, 'files', len(fl), 'unique', len(uniq), 'unreadable', unreadable, 'sampled files', len(sel), 'objects', len(recs), 'bytes', o, 'objects<64KiB', int((sm < 65536).sum()), 'bytes in <64KiB', int(sm[sm < 65536].sum()))
def pg_dbpage(rel):
    p = rel.lower()
    if '/pg_wal/' in p: return False
    if re.search(r'/base/\d+/\d+(\.\d+)?$', p) and not p.endswith('pg_filenode.map'): return int(os.path.basename(p).split('.')[0]) >= 16384
    return False
def db(name):
    assert name.endswith('-pg'), 'only PostgreSQL images are sampled for timing (see SUMMARY deviations)'
    out = os.path.join(TM, name); os.makedirs(out, exist_ok=True); S = 4 << 20; C = 16384
    with ro_mount(os.path.join(WORK, name + '.raw'), os.path.join(TM, 'mnt-' + name)) as m:
        fl = walk_files(os.path.join(m, 'pg')); fl.sort()
        objs = [(i, off, min(S, sz - off)) for i, (p, sz) in enumerate(fl) for off in range(0, sz, S)]
        sel = pick([o[2] for o in objs]); r4 = []; r1 = []; r16 = []; o = 0; seen = set(); zero = bytes(C)
        with open(os.path.join(out, 'sample.bin'), 'wb') as w:
            for k in sel:
                fi, off, ln = objs[k]; p = fl[fi][0]
                with open(p, 'rb') as f: f.seek(off); b = f.read(ln)
                assert len(b) == ln; w.write(b); r4.append((0, ln, o))
                for s in range(0, ln, 1 << 20): r1.append((0, min(1 << 20, ln - s), o + s))
                if pg_dbpage(p[len(m):]):
                    for s in range(0, ln, C):
                        c = b[s:s + C]
                        if not any(c): continue
                        d = h16(c)
                        if d in seen: continue
                        seen.add(d); r16.append((0, len(c), o + s))
                o += ln
    for n, r in (('recs4m', r4), ('recs1m', r1), ('recs16', r16)): write_recs(os.path.join(out, n), r)
    print(name, 'files', len(fl), 'objects4m', len(objs), 'sampled', len(sel), 'bytes', o, 'recs1m', len(r1), 'recs16', len(r16), 'bytes16', sum(x[1] for x in r16))
if __name__ == '__main__':
    a = sys.argv[1:]; os.makedirs(TM, exist_ok=True)
    {'lunix': lambda: lunix(a[1], int(a[2])), 'files': lambda: files(a[1]), 'db': lambda: db(a[1])}[a[0]]()
