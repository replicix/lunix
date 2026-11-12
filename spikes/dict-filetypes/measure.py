#!/usr/bin/env python3 -I
"""Compress every test object with every codec/dictionary variant.  usage: measure.py [--chunk-sens]
Output: WORK/results/<set>.npz  (per file: columns of stored bytes, -1 = not computed)."""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import multiprocessing as _mp; _mp.set_start_method("fork", force=True)
import sys, os, random, time, glob, lzma, hashlib
from multiprocessing import Pool
import numpy as np, brotli
from compression import zstd
import taxonomy as T, common as C

LEVELS = (1, 3, 9, 19)
_dicts = {}
def getdict(arm, setname, name):
    k = (arm, setname, name)
    if k not in _dicts:
        p = os.path.join(C.WORK, 'dicts', arm, setname, name.replace(':', '_') + '.zdict')
        _dicts[k] = zstd.ZstdDict(open(p, 'rb').read()) if os.path.exists(p) else None
    return _dicts[k]
def _unfile(n):   # file name -> dictionary name (type_c -> type:c, generic_text stays)
    return n.replace('_', ':', 1) if n.split('_')[0] in ('type', 'family', 'ext') else n
def dict_names(arm, setname):
    return sorted(_unfile(os.path.basename(p)[:-6]) for p in glob.glob(os.path.join(C.WORK, 'dicts', arm, setname, '*.zdict')))

def resolve(arm, setname, kind, ty, ext, proj_lang=None):
    """Dictionary name used by variant `kind` for a file of true type/ext (None = no dictionary)."""
    def has(n): return getdict(arm, setname, n) is not None
    fam = 'family:' + T.FAMILY.get(ty, '-'); typ = 'type:' + ty
    chain_t = [typ, fam] + (['generic_text'] if ty in T.TEXT_TYPES else [])
    if kind == 'gt': c = ['generic_text'] if ty in T.TEXT_TYPES else []
    elif kind == 'ga': c = ['generic_all'] if ty not in T.CONTROL_TYPES else []
    elif kind == 'fam': c = [fam] + (['generic_text'] if ty in T.TEXT_TYPES else [])
    elif kind == 'typ': c = chain_t
    elif kind == 'ext':
        x = C.ext_dict_for(ty, ext); c = (['ext:' + x] if x else []) + chain_t
        if ty == 'h' and proj_lang: c = ['ext:h-' + proj_lang] + chain_t
    else: raise ValueError(kind)
    if ty in T.CONTROL_TYPES: return None
    for n in c:
        if has(n): return n
    return None

def zc(data, level, zd=None):
    return len(zstd.compress(data, level=level, zstd_dict=zd)) if zd is not None else len(zstd.compress(data, level=level))

def measure_file(a):
    r, setname, M, csize, projlang = a
    data = open(r['path'], 'rb').read(); n = len(data); out = {'id': r['id']}
    chs = [data[o:o + l] for o, l in C.chunks(n, csize)]
    def tot(f, cond=True):
        if not cond: return -1
        return sum(C.stored(len(c), f(c)) for c in chs)
    out['raw'] = tot(lambda c: len(c) + 1)       # raw object: header + payload
    out['nchunks'] = len(chs)
    mx = max(len(c) for c in chs)
    for L in LEVELS: out[f'z{L}'] = tot(lambda c: zc(c, L))
    if mx <= 1 << 20:
        out['br11'] = tot(lambda c: len(brotli.compress(c, quality=11, lgwin=24)))
        out['xz9e'] = tot(lambda c: len(lzma.compress(c, format=lzma.FORMAT_RAW, filters=[{'id': lzma.FILTER_LZMA2, 'preset': 9 | lzma.PRESET_EXTREME}])))
    pl = projlang.get(r['project'])
    for arm in 'PC':
        for kind in ('gt', 'ga', 'fam', 'typ', 'ext'):
            dn = resolve(arm, setname, kind, r['type'], r['ext'], pl)
            out[f'{arm}.{kind}.name'] = dn or '-'
            for L in LEVELS:
                ok = mx <= (256 << 10 if L == 19 else (1 << 20 if L != 3 else 1 << 30))
                if dn is None: out[f'{arm}.{kind}.z{L}'] = out[f'z{L}'] if ok else -1
                else:
                    zd = getdict(arm, setname, dn); out[f'{arm}.{kind}.z{L}'] = tot(lambda c: zc(c, L, zd), ok)
    if M and mx <= 256 << 10:        # oracle matrix, level 3, arm C (superset) dictionaries
        for dn in dict_names('C', setname):
            out['M.' + dn] = tot(lambda c: zc(c, 3, getdict('C', setname, dn)))
    # round-trip check on every 50th file
    if int(r['id'][:4], 16) % 50 == 0 and chs[0]:
        dn = out['C.typ.name']; zd = getdict('C', setname, dn) if dn != '-' else None
        z = zstd.compress(chs[0], level=3, zstd_dict=zd) if zd else zstd.compress(chs[0], level=3)
        assert (zstd.decompress(z, zstd_dict=zd) if zd else zstd.decompress(z)) == chs[0], r['id']
        out['rt_ok'] = 1
    return out

