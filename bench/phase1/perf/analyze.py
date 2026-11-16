#!/usr/bin/env python3
"""M16 Part A tables and the DESIGN §20 verdicts.

  analyze.py OUT_DIR [OUT_DIR ...] [--cpus N] [--json verdict.json] > tables.md

Reads results.jsonl of tests/perf/partA.sh. Per chunk size, queue count and cell: median, min and max over the
repetitions of IOPS (bandwidth for the 1M cells), p99, daemon CPU per I/O; a cell is flagged noisy when max/min > 1.2
(CONVENTIONS). A row passes when the better of the two queue counts meets its DESIGN §20 target (the better one is the
one with the higher median; for r4k the p99 target is judged at the same queue count). No threshold is changed. The
oversubscription (queue threads + fio jobs against the CPUs of the host) is printed next to each verdict. For every row
that misses its target the evidence the plan asks for is printed: the BASELINE ceiling, the null_blk and journal-file-
system references of the same session, the daemon CPU per I/O, the busiest thread's share of a core and the thread split.
"""
import argparse
import json
import os
import statistics as st
import sys

TARGETS = {  # cell: (metric, target, p99 limit us)
    "r4k": ("iops", 500_000, 150.0),
    "w4k": ("iops", 300_000, None),
    "seqw": ("bw", 3.0e9, None),
    "seqr": ("bw", 5.0e9, None),
}
BASELINE = {  # ceilings measured in phase 0 (bench/BASELINE.md), for the shortfall evidence
    "r4k": "ublk-null 978 K IOPS on one queue, 3.83 M on four queue threads (null_blk 4.9 M)",
    "w4k": "ublk-null 3.6 M IOPS; with BLAKE3 of every write 2.13 M, with LZ4 1.69 M; journal prototype, one thread: 36.5 K rec/s on the real disk (flush-bound)",
    "seqw": "hash plus LZ4 inline at 64 KiB: about 15.7 GB/s on four queue threads; AES-256-GCM 17-19 GB/s per core; zstd-1 and workers not in the spike",
    "seqr": "64k random read touching every byte: 48.8 GB/s (copy), 81 GB/s (USER_COPY); 1M reads through the copy path 25 K IOPS (158 us per I/O) in the spike",
}


def rows_of(dirs):
    rows = []
    for d in dirs:
        p = os.path.join(d, "results.jsonl")
        if os.path.exists(p):
            for line in open(p):
                if line.strip():
                    r = json.loads(line)
                    r["_dir"] = d
                    rows.append(r)
    return rows


def num(r, k):
    try:
        return float(r.get(k, ""))
    except (TypeError, ValueError):
        return None


def stat(vals):
    v = [x for x in vals if x is not None]
    if not v:
        return None
    return {"n": len(v), "med": st.median(v), "min": min(v), "max": max(v), "noisy": (max(v) / min(v) > 1.2) if min(v) > 0 else True}


def fmt(x, unit=""):
    if x is None:
        return "n/a"
    if unit == "GB/s":
        return f"{x / 1e9:.2f}"
    if abs(x) >= 1000:
        return f"{x:,.0f}"
    return f"{x:.1f}"


def fio_json(path):
    try:
        t = open(path).read()
        return json.loads(t[t.index("{"):])
    except (OSError, ValueError):
        return None


