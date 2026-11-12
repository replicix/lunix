#!/usr/bin/env python3
"""measure.py <corpus> <out.csv.gz>  (run under taskset -c 12-15; 4 worker processes)
Per file: stored bytes under Constellation's model (<4 MiB: one object; larger: 4 MiB chunks, first 8 chunks measured;
zstd level L, stored raw if compressed >= original) for L in 1,3,7,19; Lunix model (16/64 KiB chunks over the first
1 MiB, 4 KiB LZ4 probe <6 % saving -> raw, else zstd-1, raw if not smaller); and held-out dictionary results
(5-fold CV grouped by source, dictionaries of 16 KiB and 112 KiB trained on head 32 KiB + tail 8 KiB of training files)."""
import sys, os, gzip, csv, hashlib, random
from multiprocessing import Pool
import zstandard as zstd, lz4.block
MiB = 1 << 20; KiB = 1024
C = os.path.expanduser(sys.argv[1]); OUT = sys.argv[2]
LV = (1, 3, 7, 19); MAXOBJ = 8; LUNIX_CAP = MiB
F = [dict(zip(l.split("\t"), [])) for l in []]
rows = [l.rstrip("\n").split("\t") for l in open(os.path.join(C, "..", "files.tsv"))]
hdr, rows = rows[0], rows[1:]
files = [dict(zip(hdr, r)) for r in rows]

def bucket(size):
    return "lt4K" if size < 4 * KiB else "4K-64K" if size < 64 * KiB else "64K-1M" if size < MiB else "1M-4M" if size < 4 * MiB else "gt4M"
def objs(data):
    n = len(data)
    if n < 4 * MiB: return [data]
    return [data[i:i + 4 * MiB] for i in range(0, min(n, MAXOBJ * 4 * MiB), 4 * MiB)]
def stored(c, o): return len(o) if len(c) >= len(o) else len(c)
_cc = {}
def cctx(l, d=None):
    k = (l, id(d))
    if k not in _cc: _cc[k] = zstd.ZstdCompressor(level=l, dict_data=d)
    return _cc[k]
def lunix(data, csz, d=None):
    """returns (chunks, stored_probe, raw_probe, stored_noprobe, raw_noprobe)"""
    data = data[:LUNIX_CAP]; n = rp = rn = sp = sn = 0
    c1 = cctx(1, d)
    for i in range(0, len(data), csz):
        ch = data[i:i + csz]; n += 1
        z = len(c1.compress(ch))
        sn += min(z, len(ch)); rn += z >= len(ch)
        p = ch[:4096]
        pl = len(lz4.block.compress(p, store_size=False))
        if pl > len(p) * 0.94: sp += len(ch); rp += 1       # probe saves < 6 %: raw
        else: sp += min(z, len(ch)); rp += z >= len(ch)
    return n, sp, rp, sn, rn

def base(f):
    data = open(os.path.join(C, f["path"]), "rb").read()
    r = dict(f); r["bucket"] = bucket(len(data)); os_ = objs(data)
    r["nobj"] = len(os_); r["cov"] = sum(map(len, os_))
    for l in LV:
        cs = [cctx(l).compress(o) for o in os_]
        r[f"z{l}"] = sum(stored(c, o) for c, o in zip(cs, os_)); r[f"z{l}_raw"] = sum(len(c) >= len(o) for c, o in zip(cs, os_))
        r[f"z{l}_plain"] = sum(len(c) for c in cs)   # without the raw guard
    for csz, nm in ((16 * KiB, "l16"), (64 * KiB, "l64")):
        n, sp, rp, sn, rn = lunix(data, csz)
        r[nm + "_in"] = min(len(data), LUNIX_CAP); r[nm + "_n"] = n; r[nm + "_probe"] = sp; r[nm + "_probe_raw"] = rp; r[nm + "_noprobe"] = sn; r[nm + "_noprobe_raw"] = rn
    return r

def sample(data):
    s = [data[:32 * KiB]]
    if len(data) > 40 * KiB: s.append(data[-8 * KiB:])
    return s

def fold(src): return int(hashlib.sha1(src.encode()).hexdigest(), 16) % 5

def dicttask(args):
    fm, k = args
    tr = [f for f in files if f["fmt"] == fm and fold(f["source"]) != k]
    te = [f for f in files if f["fmt"] == fm and fold(f["source"]) == k]
    if len(tr) < 12 or len({f["source"] for f in tr}) < 4 or not te: return []
    random.Random(k).shuffle(tr); smp = []
    for f in tr[:600]: smp += sample(open(os.path.join(C, f["path"]), "rb").read())
    smp = [s for s in smp if len(s) > 0]
    out = []; ds = {}
    for nm, sz in (("16k", 16 * KiB), ("112k", 112 * KiB)):
        try: ds[nm] = zstd.train_dictionary(sz, smp)
        except Exception as e: ds[nm] = None
    for f in te:
        data = open(os.path.join(C, f["path"]), "rb").read(); os_ = objs(data)
        r = {"path": f["path"]}
        for nm, d in ds.items():
            if d is None: continue
            cs = [cctx(3, d).compress(o) for o in os_]
            r["d3_" + nm] = sum(stored(c, o) for c, o in zip(cs, os_)); r["d3_%s_raw" % nm] = sum(len(c) >= len(o) for c, o in zip(cs, os_))
            r["d3_%s_plain" % nm] = sum(len(c) for c in cs)
        if ds.get("16k"):
            for csz, nm in ((16 * KiB, "l16"), (64 * KiB, "l64")):
                n, sp, rp, sn, rn = lunix(data, csz, ds["16k"]); r[nm + "_dict_probe"] = sp; r[nm + "_dict_noprobe"] = sn
        out.append(r)
    return out

if __name__ == "__main__":
    with Pool(4) as p:
        res = {}
        for i, r in enumerate(p.imap_unordered(base, files, chunksize=4)):
            res[r["path"]] = r
            if i % 200 == 0: print("base", i, len(files), flush=True)
        fmts = sorted({f["fmt"] for f in files})
        for rs in p.imap_unordered(dicttask, [(fm, k) for fm in fmts for k in range(5)]):
            for r in rs: res[r["path"]].update({k: v for k, v in r.items() if k != "path"})
            print("dict batch", len(rs), flush=True)
    cols = []
    for r in res.values():
        for k in r:
            if k not in cols: cols.append(k)
    with gzip.open(OUT, "wt", newline="") as o:
        w = csv.DictWriter(o, cols, extrasaction="ignore"); w.writeheader()
        for r in sorted(res.values(), key=lambda r: (r["fmt"], r["path"])): w.writerow(r)
