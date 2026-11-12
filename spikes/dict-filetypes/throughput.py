#!/usr/bin/env python3 -I
"""Per-core compression/decompression throughput and dictionary cost with tp (C, libzstd).  usage: throughput.py OUT_DIR
Pin to ONE idle core: the script re-executes tp under `taskset -c $TPCORE` (default 8).  n=7 timed passes after one warm-up pass."""
import sys as _s, os as _o; _s.path.insert(0, _o.path.dirname(_o.path.abspath(__file__)))
import os, sys, random, subprocess, csv, time
def load1(): return float(open('/proc/loadavg').read().split()[0])
def quiet(maxload=2.0, wait=900):
    t0 = time.time()
    while load1() > maxload and time.time() - t0 < wait: time.sleep(10)
    return load1()
import taxonomy as T, common as C
out = sys.argv[1]; core = os.environ.get('TPCORE', '8'); tp = os.path.join(C.WORK, 'tpbin')
rows = C.load_manifest(); fold_of = C.assign_folds(rows); rng = random.Random(11)
test = [r for r in rows if fold_of[r['project']] == 0]
def pick(pred, n, types=None, pool=None):
    c = [r for r in (pool or test) if pred(r['size']) and (types is None or r['type'] in types)]; rng.shuffle(c); return c[:n]
sets = {
 'text 1-16 KiB (all text types)': pick(lambda s: 1024 <= s < 16384, 2000, T.TEXT_TYPES),
 'text 16-256 KiB (all text types)': pick(lambda s: 16384 <= s < 262144, 300, T.TEXT_TYPES),
 'objects >= 1 MiB (cut into 4 MiB chunks)': pick(lambda s: s >= 1 << 20, 8, None, [r for r in rows if r['type'] not in T.CONTROL_TYPES]),
 'python 1-16 KiB': pick(lambda s: 1024 <= s < 16384, 800, ['python']),
 'json 1-16 KiB': pick(lambda s: 1024 <= s < 16384, 800, ['json']),
}
dictof = {'text 1-16 KiB (all text types)': 'generic_text', 'text 16-256 KiB (all text types)': 'generic_text', 'objects >= 1 MiB (cut into 4 MiB chunks)': 'generic_all', 'python 1-16 KiB': 'type_python', 'json 1-16 KiB': 'type_json'}
os.makedirs(os.path.join(C.WORK, 'tp'), exist_ok=True); res = []
for name, rs in sets.items():
    if len(rs) < 3: continue
    lst = os.path.join(C.WORK, 'tp', name.split()[0] + str(len(rs)) + '.lst'); open(lst, 'w').write('\n'.join(r['path'] for r in rs))
    dp = os.path.join(C.WORK, 'dicts', 'C', 'f0', dictof[name] + '.zdict')
    for L in (1, 3, 9, 19):
        for d in ('-', dp):
            if d != '-' and not os.path.exists(d): continue
            reps = 7 if not (L == 19 and '>= 1 MiB' in name) else 5
            l0 = quiet()
            o = subprocess.run(['taskset', '-c', core, tp, str(L), d, lst, str(reps)], capture_output=True, text=True, timeout=3600)
            if o.returncode: sys.stderr.write(o.stderr); continue
            f = o.stdout.strip().split(',')
            res.append(dict(set=name, objects=f[2], level=L, mode=f[1], bytes_in=f[3], bytes_out=f[4], ratio=round(int(f[3]) / int(f[4]), 3), comp_MBps_median=f[5], comp_MBps_slowest_rep=f[6], comp_MBps_fastest_rep=f[7],
                            decomp_MBps_median=f[8], decomp_MBps_slowest_rep=f[9], decomp_MBps_fastest_rep=f[10], cdict_prepare_ms=f[11], ddict_prepare_ms=f[12], cdict_bytes=f[13], ddict_bytes=f[14], dict=os.path.basename(d) if d != '-' else '-', reps=reps, load1_before=l0, load1_after=load1()))
with open(os.path.join(out, 'throughput.csv'), 'w', newline='') as fh:
    w = csv.DictWriter(fh, list(res[0])); w.writeheader(); w.writerows(res)
