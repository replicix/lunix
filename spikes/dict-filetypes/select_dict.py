#!/usr/bin/env python3 -I
"""Dictionary selection: extension map, libmagic, hashed byte/token naive Bayes, combinations.  Loss versus the oracle at zstd:3.
usage: select_dict.py  (needs measure.py results).  Writes WORK/out/sel_*.csv"""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import multiprocessing as _mp; _mp.set_start_method("fork", force=True)
import sys, os, re, random, zlib, csv, collections, json, time
import numpy as np, scipy.sparse as sp
import magic
import taxonomy as T, common as C
import measure as Mz
from multiprocessing import Pool

HEAD = 4096; BITS = 14; MASK = (1 << BITS) - 1
D1, D2, D3, DT = 256, 1 << BITS, 1 << BITS, 1 << BITS
OFF2, OFF3, OFFT = D1, D1 + D2, D1 + D2 + D3
DIM = D1 + D2 + D3 + DT
TOK = re.compile(rb'[A-Za-z_][A-Za-z0-9_]+|[^\sA-Za-z0-9_]{1,3}')

def feats(head, ext):
    h = np.frombuffer(head, dtype=np.uint8).astype(np.uint32)
    idx = [np.bincount(h, minlength=256)[:256].nonzero()[0]]; val = [np.bincount(h, minlength=256)[idx[0]]]
    cols = [idx[0]]; vals = [val[0]]
    if len(h) > 1:
        i2 = (((h[:-1] * 0x9E3779B1) + h[1:] * 0x85EBCA6B) & 0xFFFFFFFF) >> (32 - BITS)
        u, c = np.unique(i2, return_counts=True); cols.append(u + OFF2); vals.append(c)
    if len(h) > 2:
        i3 = (((h[:-2] * 0x9E3779B1) + h[1:-1] * 0x85EBCA6B + h[2:] * 0xC2B2AE35) & 0xFFFFFFFF) >> (32 - BITS)
        u, c = np.unique(i3, return_counts=True); cols.append(u + OFF3); vals.append(c)
    toks = [zlib.crc32(t) & MASK for t in TOK.findall(head)]
    if ext is not None: toks += [zlib.crc32(b'EXT=' + ext.encode()) & MASK] * 8
    if toks:
        u, c = np.unique(np.array(toks), return_counts=True); cols.append(u + OFFT); vals.append(c)
    return np.concatenate(cols), np.concatenate(vals).astype(np.float32)

def perturb(name, mode, rng, all_exts):
    """N0 real name, N1 no extension, N2 misleading extension."""
    if mode == 0: return name
    base = name.rsplit('.', 1)[0] if '.' in name.lstrip('.') else name
    if mode == 1: return 'file' + str(rng.randrange(1000))
    return base + '.' + rng.choice(all_exts + ['txt', 'dat', 'bak', 'tmp', 'old'])

_mg = None
def work(a):
    global _mg
    path, name, size = a
    if _mg is None: _mg = magic.Magic()
    with open(path, 'rb') as f: b = f.read(65536)
    return b[:HEAD], _mg.from_buffer(b)

MAGIC_MAP = [(r'Java class data', 'class'), (r'^ELF', 'elf'), (r'WebAssembly', 'wasm'), (r'PDF document', 'pdf'), (r'SQLite 3', 'sqlite'), (r'PNG image', 'png'), (r'JPEG image', 'jpeg'),
 (r'Zip archive|Java archive|Microsoft (Word|Excel)|OpenDocument|EPUB', 'zip'), (r'gzip compressed|bzip2 compressed|XZ compressed|Zstandard', 'gz'),
 (r'ISO Media|WebM|Matroska|MPEG|Ogg data|RIFF|GIF image|Web Open Font|Audio file|MP3|Media', 'video'), (r'SVG Scalable', 'svg'), (r'JSON', 'json'), (r'XML', 'xml'), (r'HTML document|XHTML', 'html'),
 (r'C\+\+ source', 'cpp'), (r'^C source', 'c'), (r'Python.*script|Python.*byte-compiled', 'python'), (r'Perl.*(script|module)', 'perl'), (r'PHP script', 'php'), (r'Ruby.*(script|module)', 'ruby'),
 (r'shell script|Bourne|Korn', 'sh'), (r'Lua', 'lua'), (r'Java source', 'java'), (r'JavaScript|Node\.js', 'js'), (r'TeX|LaTeX', 'tex'), (r'makefile', 'make'), (r'Rust', 'rust'), (r'Go source|Golang', 'go'),
 (r'Pascal|Fortran|Algol|ASCII text|UTF-8|Unicode text|ISO-8859|CSV|text', 'TEXT'), (r'data|empty', 'BIN')]
