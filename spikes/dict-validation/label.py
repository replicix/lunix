#!/usr/bin/env python3
"""Phase 3: class labels per 4 KiB block of a raw device (section 2.4).  usage: label.py <image> <name> [db <engine>] [--tmp mntdir]
Writes work/labels-<name>.u8 (class per 4 KiB block of the whole device), work/labels-<name>.fid (u32 file id per block, 0 none),
work/files-<name>.tsv (id, kind, size, path; host only) and out/labels-<name>.json (aggregate counts).
Classes: 1 data, 2 dir, 3 meta, 4 journal, 5 unmapped, 6 dbpage, 7 wal, 8 dbother, 9 otherpart, 10 ptable."""
import sys, os, re, json, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
CL = dict(data=1, dir=2, meta=3, journal=4, unmapped=5, dbpage=6, wal=7, dbother=8, otherpart=9, ptable=10)
def path_class_db(path, engine):
    p = path.lower()
    if engine == 'pg':
        if '/pg_wal/' in p: return CL['wal']
        if re.search(r'/base/\d+/\d+(\.\d+)?$', p) and not p.endswith(('pg_filenode.map',)): 
            n = os.path.basename(p).split('.')[0]
            return CL['dbpage'] if int(n) >= 16384 else CL['dbother']
        return CL['dbother']
    if engine == 'my':
        if '#innodb_redo' in p or '/undo_' in p or p.endswith(('/ib_logfile0', '/ib_logfile1')): return CL['wal']
        if p.endswith('.ibd') and '/mysql.ibd' not in p and '/sys/' not in p: return CL['dbpage']
        return CL['dbother']
    if p.endswith('.db'): return CL['dbpage']
    return CL['dbother']
def ext4_meta(dev, lab):
    out = lib.sh(['sudo', 'dumpe2fs', dev], check=False)
    bs = int(re.search(r'Block size:\s+(\d+)', out).group(1)); assert bs == 4096, bs
    def mark(a, b): lab[a:b + 1] = np.where(lab[a:b + 1] == 0, CL['meta'], lab[a:b + 1])
    for m in re.finditer(r'(?:Primary|Backup) superblock at (\d+), Group descriptors at (\d+)-(\d+)', out): mark(int(m.group(1)), int(m.group(3)))
    for m in re.finditer(r'Reserved GDT blocks at (\d+)-(\d+)', out): mark(int(m.group(1)), int(m.group(2)))
    for m in re.finditer(r'(?:Block|Inode) bitmap at (\d+)', out): mark(int(m.group(1)), int(m.group(1)))
    for m in re.finditer(r'Inode table at (\d+)-(\d+)', out): mark(int(m.group(1)), int(m.group(2)))
    j = lib.sh(['sudo', 'debugfs', '-R', 'stat <8>', dev], check=False); jl = 0
    for m in re.finditer(r'\((\d+)(?:-(\d+))?\):(\d+)(?:-(\d+))?', j.split('EXTENTS:')[-1]):
        a = int(m.group(3)); b = int(m.group(4) or a); lab[a:b + 1] = CL['journal']; jl += b - a + 1
    return {'fs': 'ext4', 'journal_blocks': jl}
