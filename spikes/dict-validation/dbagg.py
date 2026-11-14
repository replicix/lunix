#!/usr/bin/env python3
"""Aggregates for the database images -> CSVs (aggregate counts and bytes only, no per-chunk rows).
usage: dbagg.py <outdir> <name>...        writes db_lunix_arms.csv, db_cv_arms.csv, db_shares.csv, db_pvsamples.csv into <outdir>
Columns: corpus,view,size,pop,class,arm,n,raw_bytes,stored_bytes.  stored = min(compressed, length); Lunix dictionary arms and `probe`/`z*-probe` sit
behind the ADR-12 probe (raw if the 4 KiB LZ4 probe saves < 6 %); `z1`/`z3` are plain (probe off).  pop `all` = every unique chunk (non-pv arms);
pop `pv-sK` = the chunks outside the seed-K per-volume sample, with every arm recomputed on them (hold-out of `pv`)."""
import sys, os, json, csv, numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib
REC = np.dtype([('id', '<u4'), ('len', '<u4'), ('off', '<u8')]); RES = os.path.join(lib.WORK, 'res')
def load(binp):
    cols = open(binp + '.cols').read().split(); a = np.fromfile(binp, dtype=np.uint32); return {c: a.reshape(-1, len(cols))[:, i].astype(np.int64) for i, c in enumerate(cols)}
def cname(c):
    return {6: 'dbpage', 7: 'wal', 8: 'dbother'}.get(int(c), 'other')
NM = np.array([cname(c) for c in range(256)])
def agg(w, view, name, size, pop, classes, mask, L, arms):
    for cl in ['all', 'dbpage', 'wal', 'dbother', 'other']:
        m = mask if cl == 'all' else mask & (classes == cl)
        n = int(m.sum()); raw = int(L[m].sum())
        for a, v in arms.items(): w.writerow([name, view, size, pop, cl, a, n, raw, int(v[m].sum())])
