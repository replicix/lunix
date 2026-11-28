#!/usr/bin/env python3
"""The dedup and compression ratio of the data actually on a device (or file), measured independently of Lunix.

  chunkstat.py PATH CHUNK_BYTES LENGTH_BYTES [OFFSET] > chunkstat.json

Reads LENGTH bytes from OFFSET in CHUNK-sized blocks (page cache bypassed on block devices is not needed: the reads
come after the writes were flushed), hashes every block (BLAKE2b-128), and compresses a 1-in-STRIDE sample of the
distinct blocks with zlib level 1 as a stand-in for the daemon's `auto` codec (lz4 or zstd; only the order of
magnitude of the ratio is comparable). Zero blocks are counted apart (the daemon stores them as map holes).
This is the yardstick next to the nominal 1/(1-p) of the fio knobs: if fio's own data deviates from the nominal ratio
the table shows it.
"""
import hashlib
import json
import sys
import zlib

path, chunk, length = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
offset = int(sys.argv[4]) if len(sys.argv) > 4 else 0
STRIDE = 16
seen = {}
n = zeros = 0
zero = bytes(chunk)
with open(path, "rb", buffering=0) as f:
    f.seek(offset)
    done = 0
    while done < length:
        b = f.read(min(chunk, length - done))
        if len(b) < chunk:
            break
        done += chunk
        if b == zero:
            zeros += 1
            continue
        n += 1
        h = hashlib.blake2b(b, digest_size=16).digest()
        if h not in seen:
            seen[h] = b if (len(seen) % STRIDE == 0) else None
uniq = len(seen)
samp = [b for b in seen.values() if b is not None]
cu = sum(len(b) for b in samp)
cc = sum(len(zlib.compress(b, 1)) for b in samp)
json.dump({"chunk": chunk, "blocks": n + zeros, "zero_blocks": zeros, "nonzero_blocks": n, "unique": uniq,
           "dedup_ratio": (n / uniq) if uniq else None,
           "zlib1_compression_ratio_sample": (cu / cc) if cc else None, "sample_chunks": len(samp)}, sys.stdout)
print()
