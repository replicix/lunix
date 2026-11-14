#!/usr/bin/env python3
"""Follow-up M3: per-image timing of the Lunix volume-wide view (Amendment A1) on the quiet host, one pinned core, 1 warm-up + 7 timed passes (ztime2).
usage (from ~/dictval-bench):  timing_db.py <image> <chunk> <sample-dir>      image e.g. db-se-askubuntu-pg, chunk 16384 | 65536
<sample-dir> holds sample.bin (selected 1 MiB blocks, tmblocks.py), recs<cs> (the image's unique non-hole chunks of those blocks, ALL classes), pv<cs>.dict (pv, seed 1, fastCover)
and pvcover<cs>.dict (pv-cover, COVER).  Arms (all behind the ADR-12 probe): z1 z2 z3 plain, pv@1 pv@3, pv-cover@1 pv-cover@3; compress and decompress each.
Appends rows to out/timing-db.tsv (columns of raw/timing.csv with `cell` first); a (cell, arm, level, op) already present is not measured again."""
import os, sys, subprocess, time
B = os.path.expanduser('~/dictval-bench'); OUT = os.path.join(B, 'out'); TSV = os.path.join(OUT, 'timing-db.tsv'); CPU = '5'; PASSES = 7
HDR = ['cell', 'corpus', 'view', 'chunk', 'arm', 'level', 'op', 'median', 'min', 'max', 'flag', 'load1', 'others_busy_cores', 'sample_bytes', 'objects', 'stored_bytes', 'raw_objects', 'note']
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
        busy += ((b[k][0] - a[k][0]) - (b[k][1] - a[k][1])) / (100.0 * sec)
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
def main(image, cs, d):
    os.makedirs(OUT, exist_ok=True); cell = '%s-%d' % (image, cs); done = have(); recs, samp = os.path.join(d, 'recs%d' % cs), os.path.join(d, 'sample.bin')
    note = 'volume-wide block-stratified sample, all classes'
    def zt(arm, level, dicts=()):
        if all((cell, arm, str(level), op) in done for op in ('compress', 'decompress')): return
        cmd = ['taskset', '-c', CPU, os.path.join(B, 'ztime2'), '-r', recs, '-s', samp, '-L', str(level), '-n', str(PASSES), '-P']
        for i, p in enumerate(dicts): cmd += ['-D', 'x%d=%s' % (i, p)]
        busy = others_busy(); out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout.split('\t'); lv, total, c1, c2, c3, d1, d2, d3, load, stored, nraw, nobj = out
        for op, (m, a, b) in (('compress', (c1, c2, c3)), ('decompress', (d1, d2, d3))):
            m, a, b = float(m), float(a), float(b); put([cell, image, 'lunix-volume', cs, arm, level, op, '%.4f' % m, '%.4f' % a, '%.4f' % b, 'unstable' if a > 0 and b / a > 1.2 else '', load.strip(), '%.2f' % busy, total, nobj.strip(), stored, nraw, note])
        print(cell, arm, level, 'c %.3f (%.3f-%.3f) d %.3f s/GB stored %s raw %s' % (float(c1), float(c2), float(c3), float(d1), stored, nraw.strip()), flush=True)
    for L in (1, 2, 3): zt('z%d' % L, L)
    for L in (1, 3): zt('pv', L, [os.path.join(d, 'pv%d.dict' % cs)])
    for L in (1, 3): zt('pv-cover', L, [os.path.join(d, 'pvcover%d.dict' % cs)])
if __name__ == '__main__': main(sys.argv[1], int(sys.argv[2]), sys.argv[3])
