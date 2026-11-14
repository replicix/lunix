#!/usr/bin/env python3
"""Phase 3b: map 4 KiB block labels to chunks (majority of non-zero blocks; 255 = mixed if no class reaches 75 %).
usage: chunklabels.py <image> <name> <csize> [labelname]   -> work/<name>-<csize>.cls (u8 per record; 0 = unmapped)"""
import sys, os, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
img, name, cs = sys.argv[1], sys.argv[2], int(sys.argv[3]); ln = sys.argv[4] if len(sys.argv) > 4 else name
rec = np.fromfile(os.path.join(lib.WORK, '%s-%d.recs' % (name, cs)), dtype=np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]))
lab = np.fromfile(os.path.join(lib.WORK, 'labels-%s.u8' % ln), dtype=np.uint8)
mm = np.memmap(img, dtype=np.uint8, mode='r'); out = np.zeros(len(rec), np.uint8)
for i, r in enumerate(rec):
    o, n = int(r['off']), int(r['len']); nb = n // 4096
    if nb == 0: out[i] = 255; continue
    blk = np.asarray(mm[o:o + nb * 4096]).view(np.uint64).reshape(nb, 512).any(axis=1)
    L = lab[o // 4096:o // 4096 + nb][blk]
    if not len(L): out[i] = 255; continue
    c = np.bincount(L, minlength=11); k = int(c.argmax())
    out[i] = k if c[k] * 4 >= len(L) * 3 else 255
out.tofile(os.path.join(lib.WORK, '%s-%d.cls' % (name, cs)))
print(name, cs, {int(k): int(v) for k, v in zip(*np.unique(out, return_counts=True))})
