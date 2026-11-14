#!/usr/bin/env python3
"""Follow-up M3 sampler (test host).  usage: tmblocks.py <image-name> <outdir>      e.g. db-se-askubuntu-pg
One block-stratified draw per image serves both chunk sizes (halves the relay volume): 1 MiB origin blocks are drawn systematically (equal probability, ordered by
block, weights = bytes of unique non-hole 16 KiB chunks, bisection to about 1 GiB, as tmsample.py) from the 16 KiB population; sample.bin = the selected blocks
concatenated (full 1 MiB each, zero parts and duplicate chunks included, never used as records); recs16384 = unique non-hole 16 KiB chunks of those blocks, recs65536 = unique
non-hole 64 KiB chunks whose first occurrence lies in those blocks (a 64 KiB chunk never straddles a block); records {u32 0, u32 len, u64 off into sample.bin}."""
import sys, os, numpy as np
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
from tmsample import pick, WORK
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')])
name, out = sys.argv[1], sys.argv[2]; os.makedirs(out, exist_ok=True)
img = open(os.path.join(WORK, '%s-16384.img' % name)).read().strip(); assert img == open(os.path.join(WORK, '%s-65536.img' % name)).read().strip()
r16 = np.fromfile(os.path.join(WORK, '%s-16384.recs' % name), dtype=REC); b16 = (r16['off'] >> 20).astype(np.int64)
ub, inv = np.unique(b16, return_inverse=True); bsz = np.bincount(inv, weights=r16['len']).astype(np.int64); sel = ub[pick(bsz)]
res = {}
for cs, r in ((16384, r16), (65536, np.fromfile(os.path.join(WORK, '%s-65536.recs' % name), dtype=REC))):
    b = (r['off'] >> 20).astype(np.int64); keep = np.isin(b, sel); k = r[keep].copy(); k['id'] = 0
    k['off'] = (np.searchsorted(sel, b[keep]).astype(np.uint64) << 20) + (r['off'][keep] & 0xFFFFF); k.tofile(os.path.join(out, 'recs%d' % cs)); res[cs] = (len(k), int(k['len'].sum()))
with open(img, 'rb') as f, open(os.path.join(out, 'sample.bin'), 'wb') as w:
    for b in sel:
        f.seek(int(b) << 20); d = f.read(1 << 20); w.write(d.ljust(1 << 20, b'\0'))
# check: three records per size against the image
with open(img, 'rb') as f, open(os.path.join(out, 'sample.bin'), 'rb') as s:
    for cs in (16384, 65536):
        k = np.fromfile(os.path.join(out, 'recs%d' % cs), dtype=REC); r = np.fromfile(os.path.join(WORK, '%s-%d.recs' % (name, cs)), dtype=REC); rb = r[np.isin(r['off'] >> 20, sel)]
        assert len(rb) == len(k)
        for i in (0, len(k) // 2, len(k) - 1):
            s.seek(int(k['off'][i])); f.seek(int(rb['off'][i])); assert s.read(int(k['len'][i])) == f.read(int(rb['len'][i])), 'record mismatch'
print(name, 'blocks', len(sel), 'of', len(ub), 'sample_bytes', len(sel) << 20, 'chunks16', res[16384][0], 'bytes16', res[16384][1], 'chunks64', res[65536][0], 'bytes64', res[65536][1])