def refs(dirs):
    out = {}
    for d in dirs:
        j = fio_json(os.path.join(d, "raw", "fio-nullblk-ref.json"))
        if j:
            out["nullblk"] = {x["jobname"]: (x["read"]["iops"] + x["write"]["iops"], x["read"]["bw_bytes"] + x["write"]["bw_bytes"]) for x in j["jobs"]}
        j = fio_json(os.path.join(d, "raw", "fio-journal-raw.json"))
        if j:
            out["journal"] = {x["jobname"]: (x["write"]["iops"], x["write"]["bw_bytes"]) for x in j["jobs"]}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--cpus", type=int, default=16)
    ap.add_argument("--json")
    a = ap.parse_args()
    rows = [r for r in rows_of(a.dirs) if r["cell"].startswith("partA/") and r.get("cell_name")]
    ref = refs(a.dirs)
    out, verdict = [], {}
    chunks = sorted({int(r["chunk_k"]) for r in rows})
    for ck in chunks:
        out.append(f"\n## chunk {ck} KiB\n")
        out.append("| cell | queues | n | IOPS median (min-max) | GB/s median | p99 us median (max) | daemon CPU us/IO | noisy | rss after (MiB) | hwm (MiB) |")
        out.append("|---|---|---|---|---|---|---|---|---|---|")
        agg = {}
        for r in rows:
            if int(r["chunk_k"]) != ck:
                continue
            agg.setdefault((r["cell_name"], int(r["queues"])), []).append(r)
        for (cell, q), rs in sorted(agg.items()):
            i, b, p, c = (stat([num(x, k) for x in rs]) for k in ("iops", "bw_bytes", "p99_us", "cpu_us_per_io"))
            rss = stat([num(x, "rss_kib_after") for x in rs]); hwm = stat([num(x, "hwm_kib") for x in rs])
            noisy = any(s and s["noisy"] for s in (i, b)) or any(x.get("noisy_host") == "1" for x in rs)
            out.append(f"| {cell} | {q} | {len(rs)} | {fmt(i and i['med'])} ({fmt(i and i['min'])}-{fmt(i and i['max'])}) | {fmt(b and b['med'], 'GB/s')} | "
                       f"{fmt(p and p['med'])} ({fmt(p and p['max'])}) | {fmt(c and c['med'])} | {'NOISY' if noisy else ''} | "
                       f"{fmt(rss and rss['med'] / 1024)} | {fmt(hwm and hwm['med'] / 1024)} |")
        idle = [r for r in a_idle(a.dirs) if int(r["chunk_k"]) == ck]
        if idle:
            out.append("\nIdle resident set after attach: " + ", ".join(f"{r['queues']} queues {float(r['rss_kib']) / 1024:.0f} MiB" for r in idle))
        out.append("\n### verdicts (DESIGN §20), judged at the better queue count\n")
        out.append("| row | target | queues judged | median | p99 us | verdict | oversubscription | note |")
        out.append("|---|---|---|---|---|---|---|---|")
        verdict[f"c{ck}k"] = {}
        short = []
        for cell, (metric, target, p99max) in TARGETS.items():
            best = None
            for (cn, q), rs in agg.items():
                if cn != cell:
                    continue
                s = stat([num(x, "iops" if metric == "iops" else "bw_bytes") for x in rs])
                pp = stat([num(x, "p99_us") for x in rs])
                if s and (best is None or s["med"] > best[1]["med"]):
                    best = (q, s, pp)
            if not best:
                out.append(f"| {cell} | | | not run | | | | |")
                continue
            q, s, pp = best
            ok = s["med"] >= target and (p99max is None or (pp and pp["med"] <= p99max))
            verdict[f"c{ck}k"][cell] = bool(ok)
            threads = q + 8
            over = f"{q} queue threads + 8 fio jobs = {threads} on {a.cpus} CPUs" + (" (OVERSUBSCRIBED)" if threads > a.cpus else "")
            tt = f"{target / 1e9:.1f} GB/s" if metric == "bw" else f"{target:,.0f} IOPS" + (f", p99 <= {p99max:.0f} us" if p99max else "")
            note = "NOISY" if s["noisy"] else ""
            out.append(f"| {cell} | {tt} | {q} | {fmt(s['med'], 'GB/s' if metric == 'bw' else '')} | {fmt(pp and pp['med'])} | {'pass' if ok else 'SHORTFALL'} | {over} | {note} |")
            if not ok:
                short.append((cell, q, agg[(cell, q)], s, target, metric))
        vr = []
        for cell in ("r4k", "seqr"):
            for q in sorted({q for (cn, q) in agg if cn == cell}):
                base, v = agg.get((cell, q)), agg.get((cell + "-verifyreads", q))
                if base and v:
                    m = "iops" if cell == "r4k" else "bw_bytes"
                    b0, b1 = stat([num(x, m) for x in base]), stat([num(x, m) for x in v])
                    vr.append(f"| {cell} | {q} | {fmt(b0['med'], 'GB/s' if m == 'bw_bytes' else '')} | {fmt(b1['med'], 'GB/s' if m == 'bw_bytes' else '')} | {b1['med'] / b0['med']:.2f} |")
        if vr:
            out.append("\n### verify_reads = true (cost, Spec gaps 10; one run each)\n")
            out.append("| cell | queues | off | on | on/off |\n|---|---|---|---|---|")
            out += vr
        wb = [(cn, q, rs) for (cn, q), rs in sorted(agg.items()) if cn in ("w4k", "seqw")]
        if wb:
            out.append("\n### destage backlog at the end of the write cells (journal bytes in use)\n")
            out.append("| cell | queues | at the end of the cell (median) | 10 s later (median) |\n|---|---|---|---|")
            for cn, q, rs in wb:
                out.append(f"| {cn} | {q} | {fmt(stat([num(x, 'journal_used_end') for x in rs])['med'])} | {fmt(stat([num(x, 'journal_used_idle10') for x in rs])['med'])} |")
        for cell, q, rs, s, target, metric in short:
            out.append(f"\n#### shortfall evidence: {cell} at {q} queues\n")
            gap = target - s["med"]
            out.append(f"- measured median {fmt(s['med'], 'GB/s' if metric == 'bw' else '')} against {target / (1e9 if metric == 'bw' else 1):,.2f}: gap {gap / (1e9 if metric == 'bw' else 1):,.2f} ({100 * gap / target:.0f} % of the target)")
            out.append(f"- BASELINE ceiling: {BASELINE[cell]}")
            nb = ref.get("nullblk", {}).get(cell)
            if nb:
                out.append(f"- null_blk in this session: {nb[0]:,.0f} IOPS, {nb[1] / 1e9:.2f} GB/s")
            jr = ref.get("journal")
            if jr and cell in ("w4k", "seqw"):
                out.append("- journal file system in this session: " + ", ".join(f"{k} {v[0]:,.0f} IOPS {v[1] / 1e6:.0f} MB/s" for k, v in jr.items()))
            c = stat([num(x, "cpu_us_per_io") for x in rs])
            out.append(f"- daemon CPU per I/O: {fmt(c and c['med'])} us; thread split of the first repetition (seconds over the window): {rs[0].get('thread_split', '')}")
            w = num(rs[0], "window_s")
            busiest = 0.0
            for tok in (rs[0].get("thread_split", "") or "").split():
                try:
                    secs = float(tok.split("=")[1].split("(")[0]); n = int(tok.split("(")[1].rstrip(")"))
                    busiest = max(busiest, secs / max(w or 1, 1) / n)
                except (IndexError, ValueError):
                    pass
            out.append(f"- busiest thread group uses {busiest:.2f} cores per thread (>= 0.9 means a saturated thread)")
            out.append("- the fraction of the gap each factor explains: take the factor's ceiling in the same unit (the rows above), "
                       "explained = clamp((target - ceiling) / (target - measured), 0, 1); perf stat / report of the first repetition are under raw/<cell>/")
    print("# M16 Part A\n" + "\n".join(out))
    if a.json:
        json.dump(verdict, open(a.json, "w"), indent=1)
    return 0


def a_idle(dirs):
    return [r for r in rows_of(dirs) if r["cell"].endswith("/idle")]


if __name__ == "__main__":
    sys.exit(main())
