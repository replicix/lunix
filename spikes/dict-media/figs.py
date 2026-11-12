#!/usr/bin/env python3
"""figs.py <outdir> : SVG figures from raw/baseline.csv and raw/dictionaries.csv."""
import csv, sys
out = sys.argv[1]
B = [r for r in csv.DictReader(open(out + "/raw/baseline.csv")) if r["bucket"] == "all" and r["fmt"] != "ALL"]
D = list(csv.DictReader(open(out + "/raw/dictionaries.csv")))
def svg(path, w, h, body): open(path, "w").write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" font-family="sans-serif" font-size="11">'
    f'<rect width="{w}" height="{h}" fill="white"/>{body}</svg>')
# Fig 1: stored/original at zstd-3 (bar) and zstd-19 (tick), by bytes
B.sort(key=lambda r: -float(r["z3_ratio"])); n = len(B); h = 40 + 18 * n; x0 = 120; W = 420; s = ""
s += '<text x="10" y="18" font-weight="bold">Stored size / original (byte-weighted), zstd-3 bar, zstd-19 tick</text>'
for i, r in enumerate(B):
    y = 34 + 18 * i; z3 = float(r["z3_ratio"]); z19 = float(r["z19_ratio"])
    s += f'<text x="{x0-6}" y="{y+11}" text-anchor="end">{r["fmt"]} (n={r["files"]})</text><rect x="{x0}" y="{y}" width="{W*z3:.1f}" height="13" fill="#4c78a8"/>'
    s += f'<line x1="{x0+W*z19:.1f}" x2="{x0+W*z19:.1f}" y1="{y-1}" y2="{y+14}" stroke="#e45756" stroke-width="2"/><text x="{x0+W*z3+4:.1f}" y="{y+11}">{z3:.3f}</text>'
s += f'<line x1="{x0+W}" x2="{x0+W}" y1="28" y2="{h-6}" stroke="#999" stroke-dasharray="3"/>'
svg(out + "/fig-ratio-by-format.svg", 640, h, s)
# Fig 2: dictionary 16k byte gain by format and bucket (held out), with CI
rows = [r for r in D if r["comparison"] == "dict16k_vs_none_z3" and r["bucket"] in ("lt4K", "4K-64K", "64K-1M") and int(r["files"]) >= 10 and r["fmt"] != "ALL"]
rows.sort(key=lambda r: (r["fmt"], r["bucket"])); h = 50 + 16 * len(rows); x0 = 190; sc = 14; zero = x0 + 60
s = '<text x="10" y="18" font-weight="bold">Held-out byte gain of a 16 KiB zstd-3 dictionary (%, 95 % CI over sources)</text>'
for i, r in enumerate(rows):
    y = 36 + 16 * i; g = float(r["byte_gain_pct"]); lo = float(r["ci95_lo"] or g); hi = float(r["ci95_hi"] or g)
    col = "#54a24b" if g > 0 else "#e45756"
    s += f'<text x="{x0-6}" y="{y+10}" text-anchor="end">{r["fmt"]} {r["bucket"]} (n={r["files"]})</text>'
    s += f'<rect x="{min(zero, zero+g*sc):.1f}" y="{y}" width="{abs(g*sc):.1f}" height="12" fill="{col}"/><line x1="{zero+lo*sc:.1f}" x2="{zero+hi*sc:.1f}" y1="{y+6}" y2="{y+6}" stroke="black"/><text x="{zero+max(hi,g)*sc+5:.1f}" y="{y+10}">{g:.2f}</text>'
s += f'<line x1="{zero}" x2="{zero}" y1="28" y2="{h-6}" stroke="#333"/>'
svg(out + "/fig-dict-gain.svg", 700, h, s)
