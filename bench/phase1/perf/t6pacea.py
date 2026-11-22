#!/usr/bin/env python3
"""P1' A/B (T6): per-repetition metrics and per-cell tables of the runs under results/t6pacea/<pass>-<arm>-<chunk>/.

  t6pacea.py RESULTS_DIR [--json OUT.json] > tables.md

Each run directory holds the output of tests/perf/partA.sh (raw/fio-*.json, raw/<cell>/{lat_clat.*.log,series.jsonl,
status-before.json,status-end.json}, lunixd.1.log). Max completion is the largest per-second maximum of the fio clat logs.
"""
import glob
import json
import math
import os
import re
import statistics as st
import sys


def jl(p):
    try:
        return json.load(open(p))
    except (OSError, ValueError):
        return None


def pct(v, p):
    v = sorted(v)
    if not v:
        return None
    k = (len(v) - 1) * p / 100
    lo, hi = math.floor(k), math.ceil(k)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def kv(line):
    return {k: v for k, v in re.findall(r'(\w+)=(\S+)', line)}


def load_log(path):
    ev = []
    try:
        lines = open(path, errors='replace').read().splitlines()
    except OSError:
        return ev
    for line in lines:
        m = re.match(r'(\d{13}) (\w+)\s+(\S+): (.*)', line)
        if m:
            ev.append((int(m.group(1)), m.group(3), m.group(4)))
    return ev


def cell(run, ck, name, rep, log):
    cn = f"partA_c{ck}k_q4_{name}_rep{rep}"
    f = jl(os.path.join(run, "raw", f"fio-{cn}.json"))
    if not f:
        return None
    d = os.path.join(run, "raw", cn)
    j = f["jobs"][0]["write"]
    pc = j["clat_ns"].get("percentile", {})
    m = dict(cell=name, rep=rep, iops=j["iops"], p999_s=pc.get("99.900000", 0) / 1e9,
             err=sum(int(x.get("error", 0) or 0) for x in f["jobs"]))
    mx = 0.0
    for lf in glob.glob(os.path.join(d, "lat_clat.*.log")):
        for line in open(lf):
            p = line.split(",")
            if len(p) >= 2:
                mx = max(mx, float(p[1]) / 1e9)
    m["max_s"] = mx
    b, e = jl(os.path.join(d, "status-before.json")), jl(os.path.join(d, "status-end.json"))
    tb, te = (b or {}).get("throttle", {}), (e or {}).get("throttle", {})
    for k in ("stops", "throttled_ms", "delayed_writes", "delayed_ms", "delay_capped", "fulls", "enospc"):
        m[k] = te.get(k, 0) - tb.get(k, 0)
    wb, we = (b or {}).get("write_path", {}), (e or {}).get("write_path", {})
    for k in ("store_above_soft_ms", "store_delayed_writes", "store_delayed_ms"):
        m[k] = we.get(k, 0) - wb.get(k, 0)
    series = []
    try:
        for line in open(os.path.join(d, "series.jsonl")):
            try:
                series.append(json.loads(line))
            except ValueError:
                pass
    except OSError:
        pass
    w = [(s.get("status") or {}).get("write_path") or {} for s in series]

    def mxs(k):
        v = [x[k] for x in w if k in x]
        return max(v) if v else None
    m["factor_max"], m["factor_biased_max"], m["uncut_max"] = mxs("store_factor_milli"), mxs("store_factor_biased_milli"), mxs("store_journal_uncut")
    hp = [x["store_h_proj"] for x in w if "store_h_proj" in x]
    m["h_proj_min"] = min(hp) if hp else None
    m["paced_above_soft_samples"] = sum(1 for x in w if x.get("store_paced_above_soft"))
    end = f.get("timestamp_ms", 0)
    rt = 60 if name == "w4k" else 30
    lo, hi = end - (rt + 8) * 1000, end + 500
    on = stop = None
    cuts, above = [], 0
    for ts, mod, msg in log:
        if not (lo <= ts <= hi):
            continue
        if msg.startswith("write throttle on") and on is None:
            on = ts
        if msg.startswith("writes wait for space") and stop is None and on is not None and ts > on:
            stop = ts
        if msg.startswith("store factor of the cut"):
            k = kv(msg)
            cuts.append((int(k["a_pred_milli"]), int(k["a_real_milli"]), int(k["a_hat_milli"]), int(k["journal_bytes"])))
        if msg.startswith("store pace above the soft mark") and "ended" not in msg:
            above += 1
    m["dwell_s"] = (stop - on) / 1000 if on and stop else None
    m["throttle_on"] = on is not None
    m["above_soft_events"] = above
    m["cuts"] = cuts
    return m


