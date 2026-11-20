#!/usr/bin/env python3
"""ADR-33 T6: Lunix cells against the raw baseline of the same file system (W9), as markdown tables.

  t6.py --raw RAWDIR --lunix OUTDIR [--label NAME] [--ring-note TEXT] > tables.md

RAWDIR holds rawbase.tsv (tests/perf/rawbase.sh), OUTDIR the results.jsonl and raw/<cell>/ directories of
tests/perf/partA.sh run with STATUS_PROBE=1 SAMPLE=1 (and the fio per-second maximum latency logs).
Ratios follow W9: burst writes against raw writes, sustained writes against the model, misses against raw reads,
cache-hit reads against the ublk null target (--null FILE: lines "cell iops bw_bytes" of the null-target run).
"""
import argparse
import glob
import json
import os
import statistics as st


def med(v):
    v = [x for x in v if x is not None]
    return (st.median(v), min(v), max(v), len(v)) if v else None


def fnum(x):
    try:
        return float(x)
    except (TypeError, ValueError):
        return None


def raw_table(path):
    out = {}
    for i, line in enumerate(open(path)):
        if i == 0 or not line.strip():
            continue
        c, rep, iops, bw, p50, p99, p999, err = line.rstrip("\n").split("\t")
        out.setdefault(c, []).append({"iops": float(iops), "bw": float(bw), "p50": float(p50), "p99": float(p99)})
    return out


def lat_stalls(celldir):
    """From the per-second max completion latency logs: (max latency s, seconds with a max above 1 s, windows)."""
    mx = 0.0
    stalled = {}
    nwin = 0
    for f in glob.glob(os.path.join(celldir, "lat_clat.*.log")):
        for line in open(f):
            p = [x.strip() for x in line.split(",")]
            if len(p) < 2:
                continue
            t, v = int(p[0]) // 1000, float(p[1]) / 1e9   # fio >= 3: nanoseconds
            mx = max(mx, v)
            nwin = max(nwin, t + 1)
            if v > 1.0:
                stalled[t] = max(stalled.get(t, 0), v)
    return mx, len(stalled), nwin


def series(celldir):
    rows = []
    p = os.path.join(celldir, "series.jsonl")
    if os.path.exists(p):
        for line in open(p):
            try:
                rows.append(json.loads(line))
            except ValueError:
                pass
    return rows


