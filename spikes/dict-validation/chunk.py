#!/usr/bin/env python3
"""Phase 2: chunk a raw image (Lunix view).  usage: chunk.py <image> <csize> <outdir> <name>
Writes <outdir>/<name>-<csize>.recs (16 B records {u32 file_id=0,u32 len,u64 off} of unique non-hole chunks, first occurrence by LBA),
<name>-<csize>.h16 (first 16 bytes of the BLAKE3-256 per unique chunk) and <name>-<csize>.shares.json."""
import sys, os, json, struct
import numpy as np, blake3
img, cs, out, name = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
size = os.path.getsize(img); step = 256 << 20; seen = set(); recs = bytearray(); hs = bytearray()
total = holes = dups = 0; zero = bytes(cs)
with open(img, 'rb', buffering=0) as f:
    off = 0
    while off < size:
        buf = bytearray(min(step, size - off)); n = f.readinto(buf); mv = memoryview(buf)[:n]
        full = n // cs; nz = np.frombuffer(mv, dtype=np.uint8, count=full * cs).reshape(full, cs).view(np.uint64).any(axis=1) if full else np.zeros(0, bool)
        for i in range(full):
            total += 1
            if not nz[i]: holes += 1; continue
            d = blake3.blake3(mv[i * cs:(i + 1) * cs]).digest()
            if d in seen: dups += 1; continue
            seen.add(d); recs += struct.pack('<IIQ', 0, cs, off + i * cs); hs += d[:16]
        if n % cs:
            t = bytes(mv[full * cs:]); total += 1
            if not any(t): holes += 1
            else:
                d = blake3.blake3(t).digest()
                if d in seen: dups += 1
                else: seen.add(d); recs += struct.pack('<IIQ', 0, len(t), off + full * cs); hs += d[:16]
        off += n
base = os.path.join(out, '%s-%d' % (name, cs))
open(base + '.img', 'w').write(os.path.abspath(img)); open(base + '.recs', 'wb').write(recs); open(base + '.h16', 'wb').write(hs)
uniq = len(recs) // 16
blocks = len({struct.unpack_from('<Q', recs, i * 16 + 8)[0] >> 20 for i in range(uniq)})
json.dump({'image_bytes': size, 'chunks_total': total, 'holes': holes, 'dups': dups, 'unique': uniq, 'blocks_1MiB_nonhole': blocks,
           'unique_bytes': int(sum(struct.unpack_from('<I', recs, i * 16 + 4)[0] for i in range(uniq)))}, open(base + '.shares.json', 'w'))
print(name, cs, total, holes, dups, uniq, blocks)
