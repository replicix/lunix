#!/usr/bin/env python3
"""Corpus driver.  usage:
  corpus.py prep <name> <image> [db <engine>]   chunk at 16 and 64 KiB, labels, chunk classes, plain+probe pass (z1, z3)
  corpus.py measure <name> <cs> <tag> <levels> <dictname=path>...   run zmeasure with dictionaries; result work/res/<name>-<cs>.<tag>.bin(.cols)"""
import sys, os, subprocess, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
RES = os.path.join(lib.WORK, 'res'); os.makedirs(RES, exist_ok=True)
def py(*a): subprocess.run(['python3', '-I'] + [str(x) for x in a], check=True)
def measure(name, cs, tag, levels, dicts, threads=2, flags=()):
    base = os.path.join(lib.WORK, '%s-%d' % (name, cs)); img = open(base + '.img').read().strip(); fl = base + '.files'; open(fl, 'w').write(img + '\n')
    cmd = ['taskset', '-c', os.environ.get('DICTVAL_CPUS', '14-15'), os.path.join(lib.ROOT, 'zmeasure2'), *flags, '-r', base + '.recs', '-f', fl, '-o', os.path.join(RES, '%s-%d.%s.bin' % (name, cs, tag)), '-L', levels, '-t', str(threads)]
    for d in dicts: cmd += ['-D', d]
    subprocess.run(cmd, check=True, capture_output=True)
if __name__ == '__main__':
    a = sys.argv[1:]
    if a[0] == 'prep':
        name, img = a[1], a[2]; code = os.path.dirname(os.path.abspath(__file__))
        for cs in (16384, 65536): py(os.path.join(code, 'chunk.py'), img, cs, lib.WORK, name)
        py(os.path.join(code, 'label.py'), img, name, *(a[3:]))
        for cs in (16384, 65536): py(os.path.join(code, 'chunklabels.py'), img, name, cs); measure(name, cs, 'plain', '1,3', [])
    elif a[0] == 'post':      # labels, chunk classes and the plain pass for an already chunked corpus
        name, img = a[1], a[2]; code = os.path.join(lib.ROOT, 'code')
        py(os.path.join(code, 'label.py'), img, name, *(a[3:]))
        for cs in (16384, 65536): py(os.path.join(code, 'chunklabels.py'), img, name, cs); measure(name, cs, 'plain', '1,3', [])
    elif a[0] == 'measure':
        measure(a[1], int(a[2]), a[3], a[4], a[5:])
