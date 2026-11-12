#!/usr/bin/env python3
"""Pool per-sample CSVs (series,size_bytes,ms) and print n/p50/p90/p99/max per series and size,
plus the least-squares fixed cost and per-MiB slope of the put_size series.
usage: pool.py [-I] LABEL=file.csv[,file.csv...] ..."""
import csv, sys

def pct(v, p):
    v = sorted(v)
    return v[round((len(v) - 1) * p)]

def fit(pts):
    n = len(pts); sx = sum(x for x, _ in pts); sy = sum(y for _, y in pts)
    sxx = sum(x * x for x, _ in pts); sxy = sum(x * y for x, y in pts)
    b = (n * sxy - sx * sy) / (n * sxx - sx * sx)
    return (sy - b * sx) / n, b

for arg in sys.argv[1:]:
    label, files = arg.split("=", 1)
    data = {}
    for f in files.split(","):
        with open(f) as fh:
            for r in csv.DictReader(fh):
                data.setdefault((r["series"], int(r["size_bytes"])), []).append(float(r["ms"]))
    print(f"== {label}")
    for (s, size), v in sorted(data.items()):
        print(f"{s:34} {size:>9} n={len(v):4} p50={pct(v,.5):8.1f} p90={pct(v,.9):8.1f} p99={pct(v,.99):8.1f} max={max(v):8.1f}")
    for q in (.5, .99):
        pts = [(size / 1048576, pct(v, q)) for (s, size), v in sorted(data.items()) if s == "put_size"]
        if len(pts) > 2:
            a, b = fit(pts); big = [p for p in pts if p[0] >= 1]; a2, b2 = fit(big)
            print(f"fit p{int(q*100)}: fixed={a:.1f} ms slope={b:.2f} ms/MiB | >=1 MiB: fixed={a2:.1f} slope={b2:.2f} ms/MiB")
