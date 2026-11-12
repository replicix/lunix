#!/usr/bin/env python3
"""P0.2 tables and ADR-17 decision-rule evaluation.
Usage: analyze.py <out-dir>   (layout: <mode>/<work>/{main,qd1}/summary.csv, <mode>/hash-workersN/...)"""
import csv, os, sys

D = sys.argv[1]
MODES = ["copy", "user-copy", "batch"]

def load(mode, work):
    cells = {}
    for part in ("main", "qd1"):
        p = f"{D}/{mode}/{work}/{part}/summary.csv"
        if os.path.exists(p):
            for r in csv.DictReader(open(p)):
                cells[r["cell"]] = r
    return cells

def g(r, k):
    try:
        return float(r[k])
    except Exception:
        return float("nan")

def ratio(a, b):
    return a / b if b and b == b else float("nan")

for work in ("none", "hash", "hash+lz4"):
    data = {m: load(m, work) for m in MODES}
    cells = sorted(set().union(*[set(d) for d in data.values()]))
    if not cells:
        continue
    print(f"\n## work = {work}\n")
    print("| cell | " + " | ".join(f"{m} IOPS (min-max)" for m in MODES) + " | " + " | ".join(f"{m} lat p50/p99 us" for m in MODES) +
          " | " + " | ".join(f"{m} CPU us/IO" for m in MODES) + " | " + " | ".join(f"{m}/copy IOPS" for m in MODES[1:]) + " |")
    print("|" + "---|" * (1 + 3 + 3 + 3 + 2))
    for c in cells:
        row = [c]
        for m in MODES:
            r = data[m].get(c)
            row.append(f'{g(r,"iops_med"):.0f} ({g(r,"iops_min"):.0f}-{g(r,"iops_max"):.0f})' if r else "not run")
        for m in MODES:
            r = data[m].get(c)
            row.append(f'{g(r,"lat_p50_us"):.1f}/{g(r,"lat_p99_us"):.1f}' if r else "-")
        for m in MODES:
            r = data[m].get(c)
            row.append(f'{g(r,"daemon_cpu_us_per_io"):.2f}' if r else "-")
        base = data["copy"].get(c)
        for m in MODES[1:]:
            r = data[m].get(c)
            row.append(f'{ratio(g(r,"iops_med"), g(base,"iops_med")):.2f}' if r and base else "-")
        print("| " + " | ".join(row) + " |")

# ---- decision rules (work = hash, workers = 0)
H = {m: load(m, "hash") for m in MODES}
print("\n## Decision rules (work=hash, workers=0)\n")
def val(m, c, k):
    r = H[m].get(c)
    return g(r, k) if r else float("nan")

def evaluate(m, base):
    wr_cells = [f"64k_{rw}_qd{q}_nj8" for rw in ("write", "randwrite") for q in (32, 128)]
    iops_gain = {c: ratio(val(m, c, "iops_med"), val(base, c, "iops_med")) for c in wr_cells}
    cpu_gain = {c: ratio(val(m, c, "daemon_cpu_us_per_io"), val(base, c, "daemon_cpu_us_per_io")) for c in wr_cells}
    win = all(x >= 1.10 for x in iops_gain.values()) or all(x <= 0.90 for x in cpu_gain.values())
    q1 = "4k_randwrite_qd1_nj1"
    d50 = val(m, q1, "lat_p50_us") - val(base, q1, "lat_p50_us")
    d99 = ratio(val(m, q1, "lat_p99_us"), val(base, q1, "lat_p99_us"))
    worst = min((ratio(val(m, c, "iops_med"), val(base, c, "iops_med")), c) for c in H[m] if c in H[base])
    return iops_gain, cpu_gain, win, d50, d99, worst

for m, base in (("user-copy", "copy"), ("batch", "copy"), ("batch", "user-copy")):
    if not H[m] or not H[base]:
        continue
    ig, cg, win, d50, d99, worst = evaluate(m, base)
    print(f"### {m} vs {base}\n")
    print("| 64k write cell | IOPS ratio | daemon CPU/IO ratio |\n|---|---|---|")
    for c in ig:
        print(f"| {c} | {ig[c]:.2f} | {cg[c]:.2f} |")
    print(f"\n- 64k write clause (>=10% more IOPS or >=10% less CPU/IO at QD32/128): **{'met' if win else 'not met'}**")
    print(f"- 4k randwrite QD1 p50 delta: {d50:+.2f} us (allowed <= +2 us); p99 ratio {d99:.2f} (batch rule allows <= 1.10)")
    print(f"- worst IOPS ratio over all hash cells: {worst[0]:.2f} at {worst[1]} (user-copy rule: no cell below 0.95)\n")

print("\n## Headline cells with --workers 0 / 4 (work=hash)\n")
print("| mode | workers | cell | IOPS | lat p50/p99 us | CPU us/IO |\n|---|---|---|---|---|---|")
for m in MODES:
    for w in (0, 4):
        for part, c in (("main", "64k_write_qd32_nj8"), ("qd1", "4k_randwrite_qd1_nj1")):
            p = f"{D}/{m}/hash-workers{w}/{part}/summary.csv"
            if os.path.exists(p):
                for r in csv.DictReader(open(p)):
                    if r["cell"] == c:
                        print(f'| {m} | {w} | {c} | {g(r,"iops_med"):.0f} | {g(r,"lat_p50_us"):.1f}/{g(r,"lat_p99_us"):.1f} | {g(r,"daemon_cpu_us_per_io"):.2f} |')
