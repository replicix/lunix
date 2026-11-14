#!/usr/bin/env python3
"""Quiet-host CPU timing driver (protocol "Timing").  Runs on the timing host from ~/dictval-bench, resumable, one pinned core.
  timing_run.py <cell> [<cell> ...]     cells: vm-ubuntu-16384 vm-ubuntu-65536 db-se-askubuntu-pg-16384 db-se-askubuntu-pg-4m db-se-askubuntu-pg-1m
                                        vm-ubuntu-files vm-ubuntu-files-lt64k  (and the tlc cells when their dictionaries are present)
Appends rows to out/timing.tsv (one row per cell, arm, level, op); a row already present is not measured again.
Tools: ztime2 (C, libzstd + liblz4; 1 warm-up + 7 timed passes, in-memory sample) and zdtool seltime (Rust, zdicts v0.0.1 selector, same
passes).  The compress cost of an `ft` arm is ztime2 (probe if any, dictionary compression with the selection read from a file) plus the
selector pass over the chunks the probe let through (median + median, min + min, max + max).  Decompress never includes the selector."""
import os, sys, subprocess, time, json
B = os.path.expanduser('~/dictval-bench'); OUT = os.path.join(B, 'out'); TSV = os.path.join(OUT, 'timing.tsv'); CPU = '5'; PASSES = 7
HDR = ['cell', 'corpus', 'view', 'chunk', 'arm', 'level', 'op', 'median', 'min', 'max', 'flag', 'load1', 'others_busy_cores', 'sample_bytes', 'objects', 'stored_bytes', 'raw_objects', 'note']
D = lambda n: os.path.join(B, 'dicts', n)
FT = [('ft%02d' % i, D('%d.dict' % i)) for i in range(1, 34)]
def cells():
    c = {}
    for cs in (16384, 65536):
        n = 'vm-ubuntu-%d' % cs
        c[n] = dict(corpus='vm-ubuntu', view='lunix', chunk=cs, dir=n, recs='recs', probe=True, sel='lunix',
                    note='stand-in for host-ext4/host-xfs/host-btrfs (same-size dictionaries trained on this image)',
                    sets={'bs-dist': ('cls', [D('bs-dist-deb-meta.dict'), D('bs-dist-deb-text.dict'), D('bs-dist-deb-bin.dict')]),
                          'bs-xfs': ('cls', [D('bs-xfs-deb-ext4-meta.dict'), D('bs-xfs-deb-ext4-text.dict'), D('bs-xfs-deb-ext4-bin.dict')]),
                          'pv': ('one', [D('pv-vm-ubuntu-%d-s1-all.dict' % cs)]),
                          'pv-class': ('cls', [None, D('pv-vm-ubuntu-%d-s1-text.dict' % cs), D('pv-vm-ubuntu-%d-s1-bin.dict' % cs)])},
                    levels=(1, 3))
    for ds, fam in (('se-askubuntu', 'se'), ('tlc-2024q1', 'tlc')):
        n = 'db-%s-pg' % ds; dbd = D('dbd-pg-%s-16384.dict' % fam)
        c[n + '-16384'] = dict(corpus=n, view='lunix-dbpage', chunk=16384, dir=n, recs='recs16', probe=True, sel='lunix', note='',
                               sets={'db': ('one', [dbd]), 'pv': ('one', [D('pv-%s-16384-s1-all.dict' % n)])}, levels=(1, 3))
        for tag, cs in (('4m', 4 << 20), ('1m', 1 << 20)):
            c['%s-%s' % (n, tag)] = dict(corpus=n, view='constellation-files', chunk=cs, dir=n, recs='recs' + tag, probe=False, sel=None, note='',
                                         sets={'db': ('one', [dbd]), 'pv': ('one', [D('cvpv-%s-%s-s1.dict' % (n, tag))])}, levels=(3,))
    c['vm-ubuntu-files'] = dict(corpus='vm-ubuntu', view='constellation-files', chunk=4 << 20, dir='vm-ubuntu-files', recs='recs', probe=False, sel='obj', names='names.txt',
                                note='file objects of the image (4 MiB chunking), stand-in for the OS-tree file population', sets={}, levels=(3,))
    c['vm-ubuntu-files-lt64k'] = dict(c['vm-ubuntu-files'], dir='vm-ubuntu-files-lt64k', note='objects under 64 KiB only (subset of the file objects)')
    return c
def others_busy(sec=2.0):
    def snap():
        r = {}
        for l in open('/proc/stat'):
            if l.startswith('cpu') and l[3].isdigit():
                f = l.split(); r[int(f[0][3:])] = (sum(int(x) for x in f[1:9]), int(f[4]) + int(f[5]))
        return r
    a = snap(); time.sleep(sec); b = snap(); busy = 0.0
    for k in a:
        if k == int(CPU): continue
        t = b[k][0] - a[k][0]; i = b[k][1] - a[k][1]; busy += (t - i) / (100.0 * sec)
    return busy
def have():
    s = set()
    if os.path.exists(TSV):
        for l in open(TSV).read().splitlines()[1:]:
            f = l.split('\t'); s.add((f[0], f[4], f[5], f[6]))
    return s
def put(row):
    new = not os.path.exists(TSV)
    with open(TSV, 'a') as f:
        if new: f.write('\t'.join(HDR) + '\n')
        f.write('\t'.join(str(x) for x in row) + '\n')
