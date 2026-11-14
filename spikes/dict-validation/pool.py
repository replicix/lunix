#!/usr/bin/env python3
"""Phase 4a: block-set training pool.  usage: pool.py <distro> (alpine|rocky|suse|arch)
Converts the pool image, builds 4 views (native, ext4, xfs, btrfs copies of its tree), chunks each at 16 KiB, and keeps up to
4 MiB of random unique chunks per cheap class (1 meta, 2 text, 3 binary) per view in work/pool/<distro>-<view>-<class>.bin.
Images are deleted after sampling (sample only, as the budget requires)."""
import sys, os, json, struct, subprocess, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
distro = sys.argv[1]; FAM = {'alpine': 'apk', 'rocky': 'rhel', 'suse': 'suse', 'arch': 'arch'}[distro]
src = {'alpine': 'alpine-3.24.4-x86_64-cloudinit-r0.qcow2', 'rocky': 'Rocky-10-GenericCloud-Base-10.2-20260525.0.x86_64.qcow2',
       'suse': 'Leap-16.1-Minimal-VM.x86_64-Cloud.qcow2', 'arch': 'Arch-Linux-x86_64-cloudimg-20261101.620452.qcow2'}[distro]
POOL = os.path.join(lib.WORK, 'pool'); os.makedirs(POOL, exist_ok=True)
raw = os.path.join(lib.WORK, 'pool-%s.raw' % distro); CS = 16384; PER_CLASS = 4 << 20
lib.guard(14, 'pool ' + distro)
subprocess.run(['qemu-img', 'convert', '-O', 'raw', '-S', '4k', os.path.join(lib.DL, src), raw], check=True)
sh = lambda c, **k: subprocess.run(c, check=True, capture_output=True, text=True, **k)
def sample(img, view, fs):
    nm = 'pool-%s-%s' % (distro, view)
    sh(['python3', os.path.join(lib.ROOT, 'code', 'chunk.py'), img, str(CS), lib.WORK, nm])
    recs = os.path.join(lib.WORK, '%s-%d.recs' % (nm, CS)); of = os.path.join(lib.WORK, nm + '.cols.bin')
    open(os.path.join(lib.WORK, nm + '.files'), 'w').write(img + '\n')
    sh([os.path.join(lib.ROOT, 'zmeasure2'), '-r', recs, '-f', os.path.join(lib.WORK, nm + '.files'), '-o', of, '-L', '1', '-t', '2'])
    m = np.fromfile(of, dtype=np.uint32).reshape(-1, 3); r = np.fromfile(recs, dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]))
    rng = lib.rng_for('pool', distro, view); info = {'distro': distro, 'family': FAM, 'view': view, 'fs': fs, 'unique_chunks': int(len(r)), 'classes': {}}
    full = r['len'] == CS
    with open(img, 'rb') as f:
        for c in (1, 2, 3):
            idx = np.flatnonzero((m[:, 1] == c) & full); info['classes'][c] = int(len(idx))
            take = rng.sample(list(idx), min(len(idx), PER_CLASS // CS))
            with open(os.path.join(POOL, '%s-%s-%d.bin' % (distro, view, c)), 'wb') as o:
                for i in sorted(take): f.seek(int(r['off'][i])); o.write(f.read(CS))
            info['classes'][c] = [int(len(idx)), len(take)]
    json.dump(info, open(os.path.join(POOL, '%s-%s.json' % (distro, view)), 'w'))
    os.replace(os.path.join(lib.WORK, nm + '-%d.h16' % CS), os.path.join(POOL, nm.replace('pool-', '') + '.h16'))   # kept: cross-image duplicate share
    for e in ('.recs', '.h16', '.shares.json', '.cols.bin', '.files'):
        p = os.path.join(lib.WORK, nm + e) if e != '.recs' else recs
        if os.path.exists(p): os.remove(p)
    return info
parts = lib.partitions(raw); root = None
for o, s, t, n in sorted(parts, key=lambda x: -x[1]):
    with lib.loopdev(raw, o, s, True) as dev:
        fs = sh(['sudo', 'blkid', '-o', 'value', '-s', 'TYPE', dev]).stdout.strip() if subprocess.run(['sudo', 'blkid', dev], capture_output=True).returncode == 0 else ''
    if fs in ('ext4', 'xfs', 'btrfs'): root = (o, s, fs); break
print(distro, 'root', root, flush=True)
if root is None: raise SystemExit('no root fs')
o, s, nfs = root
print(sample(raw, 'native', nfs), flush=True)
src_m = os.path.join(lib.MNT, 'poolsrc-' + distro)
with lib.loopmount(raw, src_m, offset=o, size=s, ro=True) as sd:
    for fs in ('ext4', 'xfs', 'btrfs'):
        img = os.path.join(lib.WORK, 'pool-%s-%s.raw' % (distro, fs)); dm = os.path.join(lib.MNT, 'poolcopy-%s-%s' % (distro, fs))
        lib.guard(12, 'pool copy'); sh(['rm', '-f', img]); sh(['truncate', '-s', '12G', img])
        mk = {'ext4': ['mkfs.ext4', '-q', '-F', '-b', '4096', '-E', 'lazy_itable_init=0,lazy_journal_init=0', img], 'xfs': ['mkfs.xfs', '-q', '-f', '-b', 'size=4096', img],
              'btrfs': ['mkfs.btrfs', '-q', '-f', '-d', 'single', '-m', 'single', '-n', '16384', img]}[fs]
        sh(mk)
        with lib.loopmount(img, dm, ro=False, opts='noatime,discard', direct=True) as dd:
            r = subprocess.run(['sudo', 'rsync', '-aHAX', '--numeric-ids', '--one-file-system', src_m + '/', dm + '/'], capture_output=True, text=True)
            su, du = os.statvfs(src_m), os.statvfs(dm); sused = (su.f_blocks - su.f_bfree) * su.f_frsize; dused = (du.f_blocks - du.f_bfree) * du.f_frsize
            print('rsync', fs, 'rc', r.returncode, 'src_used', sused >> 20, 'MiB dst_used', dused >> 20, 'MiB', r.stderr[-300:].replace('\n', ' | '), flush=True)
            if r.returncode not in (0, 23, 24) or dused < 0.5 * sused: raise SystemExit('pool copy to %s incomplete' % fs)
            subprocess.run(['sudo', 'sync', '-f', dm], check=True); subprocess.run(['sudo', 'fstrim', dm], capture_output=True)
        sh(['fallocate', '-d', img]); print(sample(img, fs, fs), flush=True); os.remove(img)
os.remove(raw)
