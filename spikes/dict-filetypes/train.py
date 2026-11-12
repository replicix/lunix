#!/usr/bin/env python3 -I
"""Train dictionaries.  usage: train.py [--sweep]  Writes WORK/dicts/<arm>/<set>/<name>.zdict and dict_manifest.csv.
arm P = permissive-only training data, arm C = permissive + allowed copyleft.  set = fold 0..3 (project-disjoint) or 'full'."""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import multiprocessing as _mp; _mp.set_start_method("fork", force=True)
import sys, os, json, random, collections, csv, time
from multiprocessing import Pool
import taxonomy as T, common as C

def groups(rows, arm, fold_of, hold):
    """Return {dictname: list of (path, project, size, spdx)} for training set excluding fold `hold` (None=full)."""
    ok = {'P': {'perm'}, 'C': {'perm', 'copyleft'}}[arm]
    tr = [r for r in rows if fold_of[r['project']] >= 0 and fold_of[r['project']] != hold and r['policy'] in ok]
    g = collections.defaultdict(list)
    projlang = collections.defaultdict(collections.Counter)
    for r in rows:
        if r['type'] in ('c', 'cpp'): projlang[r['project']][r['type']] += 1
    for r in tr:
        e = (r['path'], r['project'], r['size'], r['spdx'] if 'spdx' in r else r['file_spdx'])
        t = r['type']
        g['type:' + t].append(e)
        if t in T.TEXT_TYPES: g['generic_text'].append(e)
        if t not in T.CONTROL_TYPES: g['generic_all'].append(e)
        if t in T.FAMILY and t not in T.CONTROL_TYPES: g['family:' + T.FAMILY[t]].append(e)
        x = C.ext_dict_for(t, r['ext'])
        if x: g['ext:' + x].append(e)
        if t == 'h' and projlang[r['project']]:
            g['ext:h-c' if projlang[r['project']].most_common(1)[0][0] == 'c' else 'ext:h-cpp'].append(e)
    return g

def job(a):
    arm, setname, name, files, budget, size, seed, outdir, did = a
    out = os.path.join(outdir, arm, setname, name.replace(':', '_') + '.zdict')
    projects = {f[1] for f in files}
    base = dict(arm=arm, set=setname, name=name, dict_kib=size // 1024, budget_mib=budget / 2**20, n_proj=len(projects), n_files_avail=len(files))
    if len(projects) < 2 or sum(min(f[2], 65536) for f in files) < 150_000 or len(files) < 30:
        return dict(base, status='skipped (too little training data)')
    rng = random.Random(seed); smp = C.pick_samples(files, budget, rng)
    t0 = time.time()
    import subprocess, shutil, tempfile
    tmp = tempfile.mkdtemp(dir=os.path.join(C.WORK, 'tmp'))
    try:
        for i, s in enumerate(smp): open(os.path.join(tmp, f'{i:05d}'), 'wb').write(s[0])
        os.makedirs(os.path.dirname(out), exist_ok=True)
        # zstd's optimising COVER trainer (k, d searched, 16 steps); about 1-2 pp better than the stdlib's default fastCover on our sets (see SUMMARY)
        r = subprocess.run(['zstd', '--train-cover=steps=16', '-r', tmp, f'--maxdict={size}', '-o', out, '-q', '-f', '-T1', f'--dictID={did}'], capture_output=True, text=True)
        if r.returncode: return dict(base, status='fail ' + r.stderr[:80])
        d = open(out, 'rb').read()
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    lic = collections.Counter(); pol = collections.Counter()
    for b, path, proj, spdx in smp: lic[spdx] += len(b)
    tot = sum(lic.values())
    return dict(base, status='ok', dict_bytes=len(d), n_samples=len(smp), train_bytes=tot, train_s=round(time.time() - t0, 1), dict_id=hex(did),
                projects=';'.join(sorted({s[2] for s in smp})), licence_mix=json.dumps({k: round(v / tot, 4) for k, v in lic.most_common()}))

def main():
    rows = C.load_manifest(); fold_of = C.assign_folds(rows)
    outdir = os.path.join(C.WORK, 'dicts'); sweep = '--sweep' in sys.argv
    jobs = []; did = 0x80000000
    if not sweep:
        for arm in 'PC':
            for hold in [0, 1, 2, 3, None]:
                g = groups(rows, arm, fold_of, hold)
                for name, files in sorted(g.items()):
                    did += 1
                    bud = (12 if name.startswith(('generic', 'family')) else 6) << 20
                    jobs.append((arm, 'full' if hold is None else f'f{hold}', name, files, bud, C.DICT_SIZE, 7, outdir, did))
    else:   # dictionary-size and training-size sweeps on fold 0, arm C, selected names
        g = groups(rows, 'C', fold_of, 0)
        names = ['type:python', 'type:c', 'type:js', 'type:json', 'type:html', 'type:md', 'type:java', 'type:yaml', 'type:rust', 'type:xml', 'generic_text', 'family:clike', 'family:data', 'family:markup']
        for name in names:
            if name not in g: continue
            for kib in (16, 32, 64, 112, 256):
                did += 1; jobs.append(('C', f'sweep-size{kib}', name, g[name], (12 if name.startswith(('generic', 'family')) else 6) << 20, kib * 1024, 7, outdir, did))
            for mib in (0.25, 1, 4, 16):
                did += 1; jobs.append(('C', f'sweep-train{mib}', name, g[name], int(mib * 2**20), C.DICT_SIZE, 7, outdir, did))
    if os.environ.get('TIMING_SUBSET'):   # quiet-host timing run: only the dictionaries throughput.py needs, trained on the small subset
        jobs = [j for j in jobs if j[0] == 'C' and j[1] == 'f0' and j[2] in ('generic_text', 'generic_all', 'type:python', 'type:json')]
    os.makedirs(os.path.join(C.WORK, 'tmp'), exist_ok=True); t0 = time.time()
    with Pool(int(os.environ.get('NPROC', 4))) as p:
        res = []
        for i, r in enumerate(p.imap_unordered(job, jobs, chunksize=1)):
            res.append(r)
            if i % 20 == 0: sys.stderr.write(f'{i}/{len(jobs)} {time.time()-t0:.0f}s\n')
    fn = os.path.join(C.WORK, 'dict_manifest_sweep.csv' if sweep else 'dict_manifest.csv')
    keys = sorted({k for r in res for k in r}, key=lambda k: ['arm','set','name','status'].index(k) if k in ['arm','set','name','status'] else 9)
    with open(fn, 'w', newline='') as f:
        w = csv.DictWriter(f, keys); w.writeheader(); [w.writerow(r) for r in sorted(res, key=lambda r: (r['arm'], r['set'], r['name']))]
if __name__ == '__main__': main()