def main(out, names):
    fl = open(os.path.join(out, 'db_lunix_arms.csv'), 'w', newline=''); fc = open(os.path.join(out, 'db_cv_arms.csv'), 'w', newline='')
    fs = open(os.path.join(out, 'db_shares.csv'), 'w', newline=''); fp = open(os.path.join(out, 'db_pvsamples.csv'), 'w', newline='')
    wl, wc, ws, wp = [csv.writer(f) for f in (fl, fc, fs, fp)]
    hdr = ['corpus', 'view', 'size', 'pop', 'class', 'arm', 'n', 'raw_bytes', 'stored_bytes']; wl.writerow(hdr); wc.writerow(hdr)
    ws.writerow(['corpus', 'view', 'size', 'objects_or_chunks_total', 'holes', 'dups', 'unique', 'blocks_1MiB_or_files', 'unique_bytes', 'probe_raw_share_bytes', 'zero_objects', 'engine', 'version', 'page_size', 'load_seconds'])
    wp.writerow(['corpus', 'view', 'size', 'seed', 'sample_units', 'sample_bytes', 'test_units', 'test_bytes'])
    for name in names:
        info = json.load(open(os.path.join(lib.OUT, name + '.json'))); ctl = '-ctl-' in name
        for cs in (16384, 65536):
            b = os.path.join(lib.WORK, '%s-%d' % (name, cs)); p = os.path.join(RES, '%s-%d.db.bin' % (name, cs))
            if not os.path.exists(p): continue
            rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64); cls = NM[np.fromfile(b + '.cls', dtype=np.uint8)]; C = load(p)
            zp = os.path.join(RES, 'zd-%s-%d.npz' % (name, cs)); ZD = np.load(zp) if os.path.exists(zp) else None
            pl = np.minimum(L, 4096); praw = C['probe'] >= 0.94 * pl
            beh = lambda z: np.where(praw, L, np.minimum(z, L))
            def mk(seed):
                arms = {'none': L, 'z1': np.minimum(C['z1'], L), 'z3': np.minimum(C['z3'], L), 'probe': beh(C['z1']), 'z3-probe': beh(C['z3'])}
                if ZD is not None:
                    for lv in (1, 3):
                        zl = np.minimum(C['z%d' % lv], L); ft = ZD['ft%d' % lv].astype(np.int64); orc = ZD['orc%d' % lv].astype(np.int64)
                        arms['ft@%d' % lv] = np.where(praw, L, np.where(ft == 0xFFFFFFFF, zl, np.minimum(ft, L)))
                        arms['oracle@%d' % lv] = np.where(praw, L, np.minimum(np.minimum(zl, np.minimum(orc, L)), np.minimum(C['dbd@%d' % lv], L)))
                for lv in (1, 3):
                    for d in ('dbd', 'dbs'): arms['%s@%d' % (d, lv)] = beh(C['%s@%d' % (d, lv)])
                    if seed:
                        arms['pv@%d' % lv] = beh(C['pv%d@%d' % (seed, lv)])
                        if seed == 1:
                            cc = C['class']; z = C['z%d' % lv]; pz = np.where(cc == 1, C.get('pv1m@%d' % lv, z), np.where(cc == 2, C.get('pv1t@%d' % lv, z), np.where(cc == 3, C.get('pv1b@%d' % lv, z), z)))
                            arms['pvclass@%d' % lv] = beh(pz)
                return arms
            agg(wl, 'lunix', name, cs, 'all', cls, np.ones(len(L), bool), L, mk(0))
            sh = json.load(open(b + '.shares.json')); sh2 = {'raw': int(L[praw].sum())}
            ws.writerow([name, 'lunix', cs, sh['chunks_total'], sh['holes'], sh['dups'], sh['unique'], sh['blocks_1MiB_nonhole'], sh['unique_bytes'], '%.4f' % (L[praw].sum() / L.sum()), '', info['engine'], info['version'], info['page_size'], info['load_seconds']])
            for s in (1, 2, 3):
                idx = np.load(os.path.join(RES, 'pvsample-%s-%d-s%d-0.01.npy' % (name, cs, s))); m = np.ones(len(L), bool); m[idx] = False
                wp.writerow([name, 'lunix', cs, s, len(idx), int(L[idx].sum()), int(m.sum()), int(L[m].sum())])
                agg(wl, 'lunix', name, cs, 'pv-s%d' % s, cls, m, L, mk(s))
        for tag in ('4m', '1m', 'pg'):
            b = os.path.join(lib.WORK, 'cv-%s-%s' % (name, tag)); p = os.path.join(RES, 'cv-%s-%s.db.bin' % (name, tag))
            if not os.path.exists(p): continue
            rec = np.fromfile(b + '.recs', dtype=REC); L = rec['len'].astype(np.int64); pc = NM[np.fromfile(b + '.pcls', dtype=np.uint8)]; C = load(p); sh = json.load(open(b + '.shares.json'))
            zero = C['class'] == 0
            def mk(seed):
                arms = {'none': L, 'z3': np.minimum(C['z3@3'] if 'z3@3' in C else C['z3'], L)}
                for d in ('dbd', 'dbs'): arms[d + '@3'] = np.minimum(C[d + '@3'], L)
                if seed: arms['pv@3'] = np.minimum(C['pv%d@3' % seed], L)
                return arms
            sz = sh['object_size']
            ws.writerow([name, 'constellation', sz, sh['objects'], 0, 0, sh['objects'], sh['files'], sh['bytes'], '', int(zero.sum()), info['engine'], info['version'], info['page_size'], info['load_seconds']])
            agg2 = lambda pop, m, arms: [wc.writerow([name, 'constellation', sz, pop, cl, a, int((m if cl == 'all' else m & (pc == cl)).sum()), int(L[m if cl == 'all' else m & (pc == cl)].sum()), int(v[m if cl == 'all' else m & (pc == cl)].sum())]) for cl in ['all', 'dbpage', 'wal', 'dbother'] for a, v in arms.items()]
            agg2('all', np.ones(len(L), bool), mk(0)); agg2('nonzero', ~zero, mk(0))
            for s in (1, 2, 3):
                idx = np.load(os.path.join(RES, 'cvpvsample-%s-%s-s%d.npy' % (name, tag, s))); m = np.ones(len(L), bool); m[idx] = False
                wp.writerow([name, 'constellation', sz, s, len(idx), int(L[idx].sum()), int(m.sum()), int(L[m].sum())]); agg2('pv-s%d' % s, m, mk(s))
    for f in (fl, fc, fs, fp): f.close()
if __name__ == '__main__': main(sys.argv[1], sys.argv[2:])
