"""Shared helpers: tracked loop devices and mounts (always cleaned up), free-space guard, seeds, paths."""
import os, sys, subprocess, json, atexit, signal, contextlib, shutil, hashlib, random
ROOT = os.environ.get('DICTVAL', os.path.expanduser('~/dictval'))
WORK, OUT, DL, MNT = [os.path.join(ROOT, d) for d in ('work', 'out', 'dl', 'mnt')]
TRACK = os.environ.get('DICTVAL_TRACK') or os.path.join(ROOT, 'work', 'tracked.txt')   # queue B sets its own file so cleanup never touches another queue's mounts
MASTER_SEED = 20261112
def seed_for(*parts):
    h = hashlib.sha256((str(MASTER_SEED) + '|' + '|'.join(map(str, parts))).encode()).digest()
    return int.from_bytes(h[:8], 'little')
def rng_for(*parts): return random.Random(seed_for(*parts))
def sh(cmd, check=True, **kw):
    r = subprocess.run(cmd, shell=isinstance(cmd, str), capture_output=True, text=True, **kw)
    if check and r.returncode: raise RuntimeError('%s -> %d: %s' % (cmd, r.returncode, r.stderr[-800:]))
    return r.stdout
def free_gib(path=None): st = os.statvfs(path or ROOT); return st.f_bavail * st.f_frsize / 2**30
def guard(need_gib, what=''):
    """Every mkfs, download and export is preceded by a check that 40 GiB stay free after the step."""
    f = free_gib()
    if f - need_gib < 40: raise SystemExit('free-space guard: %.1f GiB free, need %.1f + 40 for %s' % (f, need_gib, what))
def _track(kind, what): open(TRACK, 'a').write('%s %s\n' % (kind, what))
def cleanup():
    """Unmount and detach everything this study created (reverse order)."""
    if not os.path.exists(TRACK): return
    lines = open(TRACK).read().split('\n')[::-1]
    for l in lines:
        if not l.strip(): continue
        k, w = l.split(' ', 1)
        if k == 'mount': subprocess.run(['sudo', 'umount', w], capture_output=True)
        elif k == 'loop': subprocess.run(['sudo', 'losetup', '-d', w], capture_output=True)
    os.remove(TRACK)
atexit.register(cleanup)
for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP): signal.signal(s, lambda *a: sys.exit(130))
def losetup(img, offset=None, size=None, ro=False, direct=False):
    cmd = ['sudo', 'losetup', '--find', '--show']
    if ro: cmd.append('-r')
    if offset: cmd += ['-o', str(offset)]
    if size: cmd += ['--sizelimit', str(size)]
    if direct: cmd += ['--direct-io=on', '--sector-size', '512']
    dev = subprocess.run(cmd + [img], capture_output=True, text=True, check=True).stdout.strip()
    _track('loop', dev); return dev
def untrack(kind, what):
    if os.path.exists(TRACK):
        ls = [l for l in open(TRACK).read().split('\n') if l and l != '%s %s' % (kind, what)]
        open(TRACK, 'w').write('\n'.join(ls) + ('\n' if ls else ''))
def mount(dev, mnt, opts):
    os.makedirs(mnt, exist_ok=True)
    subprocess.run(['sudo', 'mount', '-o', opts, dev, mnt], check=True, capture_output=True); _track('mount', mnt)
def umount(mnt): subprocess.run(['sudo', 'umount', mnt], check=True, capture_output=True); untrack('mount', mnt)
def detach(dev): subprocess.run(['sudo', 'losetup', '-d', dev], check=True, capture_output=True); untrack('loop', dev)
@contextlib.contextmanager
def loopmount(img, mnt, offset=None, size=None, ro=True, opts=None, direct=False):
    dev = losetup(img, offset, size, ro, direct)
    try:
        mount(dev, mnt, opts or ('ro,noatime' if ro else 'noatime,discard'))
        try: yield dev
        finally: umount(mnt)
    finally: detach(dev)
@contextlib.contextmanager
def loopdev(img, offset=None, size=None, ro=True):
    dev = losetup(img, offset, size, ro)
    try: yield dev
    finally: detach(dev)
def partitions(img):
    t = json.loads(sh(['sfdisk', '-J', img]))['partitiontable']
    return [(p['start'] * 512, p['size'] * 512, p.get('type', ''), p.get('name', '')) for p in t.get('partitions', [])]