def main():
    rows = C.load_manifest(); fold_of = C.assign_folds(rows)
    sens = '--chunk-sens' in sys.argv
    projlang = {}
    cnt = {}
    for r in rows:
        if r['type'] in ('c', 'cpp'): cnt.setdefault(r['project'], {}).setdefault(r['type'], 0); cnt[r['project']][r['type']] += 1
    for p, d in cnt.items(): projlang[p] = max(d, key=d.get)
    os.makedirs(os.path.join(C.WORK, 'results'), exist_ok=True)
    # oracle subset M: <=150 files per (project,type), fixed by sha order
    grp = {}
    for r in sorted(rows, key=lambda r: r['sha256']): grp.setdefault((r['project'], r['type']), []).append(r['id'])
    Mset = {i for v in grp.values() for i in v[:150]}
    sets = {f'f{f}': [r for r in rows if fold_of[r['project']] == f] for f in range(C.K)}
    sets['full'] = [r for r in rows if fold_of[r['project']] == -1]
    with Pool(int(os.environ.get('NPROC', 4))) as pool:
        for setname, rs in sets.items():
            t0 = time.time()
            if sens:
                big = [r for r in rs if r['size'] > (1 << 20)]
                res = {}
                for cs in (1 << 20, 16 << 20):
                    out = pool.map(measure_file, [(r, setname, False, cs, projlang) for r in big], chunksize=1)
                    res[cs >> 20] = {o['id']: {k: v for k, v in o.items() if k in ('id', 'z3', 'C.typ.z3', 'P.typ.z3', 'z9', 'z19', 'nchunks')} for o in out}
                import json; json.dump({str(k): v for k, v in res.items()}, open(os.path.join(C.WORK, 'results', f'chunksens-{setname}.json'), 'w'))
                continue
            if os.environ.get('LIMIT'): rs = random.Random(1).sample(rs, min(len(rs), int(os.environ['LIMIT'])))
            args = [(r, setname, r['id'] in Mset, T.CHUNK, projlang) for r in sorted(rs, key=lambda r: -r['size'])]
            out = pool.map(measure_file, args, chunksize=8)
            keys = sorted({k for o in out for k in o})
            cols = {}
            for k in keys:
                vals = [o.get(k, -1) for o in out]
                cols[k] = np.array(vals, dtype=object if isinstance(next(v for v in vals if v != -1), str) else np.int64) if any(v != -1 for v in vals) else np.array(vals)
            np.savez_compressed(os.path.join(C.WORK, 'results', f'{setname}.npz'), **{k: (v.astype(str) if v.dtype == object else v) for k, v in cols.items()})
            sys.stderr.write(f'{setname}: {len(rs)} files, {time.time()-t0:.0f}s\n')
if __name__ == '__main__': main()