def magic_type(desc):
    for rx, t in MAGIC_MAP:
        if re.search(rx, desc): return t
    return 'BIN'

class NB:
    def fit(self, X, y, classes, alpha=0.05):
        self.cl = classes; ci = {c: i for i, c in enumerate(classes)}; yi = np.array([ci[t] for t in y])
        Y = sp.csr_matrix((np.ones(len(y)), (yi, np.arange(len(y)))), shape=(len(classes), len(y)))
        S = (Y @ X.log1p()).toarray() + alpha          # sublinear tf
        self.W = np.log(S / S.sum(1, keepdims=True)).astype(np.float32); return self
    def post(self, X):
        s = X.log1p() @ self.W.T; s = np.asarray(s); s -= s.max(1, keepdims=True); p = np.exp(s); return p / p.sum(1, keepdims=True)

def main():
    rows = C.load_manifest(); fold_of = C.assign_folds(rows)
    rows = [r for r in rows]; rng = random.Random(5)
    all_exts = sorted(T.EXT_TO_TYPE)
    cache = os.path.join(C.WORK, 'heads.pkl')
    import pickle
    if os.path.exists(cache): heads, descs = pickle.load(open(cache, 'rb'))
    else:
        with Pool(4) as p: out = p.map(work, [(r['path'], r['name'], r['size']) for r in rows], chunksize=64)
        heads = [o[0] for o in out]; descs = [o[1] for o in out]; pickle.dump((heads, descs), open(cache, 'wb'))
    classes = T.ALL_TYPES
    names = {m: [perturb(r['name'], m, random.Random(r['id'] + str(m)), all_exts) for r in rows] for m in (0, 1, 2)}
    ext_n = {m: [(T.ext_of(n).lower() if '.' in n.lstrip('.') else '') for n in names[m]] for m in names}
    def X_of(idx, mode, with_ext):
        rr, cc, vv = [], [], []
        for k, i in enumerate(idx):
            c, v = feats(heads[i], (ext_n[mode][i] if with_ext else None)); rr.append(np.full(len(c), k)); cc.append(c); vv.append(v)
        return sp.csr_matrix((np.concatenate(vv), (np.concatenate(rr), np.concatenate(cc))), shape=(len(idx), DIM))
    ty = np.array([r['type'] for r in rows]); setof = np.array([('full' if fold_of[r['project']] < 0 else f"f{fold_of[r['project']]}") for r in rows])
    pol = np.array([r['policy'] for r in rows])
    pred = {k: np.empty(len(rows), dtype=object) for k in ['ext0', 'ext1', 'ext2', 'magic', 'magic_cal', 'nb', 'nbx0', 'nbx1', 'nbx2', 'cas0', 'cas1', 'cas2', 'cas2b0', 'cas2b1', 'cas2b2']}
    conf = {k: np.zeros(len(rows)) for k in ['nb', 'nbx0', 'nbx1', 'nbx2']}
    for s in ['f0', 'f1', 'f2', 'f3', 'full']:
        te = np.where(setof == s)[0]
        trn = np.array([i for i, r in enumerate(rows) if setof[i] != s and setof[i] != 'full' and pol[i] != 'excluded'])
        # class-balanced training subsample (<=600 files per type) keeps training fast and priors flat
        byt = collections.defaultdict(list)
        for i in trn: byt[ty[i]].append(i)
        trn = np.array([i for t, l in byt.items() for i in random.Random(t + s).sample(l, min(600, len(l)))])
        Xn = X_of(trn, 0, False); nb = NB().fit(Xn, ty[trn], classes)
        # extension-aware NB: train with 80% real names, 10% stripped, 10% misleading
        modes = [random.Random(int(i)).choices([0, 1, 2], [8, 1, 1])[0] for i in trn]
        Xe = sp.vstack([X_of([i], m, True) for i, m in zip(trn, modes)]); nbx = NB().fit(Xe, ty[trn], classes)
        # calibrated libmagic: description (digits removed) -> majority true type in training
        dm = collections.defaultdict(collections.Counter)
        for i in trn: dm[re.sub(r'\d+', 'N', descs[i])][ty[i]] += 1
        Xt = X_of(te, 0, False); P = nb.post(Xt); a = P.argmax(1)
        for k, i in enumerate(te): pred['nb'][i] = classes[a[k]]; conf['nb'][i] = P[k, a[k]]
        for m in (0, 1, 2):
            Pe = nbx.post(X_of(te, m, True)); ae = Pe.argmax(1)
            for k, i in enumerate(te):
                pred[f'nbx{m}'][i] = classes[ae[k]]; conf[f'nbx{m}'][i] = Pe[k, ae[k]]
                e = ext_n[m][i]; et = T.classify_name(names[m][i]) if e or names[m][i] in T.NAME else None
                pred[f'ext{m}'][i] = et or 'UNKNOWN'
                pred[f'cas{m}'][i] = et if (et and et != 'h') else pred['nb'][i]
                nbt = pred['nb'][i]
                pred[f'cas2b{m}'][i] = (et if (et is None or nbt == et or conf['nb'][i] < 0.9) else nbt) if et else nbt
        for k, i in enumerate(te):
            d = descs[i]; pred['magic'][i] = magic_type(d)
            c = dm.get(re.sub(r'\d+', 'N', d)); pred['magic_cal'][i] = c.most_common(1)[0][0] if c else magic_type(d)
        sys.stderr.write(f'{s}: nb acc {np.mean([pred["nb"][i]==ty[i] for i in te]):.3f}\n')
        if s == 'f0': json.dump({'nb_weight_floats': int(nb.W.size), 'dim': DIM, 'classes': len(classes)}, open(os.path.join(C.WORK, 'nb_size.json'), 'w'))
    # ---- loss versus oracle at zstd:3 using the matrix (objects <= 256 KiB in subset M, arm C dictionaries)
    out = []; confrows = collections.Counter()
    rid = {r['id']: i for i, r in enumerate(rows)}
    for s in ['f0', 'f1', 'f2', 'f3', 'full']:
        z = np.load(os.path.join(C.WORK, 'results', f'{s}.npz'), allow_pickle=True); ids = z['id']
        zf = {k.replace('M.generic:', 'M.generic_'): k for k in z.files if k.startswith('M.')}; mkeys = sorted(zf)
        M = np.stack([z[zf[k]] for k in mkeys], 1); z3 = z['z3']; avail = (M >= 0).all(1)
        names_m = [k[2:] for k in mkeys]; sizes = np.array([rows[rid[i]]['size'] for i in ids])
        best = np.minimum(z3, np.where(M >= 0, M, 1 << 60).min(1))
        for strat in ['none', 'true', 'generic_text', 'oracle'] + list(pred):
            for j in np.where(avail)[0]:
                r = rows[rid[ids[j]]]; i = rid[ids[j]]
                if strat == 'oracle': b = best[j]
                elif strat == 'none': b = z3[j]
                else:
                    t = r['type'] if strat == 'true' else ('generic_text' if strat == 'generic_text' else pred[strat][i])
                    if strat == 'generic_text': dn = 'generic_text' if r['type'] in T.TEXT_TYPES else None
                    elif t in ('TEXT',): dn = 'generic_text'
                    elif t in ('BIN', 'UNKNOWN'): dn = 'generic_text' if (r['type'] in T.TEXT_TYPES) and t == 'UNKNOWN' else None
                    else: dn = Mz.resolve('C', s, 'typ', t, '', None)
                    b = M[j, names_m.index(dn)] if dn in names_m else z3[j]
                out.append((strat, s, ids[j], r['type'], r['project'], int(sizes[j]), int(z3[j]), int(best[j]), int(b)))
                if strat in pred: confrows[(strat, r['type'], pred[strat][i])] += 1
    os.makedirs(os.path.join(C.WORK, 'out'), exist_ok=True)
    with open(os.path.join(C.WORK, 'out', 'sel_loss.csv'), 'w', newline='') as f:
        w = csv.writer(f); w.writerow(['strategy', 'set', 'id', 'type', 'project', 'size', 'z3_nodict', 'oracle', 'chosen']); w.writerows(out)
    acc = []
    for k in pred:
        for s in ['f0', 'f1', 'f2', 'f3', 'full']:
            te = np.where(setof == s)[0]; acc.append((k, s, len(te), int(sum(pred[k][i] == ty[i] for i in te))))
    with open(os.path.join(C.WORK, 'out', 'sel_acc.csv'), 'w', newline='') as f:
        w = csv.writer(f); w.writerow(['strategy', 'set', 'n', 'correct']); w.writerows(acc)
    with open(os.path.join(C.WORK, 'out', 'sel_confusion.csv'), 'w', newline='') as f:
        w = csv.writer(f); w.writerow(['strategy', 'true', 'pred', 'n']); [w.writerow([a, b, c, n]) for (a, b, c), n in sorted(confrows.items())]
    # accuracy over all files (not only M) for confusion
    cf = collections.Counter()
    for k in pred:
        for i in range(len(rows)):
            if pred[k][i] is not None: cf[(k, ty[i], pred[k][i])] += 1
    with open(os.path.join(C.WORK, 'out', 'sel_confusion_all.csv'), 'w', newline='') as f:
        w = csv.writer(f); w.writerow(['strategy', 'true', 'pred', 'n']); [w.writerow([a, b, c, n]) for (a, b, c), n in sorted(cf.items())]
if __name__ == '__main__': main()
