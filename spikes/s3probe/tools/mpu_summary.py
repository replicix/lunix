#!/usr/bin/env python3
"""Tables for `s3probe mpu`: python3 mpu_summary.py <p> <dir>... (dirs hold mpu-<p>-samples.csv, optionally -goodput.csv; samples are pooled)"""
import csv, sys, collections

def pct(v, p):
    v = sorted(v)
    return v[round((len(v) - 1) * p)]

p, dirs = sys.argv[1], sys.argv[2:]
rows = []
for d in dirs:
    rows += list(csv.DictReader(open(f"{d}/mpu-{p}-samples.csv")))
order = lambda c: (0 if c == "single" else 1 if c.startswith("par") else 2, c)
cells = collections.defaultdict(list)
for r in rows:
    if r["phase"] == "lat":
        cells[(int(r["size_mib"]), r["cell"])].append(r)
print("| size | form | n ok/n | total p50 | p90 | min | max | create p50 | parts wall p50 | slowest part p50 | Complete p50 |")
print("|---|---|---|---|---|---|---|---|---|---|---|")
for (sz, c) in sorted(cells, key=lambda k: (k[0], order(k[1]), k[1])):
    rs = cells[(sz, c)]
    ok = [r for r in rs if r["ok"] == "1"]
    f = lambda col: [float(r[col]) for r in ok]
    t = f("total_ms")
    if not t:
        continue
    m = lambda col: "-" if c == "single" else f"{pct(f(col), .5):.0f}"
    print(f"| {sz} MiB | {c} | {len(ok)}/{len(rs)} | {pct(t,.5):.0f} | {pct(t,.9):.0f} | {min(t):.0f} | {max(t):.0f} | "
          f"{m('create_ms')} | {m('parts_wall_ms')} | {m('max_part_ms')} | {m('complete_ms')} |")
try:
    g = []
    for d in dirs:
        try:
            g += list(csv.DictReader(open(f"{d}/mpu-{p}-goodput.csv")))
        except FileNotFoundError:
            pass
    print()
    print("| size | form | objects ok/n | wall ms | MB/s | MiB/s | requests | requests per GB |")
    print("|---|---|---|---|---|---|---|---|")
    for r in g:
        print(f"| {r['size_mib']} MiB | {r['cell']} | {r['ok']}/{r['objects']} | {float(r['wall_ms']):.0f} | {float(r['mb_s']):.0f} | {float(r['mb_s'])*1e6/2**20:.0f} | {r['reqs']} | {float(r['reqs_per_gb']):.0f} |")
    gp = [r for r in rows if r["phase"] == "gp" and r["ok"] == "1"]
    by = collections.defaultdict(list)
    for r in gp:
        by[(int(r["size_mib"]), r["cell"])].append(float(r["total_ms"]))
    print()
    print("Per-object time while 8 in flight (ms): " + "; ".join(
        f"{k[0]} MiB {k[1]}: p50 {pct(v,.5):.0f} p90 {pct(v,.9):.0f} max {max(v):.0f}" for k, v in sorted(by.items())))
except FileNotFoundError:
    pass
