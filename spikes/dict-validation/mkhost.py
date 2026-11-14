#!/usr/bin/env python3
"""Phase 2a: copy the test host's own OS tree into a fresh file system image (section 2.2).  usage: mkhost.py ext4|xfs|btrfs
Image work/host-<fs>.raw (32G sparse).  Hostname, addresses, MACs and login names (uid >= 1000, except the generic distribution
default login, see SUMMARY deviations) are searched in etc and var of the copy; hits are deleted and only their number is recorded."""
import sys, os, re, json, socket, subprocess, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
fs = sys.argv[1]; n = {'ext4': 1, 'xfs': 2, 'btrfs': 3}[fs]; uuid = '00000000-0000-4000-8000-00000000000%d' % n
img = os.path.join(lib.WORK, 'host-%s.raw' % fs); mnt = os.path.join(lib.MNT, 'host-' + fs)
lib.guard(24, 'host-' + fs)
sh = lambda c: subprocess.run(c, check=True, capture_output=True, text=True)
sh(['rm', '-f', img]); sh(['truncate', '-s', '32G', img])
mk = {'ext4': ['mkfs.ext4', '-q', '-F', '-b', '4096', '-U', uuid, '-E', 'lazy_itable_init=0,lazy_journal_init=0', img],
      'xfs': ['mkfs.xfs', '-q', '-f', '-b', 'size=4096', '-m', 'uuid=' + uuid, img],
      'btrfs': ['mkfs.btrfs', '-q', '-f', '-d', 'single', '-m', 'single', '-n', '16384', '-U', uuid, img]}[fs]
sh(mk)
ident = set()
ident.add(socket.gethostname()); ident.add(socket.gethostname().split('.')[0])
ip = subprocess.run(['ip', '-o', 'addr'], capture_output=True, text=True).stdout
ident |= set(re.findall(r'inet6? ([0-9a-f:.]+)/', ip)) - {'127.0.0.1', '::1'}
ident |= set(re.findall(r'link/ether ([0-9a-f:]{17})', ip)) | {m.upper() for m in re.findall(r'link/ether ([0-9a-f:]{17})', ip)}
generic = {'ubuntu', 'nobody'}
for l in open('/etc/passwd'):
    p = l.split(':')
    if int(p[2]) >= 1000 and p[0] not in generic: ident.add(p[0])
ident = {x for x in ident if len(x) >= 4}
pf = os.path.join(lib.WORK, 'ident.%s.txt' % fs); open(pf, 'w').write('\n'.join(sorted(ident)) + '\n')
deleted = 0
with lib.loopmount(img, mnt, ro=False, opts='noatime,discard', direct=True) as dev:
    r = subprocess.run(['sudo', 'rsync', '-aHAX', '--numeric-ids', '--one-file-system', '--exclude-from=' + os.path.join(lib.ROOT, 'code', 'exclude.txt'), '/', mnt + '/'], capture_output=True, text=True)
    if r.returncode not in (0, 23, 24): raise SystemExit('rsync failed %d %s' % (r.returncode, r.stderr[-300:]))
    used = os.statvfs(mnt); used_gib = (used.f_blocks - used.f_bfree) * used.f_frsize / 2**30
    if used_gib > 24: raise SystemExit('size cap 24 GiB hit: %.1f' % used_gib)
    for sub in ('etc', 'var'):
        g = subprocess.run(['sudo', 'grep', '-rIlF', '-f', pf, os.path.join(mnt, sub)], capture_output=True, text=True)
        for f in [x for x in g.stdout.split('\n') if x]:
            subprocess.run(['sudo', 'rm', '-f', f]); deleted += 1
    subprocess.run(['sudo', 'sync', '-f', mnt], check=True)       # syncfs of this file system only, never a bare sync
    t = subprocess.run(['sudo', 'fstrim', '-v', mnt], capture_output=True, text=True).stdout.strip()
    sh(['sudo', 'chmod', 'a+rx', mnt])
os.remove(pf); sh(['fallocate', '-d', img])
json.dump({'fs': fs, 'copied_gib': round(used_gib, 2), 'identity_files_deleted': deleted, 'identity_patterns': len(ident), 'rsync_rc': r.returncode,
           'fstrim': re.sub(r'^\S+', 'mnt', t)}, open(os.path.join(lib.OUT, 'host-%s.json' % fs), 'w'))
print(fs, used_gib, deleted, r.returncode)
