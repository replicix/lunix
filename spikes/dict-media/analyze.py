#!/usr/bin/env python3
"""analyze.py <measure.csv.gz> <outdir> : summary CSVs (raw/) and SVG figures."""
import sys, os, gzip, csv, math, random
import numpy as np
from scipy.stats import wilcoxon
src, out = sys.argv[1], sys.argv[2]; os.makedirs(out + "/raw", exist_ok=True)
R = list(csv.DictReader(gzip.open(src, "rt")))
def f(r, k): return float(r[k]) if r.get(k) not in (None, "") else None
BK = ["lt4K", "4K-64K", "64K-1M", "1M-4M", "gt4M"]
rng = np.random.default_rng(1)
groups = {}
for r in R: groups.setdefault((r["fmt"], r["bucket"]), []).append(r)
for fm in {r["fmt"] for r in R}: groups[(fm, "all")] = [r for r in R if r["fmt"] == fm]
for r in R: groups.setdefault((r["fmt"] + "/" + r["set"], "all"), []).append(r)
groups[("ALL", "all")] = list(R)
for b in BK: groups[("ALL", b)] = [r for r in R if r["bucket"] == b]
def key(g): return (g[0] == "ALL", g[0], (BK + ["all"]).index(g[1]))

def boot(rows, num, den, n=1000):
    """95 % CI of 1 - sum(num)/sum(den) with sources resampled."""
    bs = {}
    for r in rows: bs.setdefault(r["source"], [0.0, 0.0]); bs[r["source"]][0] += num(r); bs[r["source"]][1] += den(r)
    a = np.array(list(bs.values()))
    if len(a) < 3: return ("", "")
    v = []
    for _ in range(n):
        s = a[rng.integers(0, len(a), len(a))].sum(0); v.append(1 - s[0] / s[1] if s[1] else 0)
    return (round(100 * np.percentile(v, 2.5), 2), round(100 * np.percentile(v, 97.5), 2))

# ---- baseline
with open(out + "/raw/baseline.csv", "w", newline="") as o:
    w = csv.writer(o)
    w.writerow(["fmt", "bucket", "files", "files_origin_real", "sources", "bytes_measured"] +
               [f"z{l}_ratio" for l in (1, 3, 7, 19)] + ["z3_ratio_ci_lo", "z3_ratio_ci_hi", "z3_mean_file_ratio", "z3_raw_obj_share", "z3_raw_byte_share",
                "z3_files_all_raw", "z19_raw_obj_share", "l16_probe_ratio", "l16_noprobe_ratio", "l16_probe_raw_chunk_share", "l16_noprobe_raw_chunk_share",
                "l64_probe_ratio", "l64_noprobe_ratio", "l64_probe_raw_chunk_share", "l64_noprobe_raw_chunk_share", "probe_forfeit_pct_of_bytes"])
    for g in sorted(groups, key=key):
        rs = groups[g]; cov = sum(f(r, "cov") for r in rs)
        if not rs: continue
        row = [g[0], g[1], len(rs), sum(r["origin"] == "real" for r in rs), len({r["source"] for r in rs}), int(cov)]
        for l in (1, 3, 7, 19): row.append(round(sum(f(r, f"z{l}") for r in rs) / cov, 4))
        lo, hi = boot(rs, lambda r: f(r, "z3"), lambda r: f(r, "cov")); row += [(100 - hi) / 100 if hi != "" else "", (100 - lo) / 100 if lo != "" else ""]
        row.append(round(float(np.mean([f(r, "z3") / f(r, "cov") for r in rs])), 4))
        nobj = sum(f(r, "nobj") for r in rs)
        row.append(round(sum(f(r, "z3_raw") for r in rs) / nobj, 4))
        # raw bytes: bytes of files whose all objects raw, plus approx for partial (use proportion)
        row.append(round(sum(f(r, "cov") * f(r, "z3_raw") / f(r, "nobj") for r in rs) / cov, 4))
        row.append(round(sum(f(r, "z3_raw") == f(r, "nobj") for r in rs) / len(rs), 4))
        row.append(round(sum(f(r, "z19_raw") for r in rs) / nobj, 4))
        for nm in ("l16", "l64"):
            lin = sum(f(r, nm + "_in") for r in rs); nn = sum(f(r, nm + "_n") for r in rs)
            row += [round(sum(f(r, nm + "_probe") for r in rs) / lin, 4), round(sum(f(r, nm + "_noprobe") for r in rs) / lin, 4),
                    round(sum(f(r, nm + "_probe_raw") for r in rs) / nn, 4), round(sum(f(r, nm + "_noprobe_raw") for r in rs) / nn, 4)]
        lin = sum(f(r, "l16_in") for r in rs)
        row.append(round(100 * (sum(f(r, "l16_probe") for r in rs) - sum(f(r, "l16_noprobe") for r in rs)) / lin, 3))
        w.writerow(row)

