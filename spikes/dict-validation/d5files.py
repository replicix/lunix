#!/usr/bin/env python3
"""Follow-up M2 (Amendment A1, D5): per-object Constellation pass over the four file corpora.  Test host, from ~/dictval, `python3 -I codeC/d5files.py <corpus>`.
corpora: vm-ubuntu vm-debian vm-fedora host-files (host-files = files of the host-ext4 image; the three host-* images carry the same bytes).
Population (protocol 2.5): regular files of the image's root file system, mounted read-only (loop device and mount tracked and released by lib.py, also on
SIGTERM); symlinks, empty files and unreadable files skipped (counted); files with identical content kept once (BLAKE3-256 of the content, first path in walk order);
objects = the whole file if <= 4 MiB, else 4 MiB chunks (last one short).  Per object, all at zstd:3 (dictID off, content size on), stored = min(len, compressed):
  z3      plain zstd:3
  ft      select(Some(name), head, len) of zdicts v0.0.1 (commit 3b8645ef; zdtoolC objsel, head = first 4 KiB of the object, name = path inside the image) -> that
          dictionary at zstd:3, or z3 where select returns none (every object >= 64 KiB, by the 64 KiB rule)
  oracle  min over z3 and the 33 shipped dictionaries
Results: per-object npz work/res/d5-<corpus>.npz (fid = file id = bootstrap unit, len, z3, ft, orc, sel; STAYS ON THE HOST); aggregates out/d5_<corpus>.csv, merged into raw/d5_objects.csv."""
import sys, os, csv, json, subprocess, numpy as np, blake3
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
RES = os.path.join(lib.WORK, 'res'); S = 4 << 20; SMALL = 64 << 10
ZDDIR = os.path.join(lib.WORK, 'zdB'); TOOL = os.path.join(lib.ROOT, 'zdtoolC', 'target', 'release', 'zdtool')
IMG = {'vm-ubuntu': 'vm-ubuntu', 'vm-debian': 'vm-debian', 'vm-fedora': 'vm-fedora', 'host-files': 'host-ext4'}
def walk(m):
    skip = {os.path.join(m, x) for x in ('proc', 'sys', 'dev', 'run', 'lost+found')}
    for dp, dn, fn in os.walk(m, onerror=lambda e: None):
        dn[:] = sorted(d for d in dn if os.path.join(dp, d) not in skip)
        for f in sorted(fn):
            p = os.path.join(dp, f)
            try:
                if os.path.islink(p) or not os.path.isfile(p): continue
                sz = os.path.getsize(p)
            except OSError: continue
            if sz: yield p, sz
def digest(p):
    h = blake3.blake3()
    with open(p, 'rb') as f:
        while True:
            b = f.read(8 << 20)
            if not b: break
            h.update(b)
    return h.digest()
