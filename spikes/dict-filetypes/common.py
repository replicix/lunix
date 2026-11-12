"""Shared helpers: manifest, folds, training-sample selection, stored-size model."""
import os, csv, random, hashlib, struct, collections
import taxonomy as T
CORPUS = os.environ.get('DICTFT_CORPUS', os.path.expanduser('~/dictft/corpus'))
WORK = os.environ.get('DICTFT_WORK', os.path.expanduser('~/dictft/work'))
K = 4
DICT_SIZE = 112640          # zstd's default dictionary size (112 KiB)

def load_manifest():
    rows = []
    with open(os.path.join(CORPUS, 'manifest.tsv')) as f:
        rd = csv.DictReader(f, delimiter='\t')
        for r in rd:
            if r['type'] not in T.ALL_TYPES: continue
            r['size'] = int(r['size']); r['path'] = os.path.join(CORPUS, 'files', r['project'], r['id'])
            r['ext'] = T.ext_of(r['name']).lower() if '.' in r['name'].lstrip('.') else ''
            rows.append(r)
    return rows

def assign_folds(rows):
    """project -> fold in 0..K-1 for eco=main; -1 for eco=new (never trained on).  Greedy balance per type."""
    projs = collections.defaultdict(lambda: collections.defaultdict(int))   # project -> type -> bytes
    eco = {}
    for r in rows:
        projs[r['project']][r['type']] += r['size']; eco[r['project']] = r['eco']
    fold = {p: -1 for p in projs if eco[p] == 'new'}
    main = sorted([p for p in projs if eco[p] == 'main'], key=lambda p: (len(projs[p]), p))
    cnt = collections.defaultdict(lambda: [0] * K)       # type -> per-fold counts
    rng = random.Random(1234)
    for p in main:
        score = [sum(cnt[t][f] for t in projs[p]) + rng.random() * 0.1 for f in range(K)]
        f = score.index(min(score)); fold[p] = f
        for t in projs[p]: cnt[t][f] += 1
    return fold

def chunks(n, csize):
    return [(o, min(csize, n - o)) for o in range(0, n, csize)] or [(0, 0)]

def stored(raw_len, comp_len):
    """Constellation object size: 16 B header + payload, raw if compression does not help."""
    return T.HEADER_LEN + (comp_len if comp_len < raw_len else raw_len)

def pick_samples(files, budget, rng, headcap=65536):
    """files: list of (path, project, size, spdx). Equalise bytes across projects. Returns list of (bytes, path, project, spdx)."""
    byp = collections.defaultdict(list)
    for f in files: byp[f[1]].append(f)
    for p in byp: rng.shuffle(byp[p])
    order = sorted(byp, key=lambda p: sum(min(x[2], headcap) for x in byp[p]))
    out = []; left = budget
    for i, p in enumerate(order):
        quota = left / (len(order) - i); used = 0
        for path, proj, size, spdx in byp[p]:
            if used >= quota or len(out) > 20000: break
            n = min(size, headcap)
            with open(path, 'rb') as fh:
                if size > 4 * headcap and rng.random() < 0.5: fh.seek(rng.randrange(0, size - headcap))   # a middle slice for big files
                b = fh.read(n)
            out.append((b, path, proj, spdx)); used += len(b)
        left -= used
    return out

def set_dict_id(d, did):
    assert d[:4] == b'\x37\xa4\x30\xec'
    return d[:4] + struct.pack('<I', did) + d[8:]

EXT_DICTS = {  # ext-specific dictionaries (name -> (type, [exts]))
 'ts': ('ts', ['ts','mts','cts']), 'tsx': ('ts', ['tsx']), 'js': ('js', ['js','mjs','cjs']), 'jsx': ('js', ['jsx']),
 'cpp-src': ('cpp', ['cc','cpp','cxx','c++']), 'cpp-hdr': ('cpp', ['hpp','hh','hxx','ipp','tpp','inl']),
 'css': ('css', ['css']), 'scss': ('css', ['scss']), 'less': ('css', ['less']),
 'f77': ('fortran', ['f','for']), 'f90': ('fortran', ['f90','f95','f03']),
 'csproj': ('xml', ['csproj','props','targets','resx']), 'plist-xml': ('xml', ['xml','xsd','xsl','xslt','plist']),
 'ini': ('ini', ['ini','cfg']), 'conf': ('ini', ['conf']), 'properties': ('ini', ['properties']),
 'py': ('python', ['py']), 'pyi': ('python', ['pyi']),
}
def ext_dict_for(ty, ext):
    for n, (t, es) in EXT_DICTS.items():
        if t == ty and ext in es: return n
    return None