def flag(mn, mx): return 'unstable' if mn > 0 and mx / mn > 1.2 else ''
def prep_lt64k(c):
    import numpy as np
    src = os.path.join(B, 'tm', 'vm-ubuntu-files'); dst = os.path.join(B, 'tm', c['dir']); os.makedirs(dst, exist_ok=True)
    if os.path.exists(os.path.join(dst, 'recs')): return
    rec = np.fromfile(os.path.join(src, 'recs'), dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])); names = open(os.path.join(src, 'names.txt')).read().split('\n')[:len(rec)]
    keep = np.nonzero(rec['len'] < 65536)[0]; rec[keep].tofile(os.path.join(dst, 'recs')); open(os.path.join(dst, 'names.txt'), 'w').write('\n'.join(names[k] for k in keep) + '\n')
    if not os.path.exists(os.path.join(dst, 'sample.bin')): os.symlink(os.path.join(src, 'sample.bin'), os.path.join(dst, 'sample.bin'))
def cell(name):
    c = cells()[name]; d = os.path.join(B, 'tm', c['dir'])
    if name.endswith('lt64k'): prep_lt64k(c)
    recs, samp = os.path.join(d, c['recs']), os.path.join(d, 'sample.bin'); done = have()
    def row(arm, level, op, med, mn, mx, load, busy, total, nobj, stored, nraw, note=''):
        put([name, c['corpus'], c['view'], c['chunk'], arm, level, op, '%.4f' % med, '%.4f' % mn, '%.4f' % mx, flag(mn, mx), load, '%.2f' % busy, total, nobj, stored, nraw, (c['note'] + ' ' + note).strip()])
    def zt(arm, level, probe=False, sets=None, mode=None, sel=None, selres=None, extra=()):
        if (name, arm, str(level), 'compress') in done and (name, arm, str(level), 'decompress') in done and not extra: return
        cmd = ['taskset', '-c', CPU, os.path.join(B, 'ztime2'), '-r', recs, '-s', samp, '-L', str(level), '-n', str(PASSES)]
        if probe: cmd.append('-P')
        if mode == 'cls': cmd.append('-C')
        if sel: cmd += ['-S', sel]
        for i, p in enumerate(sets or []): cmd += ['-D', 'x%d=%s' % (i, p or '-')]
        cmd += list(extra); busy = others_busy()
        out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.split('\t'); lv, total, c1, c2, c3, d1, d2, d3, load, stored, nraw, nobj = out
        c1, c2, c3, d1, d2, d3 = map(float, (c1, c2, c3, d1, d2, d3)); note = ''
        if selres: c1, c2, c3 = c1 + selres[0], c2 + selres[1], c3 + selres[2]; note = 'compress includes the selector pass (separate row ft-select)'
        if (name, arm, str(level), 'compress') not in done: row(arm, level, 'compress', c1, c2, c3, load.strip(), busy, total, nobj.strip(), stored, nraw, note)
        if (name, arm, str(level), 'decompress') not in done: row(arm, level, 'decompress', d1, d2, d3, load.strip(), busy, total, nobj.strip(), stored, nraw)
        print(name, arm, level, 'c %.3f (%.3f-%.3f) d %.3f s/GB, stored %s, raw %s' % (c1, c2, c3, d1, stored, nraw.strip()), flush=True)
    def seltime(mask):
        outsel = os.path.join(d, 'ft.sel.u8'); key = (name, 'ft-select', '0', 'compress')
        cmd = ['taskset', '-c', CPU, os.path.join(B, 'zdtool', 'target', 'release', 'zdtool'), 'seltime', 'obj' if c['sel'] == 'obj' else 'lunix', recs, samp, str(PASSES), mask or '-', os.path.join(d, c['names']) if c['sel'] == 'obj' else '-', outsel]
        busy = others_busy(); m, a, b_, total = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.split(); m, a, b_ = float(m), float(a), float(b_)
        if key not in done: row('ft-select', 0, 'compress', m, a, b_, open('/proc/loadavg').read().split()[0], busy, total, '', '', '', 'selector only: ' + ('select(name, head, len)' if c['sel'] == 'obj' else 'select_by_content on the first 4 KiB of chunks that passed the probe'))
        print(name, 'ft-select', 'c %.3f (%.3f-%.3f) s/GB' % (m, a, b_), flush=True); return outsel, (m, a, b_)
    pq = os.path.join(d, 'probe.raw.u8')
    for L in range(1, 10):                              # plain levels: Lunix behind the probe, Constellation as is
        zt('z%d' % L, L, probe=c['probe'], extra=(['-Q', pq] if c['probe'] and L == 1 and not os.path.exists(pq) else []))
    if c['probe']:
        for L in (1, 3): zt('z%doff' % L, L)               # probe off, for the forfeited bytes
    if c['sel']:
        outsel, st = seltime(pq if c['probe'] else None)
        for L in c['levels']: zt('ft', L, probe=c['probe'], sets=[p for _, p in FT], sel=outsel, selres=st)
    for arm, (mode, ds) in c['sets'].items():
        for L in c['levels']: zt(arm, L, probe=c['probe'], sets=ds, mode='cls' if mode == 'cls' else None)
if __name__ == '__main__':
    os.makedirs(OUT, exist_ok=True)
    for n in sys.argv[1:]: cell(n)
    print('done', flush=True)
