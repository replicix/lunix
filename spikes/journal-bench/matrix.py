#!/usr/bin/env python3
"""Run the P0.5 cell matrix against one mounted directory and append JSON lines.
Usage: matrix.py <dir> <label> <out.jsonl>   (env: BIN, SECS, CPU, QUICK, FULL=0 for primary only)"""
import json, os, subprocess, sys

d, label, out = sys.argv[1:4]
BIN = os.environ.get("BIN", "./target/release/journal-bench")
SECS = os.environ.get("SECS", "3")
CPU = os.environ.get("CPU", "0")
QUICK = os.environ.get("QUICK") == "1"
PRIMARY_ONLY = os.environ.get("FULL", "1") == "0"
F = os.path.join(d, "journal.dat")
G = os.path.join(d, "growing.dat")

STRATS = [("group", b, 0) for b in (1, 4, 16, 64)] + [("group64", 64, w) for w in (50, 200)] + \
         [("linked", b, 0) for b in (1, 4, 16, 64)] + [("dsync", 1, 0), ("odsync", 1, 0)]

def cell(name, b, w, rec, rate, pre=8, bg=False, file=None):
    strat = {"group64": "group"}.get(name, name)
    args = [BIN, "--file", file or F, "--strategy", strat, "--rec", str(rec), "--batch", str(b), "--window-us", str(w),
            "--rate", str(rate), "--secs", SECS, "--prealloc-gib", str(pre), "--cpu", CPU, "--label", label]
    if bg:
        args.append("--bg-read")
    r = subprocess.run(["timeout", "-k", "5", "120"] + args, capture_output=True, text=True)
    if r.returncode != 0:
        print("FAIL", args, r.stderr[-300:], file=sys.stderr)
        return
    j = json.loads(r.stdout.strip().splitlines()[-1])
    j["name"] = name
    j["offered"] = rate
    j["growing"] = pre == 0
    j["saturated"] = rate > 0 and j["rec_per_s"] < 0.95 * rate
    with open(out, "a") as f:
        f.write(json.dumps(j) + "\n")

cells = []
for rec in (4096, 65536):
    for rate in (50000, 0):
        for (n, b, w) in STRATS:
            cells.append(dict(name=n, b=b, w=w, rec=rec, rate=rate))
if not PRIMARY_ONLY:
    for rec in (16384, 262144):
        for (n, b, w) in STRATS:
            cells.append(dict(name=n, b=b, w=w, rec=rec, rate=0))
    for rate in (10000, 200000):
        for (n, b, w) in STRATS:
            cells.append(dict(name=n, b=b, w=w, rec=4096, rate=rate))
    for rec in (4096, 65536):
        for rate in (50000, 0):
            for (n, b, w) in (("group", 16, 0), ("linked", 4, 0), ("dsync", 1, 0)):
                cells.append(dict(name=n, b=b, w=w, rec=rec, rate=rate, pre=0, file=G))
            for (n, b, w) in (("group", 16, 0), ("linked", 4, 0)):
                cells.append(dict(name=n, b=b, w=w, rec=rec, rate=rate, bg=True))
if QUICK:
    cells = [c for c in cells if c["rec"] == 4096 and c["rate"] == 50000 and c["name"] in ("group", "linked", "dsync")][:6]
for c in cells:
    cell(**c)
    if c.get("file") == G and os.path.exists(G):
        os.unlink(G)
