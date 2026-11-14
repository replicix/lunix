#!/usr/bin/env python3
"""Aggregates out/timing.tsv of timing_run.py into raw/timing.csv (all cells, comparable columns) and raw/timing-ratios.csv (throughput of
every arm relative to the plain-level comparators, per cell and operation; the 0.8x hard cap of section 1.1 is marked; delta_cpu = cost(A) - cost(B), delta_cores = max(0, delta_cpu) x ingest target, 5 GB/s on Lunix cells, 1 GB/s on Constellation cells).
usage: timing_agg.py <timing.tsv> <timing.csv> <timing-ratios.csv>
Throughput ratio of arm A against comparator B = cost(B) / cost(A) (CPU-s per GB, medians).  Comparators: z1 and z3 of the same cell (behind
the probe on Lunix cells, plain on Constellation cells); `cap_fail` is 1 if the ratio is below 0.8.  `ft` compress cost already includes
the selector pass; for decompress the selector is not part of the read path."""
import csv, sys, collections
src, out, outr = sys.argv[1:4]
rows = list(csv.DictReader(open(src), delimiter='\t'))
cols = ['corpus', 'view', 'chunk', 'arm', 'level', 'op', 'median', 'min', 'max', 'flag', 'load1', 'others_busy_cores', 'sample_bytes', 'objects', 'stored_bytes', 'raw_objects', 'note']
with open(out, 'w', newline='') as f:
    w = csv.writer(f); w.writerow(['cell'] + cols)
    for r in rows: w.writerow([r['cell']] + [r[c] for c in cols])
cost = {(r['cell'], r['arm'], r['level'], r['op']): float(r['median']) for r in rows}
target = {r['cell']: (5.0 if r['view'].startswith('lunix') else 1.0) for r in rows}      # ingest targets of section 1.1 (GB/s per node)
cells = sorted({r['cell'] for r in rows}); res = []
for cell in cells:
    arms = sorted({(r['arm'], r['level']) for r in rows if r['cell'] == cell and r['arm'] != 'ft-select'}, key=lambda x: (x[0], int(x[1])))
    comps = [c for c in ('z1', 'z3') if (cell, c, c[1], 'compress') in cost]
    for arm, lv in arms:
        for op in ('compress', 'decompress'):
            a = cost.get((cell, arm, lv, op))
            if a is None: continue
            for c in comps:
                b = cost[(cell, c, c[1], op)]; ratio = b / a
                dcpu = a - b; res.append([cell, arm, lv, op, '%.4f' % a, c, '%.4f' % b, '%.3f' % ratio, int(ratio < 0.8), '%.4f' % dcpu, '%.3f' % (max(0.0, dcpu) * target[cell])])
with open(outr, 'w', newline='') as f:
    w = csv.writer(f); w.writerow(['cell', 'arm', 'level', 'op', 'cost_s_per_gb', 'comparator', 'comparator_cost_s_per_gb', 'throughput_ratio', 'cap_fail', 'delta_cpu_s_per_gb', 'delta_cores_at_target']); w.writerows(res)
print(len(rows), 'rows,', len(res), 'ratio rows')