def collect(root):
    runs = {}
    for run in sorted(glob.glob(os.path.join(root, "p*-*-*"))):
        base = os.path.basename(run)
        ps, arm, ck = base.split("-")
        ck = int(ck) // 1024
        log = load_log(os.path.join(run, "lunixd.1.log"))
        fsck = jl(os.path.join(run, "raw", f"fsck-parta-c{ck}k-q4.json"))
        rp = os.path.join(run, "result.txt")
        res = open(rp).read().strip() if os.path.exists(rp) else "?"
        cells = []
        for name in ("w4k", "seqw"):
            for rep in range(1, 6):
                c = cell(run, ck, name, rep, log)
                if c:
                    cells.append(c)
        runs[base] = dict(pass_=ps, arm=arm, ck=ck, cells=cells, fsck_ok=fsck is not None, result=res,
                          eio_log=sum(1 for _, _, msg in log if "EIO" in msg or "ENOSPC" in msg))
    return runs


def f1(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def summary(vals, d=1):
    vals = [v for v in vals if v is not None]
    if not vals:
        return "-"
    return f"{f1(st.median(vals), d)} / {f1(pct(vals, 90), d)} / {f1(max(vals), d)}"


def main():
    root = sys.argv[1]
    runs = collect(root)
    if "--json" in sys.argv:
        json.dump(runs, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)
    out = []
    for cellname in ("w4k", "seqw"):
        for ck in (64, 16):
            out.append(f"\n### {ck} KiB {cellname}\n")
            out.append("| arm | reps | stops (reps with a stop) | max completion s (med / p90 / worst) | IOPS (med / p90 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |")
            out.append("|---|---|---|---|---|---|---|---|---|")
            for arm in ("off", "on"):
                cs = [c for r in runs.values() if r["arm"] == arm and r["ck"] == ck for c in r["cells"] if c["cell"] == cellname]
                if not cs:
                    continue
                nstop = sum(1 for c in cs if c["stops"] > 0)
                iops = [c["iops"] for c in cs]
                out.append(f"| {arm.upper()} | {len(cs)} | {sum(c['stops'] for c in cs)} ({nstop} of {len(cs)}) | {summary([c['max_s'] for c in cs])} | "
                           f"{f1(st.median(iops),0)} / {f1(pct(iops,90),0)} / {f1(min(iops),0)} | {summary([c['p999_s'] for c in cs],2)} | "
                           f"{f1(st.median([c['throttled_ms'] for c in cs]),0)} | {f1(st.median([c['delayed_writes'] for c in cs]),0)} | {max(c['delay_capped'] for c in cs)} |")
    out.append("\n### per repetition (w4k; seqw in the json)\n")
    out.append("| run | rep | max s | stops | IOPS | p99.9 s | throttled_ms | delayed_writes | capped | dwell s | above-soft events | above_soft_ms | store_delayed_ms | factor max (milli) | biased max | uncut max MiB | cuts |")
    out.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for name, r in runs.items():
        for c in r["cells"]:
            if c["cell"] != "w4k":
                continue
            out.append(f"| {name} | {c['rep']} | {c['max_s']:.1f} | {c['stops']} | {c['iops']:.0f} | {c['p999_s']:.2f} | {c['throttled_ms']} | {c['delayed_writes']} | {c['delay_capped']} | {f1(c['dwell_s'])} | {c['above_soft_events']} | {c['store_above_soft_ms']} | {c['store_delayed_ms']} | {c['factor_max']} | {c['factor_biased_max']} | {f1((c['uncut_max'] or 0)/2**20,0)} | {len(c['cuts'])} |")
    out.append("\n### A_hat error per cut (real - pred, milli; pooled over the w4k cells of all runs of the arm and chunk)\n")
    out.append("| arm | chunk | cuts | pred median | real median | error p10 / p50 / p90 / p99 | under-predicted (real > pred) | under by more than 20 % |")
    out.append("|---|---|---|---|---|---|---|---|")
    for arm in ("off", "on"):
        for ck in (64, 16):
            cuts = [x for r in runs.values() if r["arm"] == arm and r["ck"] == ck for c in r["cells"] if c["cell"] == "w4k" for x in c["cuts"]]
            if not cuts:
                continue
            err = [r_ - p for p, r_, h, j in cuts]
            under = sum(1 for e_ in err if e_ > 0)
            big = sum(1 for p, r_, h, j in cuts if r_ > 1.2 * p)
            out.append(f"| {arm.upper()} | {ck} | {len(cuts)} | {st.median([x[0] for x in cuts])} | {st.median([x[1] for x in cuts])} | "
                       f"{f1(pct(err,10),0)} / {f1(pct(err,50),0)} / {f1(pct(err,90),0)} / {f1(pct(err,99),0)} | {under} ({100*under/len(cuts):.0f} %) | {big} ({100*big/len(cuts):.0f} %) |")
    out.append("\n### A_hat error, cuts of 128 MiB of journal or more (real - pred, milli)\n")
    out.append("| arm | chunk | cuts | pred median | real median | error p10 / p50 / p90 | relative error p10 / p50 / p90 (%) |")
    out.append("|---|---|---|---|---|---|---|")
    for arm in ("off", "on"):
        for ck in (64, 16):
            cuts = [x for r in runs.values() if r["arm"] == arm and r["ck"] == ck for c in r["cells"] if c["cell"] == "w4k" for x in c["cuts"] if x[3] >= 128 << 20]
            if not cuts:
                continue
            err = [r_ - p for p, r_, h, j in cuts]
            rel = [100 * (r_ - p) / r_ for p, r_, h, j in cuts if r_ > 0]
            out.append(f"| {arm.upper()} | {ck} | {len(cuts)} | {st.median([x[0] for x in cuts])} | {st.median([x[1] for x in cuts])} | "
                       f"{f1(pct(err,10),0)} / {f1(pct(err,50),0)} / {f1(pct(err,90),0)} | {f1(pct(rel,10),0)} / {f1(pct(rel,50),0)} / {f1(pct(rel,90),0)} |")
    out.append("\n### per block of five repetitions (one daemon, one fresh volume)\n")
    out.append("| run | w4k reps with a stop | stops | max completion median s | worst s | IOPS median |")
    out.append("|---|---|---|---|---|---|")
    for name, r in runs.items():
        cs = [c for c in r["cells"] if c["cell"] == "w4k"]
        out.append(f"| {name} | {sum(1 for c in cs if c['stops'] > 0)} of {len(cs)} | {sum(c['stops'] for c in cs)} | {f1(st.median([c['max_s'] for c in cs]))} | {f1(max(c['max_s'] for c in cs))} | {f1(st.median([c['iops'] for c in cs]),0)} |")
    out.append("\n### integrity\n")
    out.append("| run | result.txt (0 = pass) | fsck json present | EIO/ENOSPC lines in daemon log | fio errors (all cells) | enospc+fulls deltas |")
    out.append("|---|---|---|---|---|---|")
    for name, r in runs.items():
        out.append(f"| {name} | {r['result']} | {r['fsck_ok']} | {r['eio_log']} | {sum(c['err'] for c in r['cells'])} | {sum(c['enospc'] + c['fulls'] for c in r['cells'])} |")
    print("\n".join(out))


if __name__ == "__main__":
    main()