def run(corpus):
    lib.guard(5, 'd5 ' + corpus)
    img = open(os.path.join(lib.WORK, IMG[corpus] + '-16384.img')).read().strip(); mnt = os.path.join(lib.MNT, 'd5-' + corpus)
    try: parts = lib.partitions(img)
    except Exception: parts = []
    off, size = (max(parts, key=lambda p: p[1])[:2]) if parts else (None, None)
    opts_try = ['ro,noatime,noload', 'ro,noatime,subvol=root', 'ro,noatime,subvolid=5'] if corpus == 'vm-fedora' else ['ro,noatime,noload']
    ctx = None
    for o in opts_try:
        try:
            cm = lib.loopmount(img, mnt, offset=off, size=size, ro=True, opts=o); cm.__enter__(); ctx = (cm, o); break
        except Exception as e: print('mount option set failed:', o, str(e)[-120:], flush=True)
    assert ctx, 'no mount'
    cm, used = ctx; info = {'corpus': corpus, 'mount_opts': used.replace('noload', 'noload')}
    try:
        base = os.path.join(lib.WORK, 'd5-' + corpus); seen = set(); files = []; names = []; recs = []; nfiles = nunread = ndup = nbad = 0; bunread = bdup = 0; k = 0
        for p, sz in walk(mnt):
            nfiles += 1
            if '\n' in p or '\0' in p: nbad += 1; continue
            try: d = digest(p)
            except OSError: nunread += 1; bunread += sz; continue
            if d in seen: ndup += 1; bdup += sz; continue
            seen.add(d); fid = len(files); files.append(p); rel = p[len(mnt) + 1:]
            for off_ in range(0, sz, S): recs.append((fid, min(S, sz - off_), off_)); names.append(rel)
        np.array(recs, dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])).tofile(base + '.recs'); open(base + '.files', 'w').write('\n'.join(files) + '\n')
        open(base + '.names', 'wb').write(b''.join(n.encode('utf-8', 'surrogateescape') + b'\0' for n in names)); del names
        info.update(files_seen=nfiles, files_unreadable=nunread, bytes_unreadable=bunread, files_dup=ndup, bytes_dup=bdup, files_bad_name=nbad, files_unique=len(files), objects=len(recs))
        print(corpus, info, flush=True)
        subprocess.run([TOOL, 'objsel', base + '.recs', base + '.files', base + '.names', base + '.sel.u8'], check=True)
        man = [l.rstrip('\n').split('\t') for l in open(os.path.join(ZDDIR, 'manifest.tsv'))]; assert len(man) == 33
        out = os.path.join(RES, 'd5-%s.bin' % corpus)
        cmd = ['taskset', '-c', os.environ.get('DICTVAL_CPUS', '12-15'), os.path.join(lib.ROOT, 'zmeasure2'), '-r', base + '.recs', '-f', base + '.files', '-o', out, '-L', '3', '-t', '4']
        for i in man: cmd += ['-D', 'zd%02d=%s' % (int(i[0]), os.path.join(ZDDIR, i[0] + '.dict'))]
        subprocess.run(cmd, check=True, capture_output=True)
    finally:
        cm.__exit__(None, None, None)
    cols = open(out + '.cols').read().split(); a = np.fromfile(out, dtype=np.uint32).reshape(-1, len(cols)); c = {k_: a[:, i].astype(np.int64) for i, k_ in enumerate(cols)}
    rec = np.fromfile(base + '.recs', dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])); L = rec['len'].astype(np.int64); sel = np.fromfile(base + '.sel.u8', dtype=np.uint8); assert len(sel) == len(L) == len(a)
    z3 = np.minimum(c['z3'], L); M = np.stack([np.minimum(c['zd%02d@3' % k_], L) for k_ in range(1, 34)], axis=1); assert (c['zd01@3'] != 0xFFFFFFFF).all()
    ft = np.where(sel > 0, np.minimum(M[np.arange(len(L)), np.maximum(sel.astype(np.int64) - 1, 0)], L), z3); orc = np.minimum(z3, M.min(axis=1))
    assert (ft <= L).all() and (orc <= z3).all()
    assert not ((sel > 0) & (L >= SMALL)).any(), 'select returned a dictionary for an object of 64 KiB or more'
    np.savez_compressed(os.path.join(RES, 'd5-%s.npz' % corpus), fid=rec['id'], len=L.astype(np.uint32), z3=z3.astype(np.uint32), ft=ft.astype(np.uint32), orc=orc.astype(np.uint32), sel=sel)
    os.remove(out); os.remove(out + '.cols')
    rows = []
    for pop, m in (('all', np.ones(len(L), bool)), ('lt64k', L < SMALL), ('ge64k', L >= SMALL)):
        if not m.any(): continue
        sz3, sft, sor = int(z3[m].sum()), int(ft[m].sum()), int(orc[m].sum())
        rows.append([corpus, pop, int(m.sum()), len(np.unique(rec['id'][m])), int(L[m].sum()), sz3, sft, sor, '%.4f' % (100 * (1 - sft / sz3)), '%.4f' % (100 * (1 - sor / sz3)), int((sel[m] > 0).sum()), int((z3[m] >= L[m]).sum())])
    w = csv.writer(open(os.path.join(lib.OUT, 'd5_%s.csv' % corpus), 'w', newline='')); w.writerow(['corpus', 'pop', 'objects', 'files', 'bytes_in', 'stored_z3', 'stored_ft', 'stored_oracle', 'S_ft_pct', 'S_oracle_pct', 'ft_selected_objects', 'z3_raw_objects']); w.writerows(rows)
    json.dump(info, open(os.path.join(lib.OUT, 'd5_%s.json' % corpus), 'w')); print(corpus, 'done', rows, flush=True)
if __name__ == '__main__': run(sys.argv[1])