# ---- dictionaries and paired tests
tests = []
def cmp(g, label, a, b, rs):
    rs = [r for r in rs if r.get(a) not in (None, "") and r.get(b) not in (None, "")]
    if len(rs) < 5: return None
    A = np.array([f(r, a) for r in rs]); B = np.array([f(r, b) for r in rs]); d = A - B
    cmpd = np.array([f(r, "cov") if "cov" in r else 1 for r in rs])
    byte_gain = 100 * (1 - B.sum() / A.sum()); mean_gain = 100 * np.mean(1 - B / np.maximum(A, 1))
    helped = np.mean((A - B) / np.maximum(A, 1) > 0.01); hurt = np.mean((B - A) / np.maximum(A, 1) > 0.01)
    p = ""
    nz = d[d != 0]
    if len(nz) >= 10:
        try: p = wilcoxon(nz).pvalue
        except Exception: p = ""
    lo, hi = boot(rs, lambda r: f(r, b), lambda r: f(r, a))
    row = dict(fmt=g[0], bucket=g[1], comparison=label, files=len(rs), sources=len({r["source"] for r in rs}), base_bytes=int(A.sum()), new_bytes=int(B.sum()),
               byte_gain_pct=round(byte_gain, 2), ci95_lo=lo, ci95_hi=hi, mean_file_gain_pct=round(mean_gain, 2), share_files_gain_gt1pct=round(helped, 3),
               share_files_loss_gt1pct=round(hurt, 3), p=p)
    tests.append(row); return row
for g in sorted(groups, key=key):
    rs = groups[g]
    cmp(g, "dict16k_vs_none_z3", "z3", "d3_16k", rs); cmp(g, "dict112k_vs_none_z3", "z3", "d3_112k", rs)
    cmp(g, "lunix16_dict16k_vs_none_probe", "l16_probe", "l16_dict_probe", rs)
    cmp(g, "z19_vs_z3", "z3", "z19", rs)
# Holm over rows with a p-value, within comparison families
for fam in {t["comparison"] for t in tests}:
    ps = sorted([(t["p"], i) for i, t in enumerate(tests) if t["comparison"] == fam and t["p"] != ""])
    m = len(ps); run = 0
    for rank, (p, i) in enumerate(ps):
        run = max(run, min(1, (m - rank) * p)); tests[i]["p_holm"] = run
with open(out + "/raw/dictionaries.csv", "w", newline="") as o:
    cols = ["fmt", "bucket", "comparison", "files", "sources", "base_bytes", "new_bytes", "byte_gain_pct", "ci95_lo", "ci95_hi", "mean_file_gain_pct", "share_files_gain_gt1pct", "share_files_loss_gt1pct", "p", "p_holm"]
    w = csv.DictWriter(o, cols, extrasaction="ignore"); w.writeheader()
    for t in tests: t.setdefault("p_holm", ""); w.writerow({k: (f"{v:.3g}" if k.startswith("p") and v != "" else v) for k, v in t.items()})
print("ok", len(tests))
