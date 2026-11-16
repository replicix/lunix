#!/usr/bin/env python3
"""Latency percentiles of a pgbench run from its per-transaction logs (`pgbench -l --log-prefix=P`).

  pglat.py PREFIX [SAMPLING_RATE] > pglat.json

Reads PREFIX.<pid>[.<n>] files (columns: client_id transaction_no time script_no time_epoch time_us; `time` is the
transaction latency in microseconds), prints p50/p90/p99/p99.9/max in milliseconds and the sample count.
"""
import glob
import json
import sys


def pct(v, p):
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]


lat = []
for f in glob.glob(sys.argv[1] + ".*") + glob.glob(sys.argv[1]):
    if f.endswith((".json", ".txt", ".out")):
        continue
    for line in open(f, errors="replace"):
        p = line.split()
        if len(p) >= 6 and p[2].isdigit():
            lat.append(int(p[2]))
lat.sort()
out = {"samples": len(lat), "sampling_rate": float(sys.argv[2]) if len(sys.argv) > 2 else None}
if lat:
    out.update({f"p{k}_ms": pct(lat, k) / 1000 for k in (50, 90, 99, 99.9)})
    out["max_ms"] = lat[-1] / 1000
json.dump(out, sys.stdout)
print()