def ext4_icheck(dev, lab, fid, inos, name):
    """1 % random sample of data/dir blocks cross-checked with debugfs icheck against the FIEMAP attribution."""
    import random
    idx = np.flatnonzero((lab == CL['data']) | (lab == CL['dir'])); r = random.Random(lib.seed_for('icheck', name))
    pick = sorted(r.sample(list(idx), max(1, len(idx) // 100))) if len(idx) else []
    agree = dis = nf = 0; cmd = os.path.join(lib.WORK, 'icheck-%s.cmd' % name)
    for i in range(0, len(pick), 400):
        part = pick[i:i + 400]; open(cmd, 'w').write('icheck ' + ' '.join(str(x) for x in part) + '\n')
        out = lib.sh(['sudo', 'debugfs', '-f', cmd, dev], check=False)
        for m in re.finditer(r'^(\d+)\t(\S.*)$', out, re.M):
            b = int(m.group(1)); v = m.group(2).strip()
            if not v.isdigit(): nf += 1; dis += 1
            elif int(v) == inos.get(int(fid[b]), -1): agree += 1
            else: dis += 1
    os.remove(cmd); return {'sampled': len(pick), 'agree': agree, 'disagree': dis, 'not_found': nf}
def xfs_meta(dev, lab):
    sb = lib.sh(['sudo', 'xfs_db', '-r', '-c', 'sb 0', '-c', 'print', dev]); g = lambda k: int(re.search(r'^%s = (\d+)' % k, sb, re.M).group(1))
    bs, agb, agc, lstart, lblocks, agl = g('blocksize'), g('agblocks'), g('agcount'), g('logstart'), g('logblocks'), g('agblklog'); assert bs == 4096
    for i in range(agc): lab[i * agb] = CL['meta']
    ag, off = lstart >> agl, lstart & ((1 << agl) - 1); a = ag * agb + off; lab[a:a + lblocks] = CL['journal']
    n = 0
    out = lib.sh(['sudo', 'xfs_db', '-r', '-c', 'blockget -v', dev], check=False)          # per-block usage types of the whole file system
    for m in re.finditer(r'^setting block (\d+)/(\d+) to (\S+)', out, re.M):
        if m.group(3) in ('data', 'dir', 'rldata', 'rtdata', 'unknown', 'free1', 'free2'): continue
        b = int(m.group(1)) * agb + int(m.group(2))
        if b < len(lab) and lab[b] == 0: lab[b] = CL['journal'] if m.group(3) == 'log' else CL['meta']; n += 1
    return {'fs': 'xfs', 'journal_blocks': lblocks, 'blockget_meta_blocks': n,
            'note': 'blockget -v types give meta; non-zero blocks it does not name stay unmapped (0)' if n else 'blockget produced nothing: remaining non-zero blocks are meta-or-unmapped (0)'}
def btrfs_chunks(dev):
    out = lib.sh(['sudo', 'btrfs', 'inspect-internal', 'dump-tree', '-t', 'chunk', dev], check=False); res = []
    for m in re.finditer(r'CHUNK_ITEM (\d+)\).*?\n\s+length (\d+) owner \d+ stripe_len \d+ type (\S+)\s*\n.*?\n\s+stripe 0 devid \d+ offset (\d+)', out, re.S):
        res.append((int(m.group(1)), int(m.group(2)), m.group(3), int(m.group(4))))
    return sorted(res)
def run(img, name, mode='fs', engine=None):
    size = os.path.getsize(img); nb = (size + 4095) // 4096
    lab = np.zeros(nb, np.uint8); fid = np.zeros(nb, np.uint32); files = []
    parts = lib.partitions(img) if lib.sh(['sfdisk', '-J', img], check=False).strip() else []
    spans = [(0, size, None)] if not parts else [(o, s, t) for o, s, t, n in parts]
    meta = {'name': name, 'partitions': [(o, s, t) for o, s, t in spans], 'unaligned_64k': [o % 65536 != 0 for o, s, t in spans]}
    if parts:
        lab[:] = CL['ptable']
        for o, s, t in spans: lab[o // 4096:(o + s) // 4096] = CL['otherpart']
    # root = the largest partition that holds a mountable fs
    root = max(spans, key=lambda x: x[1]); ro, rs = root[0], root[1]
    mnt = os.path.join(lib.MNT, 'lab-' + name)
    with lib.loopdev(img, ro or None, rs if parts else None, True) as d0:
        fs0 = lib.sh(['sudo', 'blkid', '-o', 'value', '-s', 'TYPE', d0]).strip()
    with lib.loopmount(img, mnt, offset=ro or None, size=rs if parts else None, ro=True, opts='ro,noatime,subvolid=5' if fs0 == 'btrfs' else None) as dev:
        fs = lib.sh(['sudo', 'blkid', '-o', 'value', '-s', 'TYPE', dev]).strip(); meta['fs'] = fs
        base = ro // 4096; fsb = rs // 4096
        sub = np.zeros(fsb, np.uint8); sfid = np.zeros(fsb, np.uint32)
        cmap = btrfs_chunks(dev) if fs == 'btrfs' else None
        def phys(p):   # byte address reported by FIEMAP -> byte offset in the device
            if cmap is None: return p
            for l, ln, t, off in cmap:
                if l <= p < l + ln: return off + (p - l)
            return None
        txt = lib.sh(['sudo', os.path.join(lib.ROOT, 'fiemap2'), mnt]); cur = None; nf = 0; nfail = 0; inos = {}
        with open(os.path.join(lib.WORK, 'files-%s.tsv' % name), 'w') as ft:
            for l in txt.split('\n'):
                if l.startswith('P\t'):
                    _, i, k, sz, ino, p = l.split('\t', 5); rel = p[len(mnt):]; cur = (int(i) + 1, k, rel); ft.write('%d\t%s\t%s\t%s\t%s\n' % (int(i) + 1, k, sz, ino, rel)); inos[int(i) + 1] = int(ino); nf += 1
                elif l.startswith('E\t') and cur:
                    _, i, ph, ln, fl = l.split('\t'); q = phys(int(ph))
                    if q is None: nfail += 1; continue
                    a, b = q // 4096, (q + int(ln) + 4095) // 4096
                    if b > fsb: continue
                    if mode == 'db': c = path_class_db(cur[2], engine) if cur[1] == 'F' else CL['dbother']
                    else: c = CL['data'] if cur[1] == 'F' else CL['dir']
                    sub[a:b] = c; sfid[a:b] = cur[0]
        meta['files'] = nf; meta['fiemap_translation_failures'] = nfail
        if fs == 'ext4':
            meta.update(ext4_meta(dev, sub)); meta['icheck'] = ext4_icheck(dev, sub, sfid, inos, name)
        elif fs == 'xfs': meta.update(xfs_meta(dev, sub))
        elif fs == 'btrfs':
            sub[:256] = np.where(sub[:256] == 0, CL['meta'], sub[:256])      # first 1 MiB: superblock copy, reserved
            for l, ln, t, off in cmap:
                a, b = off // 4096, (off + ln) // 4096
                if 'DATA' in t.split('|'): sub[a:b] = np.where(sub[a:b] == 0, CL['data'], sub[a:b])
                else: sub[a:b] = np.where(sub[a:b] == 0, CL['meta'], sub[a:b])
            meta['btrfs_chunks'] = len(cmap)
    lab[base:base + fsb] = sub; fid[base:base + fsb] = sfid
    lab.tofile(os.path.join(lib.WORK, 'labels-%s.u8' % name)); fid.tofile(os.path.join(lib.WORK, 'labels-%s.fid' % name))
    meta['blocks_by_class'] = {k: int((lab == v).sum()) for k, v in CL.items()}
    json.dump(meta, open(os.path.join(lib.OUT, 'labels-%s.json' % name), 'w'), indent=1); print(json.dumps(meta['blocks_by_class']), meta.get('fs'))
if __name__ == '__main__':
    a = sys.argv[1:]; run(a[0], a[1], 'db' if len(a) > 2 and a[2] == 'db' else 'fs', a[3] if len(a) > 3 else None)