def fmt(x, d=0):
    return "-" if x is None else (f"{x:,.{d}f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", required=True)
    ap.add_argument("--lunix", required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--null", default=None)
    a = ap.parse_args()
    raw = raw_table(os.path.join(a.raw, "rawbase.tsv"))
    rawm = {c: med([r["iops"] for r in v]) for c, v in raw.items()}
    rawb = {c: med([r["bw"] for r in v]) for c, v in raw.items()}
    rows = [json.loads(line) for line in open(os.path.join(a.lunix, "results.jsonl")) if line.strip()]
    cells = {}
    for r in rows:
        if r.get("cell_name") in ("r4k", "w4k", "seqw", "seqr") and r.get("cell", "").startswith("partA/"):
            cells.setdefault((int(r["chunk_k"]), r["cell_name"]), []).append(r)
    nullm = {}
    if a.null and os.path.exists(a.null):
        for line in open(a.null):
            p = line.split()
            if len(p) == 3:
                nullm[p[0]] = (float(p[1]), float(p[2]))
    print(f"# T6 tables {a.label}\n")
    print("## raw baseline (fio on the file system that holds the journal and the store)\n")
    print("| cell | IOPS median (min-max) | MB/s median | p50 ms | p99 ms |")
    print("|---|---|---|---|---|")
    for c in ("w4k", "r4k", "seqw", "seqr", "commit64k", "commit4k", "commit4k-x8"):
        if c in raw:
            m = rawm[c]; b = rawb[c]
            print(f"| {c} | {fmt(m[0])} ({fmt(m[1])}-{fmt(m[2])}) | {fmt(b[0]/1e6, 1)} | "
                  f"{fmt(med([r['p50'] for r in raw[c]])[0]/1000, 1)} | {fmt(med([r['p99'] for r in raw[c]])[0]/1000, 1)} |")
    print("\n## Lunix against raw\n")
    print("| chunk | cell | working set | ring | Lunix median (min-max) | raw counterpart | ratio | p99 ms | max completion s | 1 s windows over 1 s | status probe max ms |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    pair = {"w4k": "w4k", "r4k": "r4k", "seqw": "seqw", "seqr": "seqr"}
    for (ck, name), rs in sorted(cells.items()):
        bw = name in ("seqw", "seqr")
        val = [fnum(r.get("bw_bytes")) if bw else fnum(r.get("iops")) for r in rs]
        m = med(val)
        rm = (rawb if bw else rawm).get(pair[name])
        ratio = m[0] / rm[0] if m and rm else None
        unit = (lambda x: fmt(x / 1e6, 1) + " MB/s") if bw else (lambda x: fmt(x) + " IOPS")
        p99 = med([fnum(r.get("p99_us")) for r in rs])
        stalls = [lat_stalls(os.path.join(a.lunix, "raw", r["cell"].replace("/", "_"))) for r in rs]
        mxl = max((s[0] for s in stalls), default=0)
        nst = sorted(s[1] for s in stalls)
        ws = rs[0].get("ws_gib", "4")
        ring = int(rs[0].get("journal_max", 0) or 0) >> 20
        pm = [fnum(r.get("status_probe_max_ms")) for r in rs]
        pm = [x for x in pm if x is not None]
        print(f"| {ck} KiB | {name} | {ws} GiB | {ring} MiB | {unit(m[0])} ({unit(m[1])}-{unit(m[2])}) | {unit(rm[0]) if rm else '-'} | "
              f"{fmt(ratio * 100, 1) + ' %' if ratio is not None else '-'} | {fmt((p99[0] if p99 else 0) / 1000, 1)} | {fmt(mxl, 1)} | "
              f"{'/'.join(str(x) for x in nst)} | {fmt(max(pm)) if pm else '-'} |")
    print("\n## per-cell detail\n")
    print("| cell | rep | IOPS | MB/s | commits | stage ms (hash,dedup,compress,seal,append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for (ck, name), rs in sorted(cells.items()):
        for r in rs:
            wp = (r.get("write_path_end") or ",,,,,,").split(",")
            stg = [fnum(x) for x in (r.get("stage_ns_delta") or "").split(",") if x != ""]
            stgs = "; ".join(fmt(x / 1e6) for x in stg) if stg else "-"
            print(f"| {r['cell']} | {r['rep']} | {fmt(fnum(r.get('iops')))} | {fmt((fnum(r.get('bw_bytes')) or 0) / 1e6, 1)} | {r.get('commits_delta', '')} | {stgs} | "
                  f"{wp[0]} / {wp[1]} | {wp[3]} / {wp[4]} | {wp[5]} / {wp[6]} | {r.get('journal_used_end', '')} |")
    # time series: ring waiters, window, cut bytes and ms, per sample, first repetition of the write cells
    print("\n## counters over time (first repetition of the write cells, sampler)\n")
    for (ck, name), rs in sorted(cells.items()):
        if name not in ("w4k", "seqw"):
            continue
        ser = series(os.path.join(a.lunix, "raw", rs[0]["cell"].replace("/", "_")))
        if not ser:
            continue
        print(f"\n### {ck} KiB {name}\n")
        print("| t s | commits | ops_write | journal used | ring waiters | longest wait ms | window MiB | hits | misses | last cut MiB | last cut ms |")
        print("|---|---|---|---|---|---|---|---|---|---|---|")
        for s in ser:
            stt = s.get("status") or {}
            wp = stt.get("write_path") or {}
            sx = stt.get("stats") or {}
            print(f"| {s.get('t')} | {sx.get('commits', '')} | {sx.get('ops_write', '')} | {stt.get('journal_used', '')} | {wp.get('ring_waiters', '')} | {wp.get('ring_wait_longest_ms', '')} | "
                  f"{fmt((wp.get('window_resident_bytes') or 0) / 1048576)} | {wp.get('window_hits', '')} | {wp.get('window_misses', '')} | {fmt((wp.get('last_cut_bytes') or 0) / 1048576)} | {wp.get('last_cut_ms', '')} |")


if __name__ == "__main__":
    main()
