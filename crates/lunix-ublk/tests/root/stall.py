#!/usr/bin/env python3
"""Join supervisor cycles (cycles.csv: phase,cycle,t_kill_ns,t_ready_ns,daemon_ms) with
heartbeat gaps (hb.csv) and print per-phase stats. Usage: stall.py <dir>..."""
import csv, sys, statistics as st

def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))] if v else float("nan")

def load_gaps(path):
    g = []
    for line in open(path):
        if line.startswith(("start_ns", "#")) or not line.strip():
            continue
        s, d = line.split(",")
        g.append((int(s), float(d)))
    return g

print("| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |")
print("|---|---|---|---|---|---|---|---|---|---|")
for d in sys.argv[1:]:
    rows = list(csv.DictReader(open(f"{d}/cycles.csv")))
    gaps = load_gaps(f"{d}/hb.csv")
    dm, kr, stall = [], [], []
    for r in rows:
        tk, tr = int(r["t_kill_ns"]), int(r["t_ready_ns"])
        if r["daemon_ms"] not in ("", "NA"):
            dm.append(float(r["daemon_ms"]))
        kr.append((tr - tk) / 1e6)
        w = [g for s, g in gaps if tk - 50_000_000 <= s <= tr + 2_000_000_000]
        stall.append(max(w) if w else 0.0)
    f = lambda v, p: f"{pct(v, p):.0f}"
    print(f"| {rows[0]['phase'] if rows else d} | {len(rows)} | {f(dm,50)} | {f(dm,99)} | {max(dm) if dm else float('nan'):.0f} | "
          f"{f(kr,50)} | {max(kr) if kr else float('nan'):.0f} | {f(stall,50)} | {f(stall,99)} | {max(stall) if stall else float('nan'):.0f} |")
