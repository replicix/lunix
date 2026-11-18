#!/usr/bin/env python3
"""M16 Part B tables, the advisory rule of ADR-5 and the proposed "Gate outcome" block.

  analyze.py OUT_DIR [OUT_DIR ...] [--parta verdict.json] [--md out.md] > tables.md

Reads results.jsonl of tests/perf/chunksize.sh (and the raw files each cell directory holds). Per workload, chunk size and
file system: logical bytes written (the status counter `bytes_written` over the workload), dedup and compression ratio
(the recount of `lunix fsck --deep`), live data bytes, map pack bytes (per TiB mapped and per GiB written), the map seal
write amplification (exact: the delta of the status counter `stats.map_bytes_written`, map pack bytes written by commits, over
the bytes written; when an input lacks that counter the space ledger's map bytes over the bytes written is used instead and
the cell is marked "proxy" under the table), the on-disk store after the final GC, daemon CPU seconds per GiB written (utime+stime of the
process), partial-chunk RMW per 1 000 writes (`cold_rmw` over write operations), and the workload's own throughput and
latency. Cells run more than once show median and min-max, noisy when max/min > 1.2; cells run once say so.

Advisory rule (fixed in advance in the plan; the maintainer decides): recommend 16 KiB iff, over the VM-image and pgbench
workloads, stored bytes per logical GiB written (data plus map) are >= 15 % lower than at 64 KiB, AND daemon CPU per GiB is
<= 1.3x, AND the Part A rows that pass at 64 KiB still pass at 16 KiB (--parta, the JSON of bench/phase1/perf/analyze.py).
Otherwise recommend 64 KiB. Every clause is printed with its numbers.
"""
import argparse
import json
import os
import statistics as st
import sys

RULE = ("pgbench", "vm")
GIB = 2 ** 30
TIB = 2 ** 40


def jload(p):
    try:
        t = open(p).read()
        i = [k for k in (t.find("{"), t.find("[")) if k >= 0]
        return json.loads(t[min(i):]) if i else None
    except (OSError, ValueError):
        return None


def rows(dirs):
    out = []
    for d in dirs:
        p = os.path.join(d, "results.jsonl")
        if os.path.exists(p):
            for line in open(p):
                if line.strip():
                    r = json.loads(line)
                    r["_out"] = d
                    out.append(r)
    return out


def cell_metrics(r):
    d = r.get("dir")
    base = os.path.join(r["_out"], "raw", os.path.basename(d.rstrip("/"))) if d else None
    if not base or not os.path.isdir(base):
        return None
    s0, s1, s2 = (jload(os.path.join(base, f"status-{i}.json")) or {} for i in (0, 1, 2))
    fs = jload(os.path.join(base, "fsck.json")) or {}
    ra = fs.get("ratios") or {}
    by = ra.get("bytes") or {}
    g = lambda s, k: (s.get("stats") or {}).get(k)
    bw = (g(s1, "bytes_written") or 0) - (g(s0, "bytes_written") or 0)
    opsw = (g(s1, "ops_write") or 0) - (g(s0, "ops_write") or 0)
    rmw = (g(s1, "cold_rmw") or 0) - (g(s0, "cold_rmw") or 0)
    if g(s1, "map_bytes_written") is not None:
        mwa = ((g(s1, "map_bytes_written") or 0) - (g(s0, "map_bytes_written") or 0), "exact")
    else:
        mwa = ((s1.get("space") or {}).get("map"), "proxy")
    chunk = int(r["chunk"])
    mapped = ra.get("mapped_chunks") or 0
    store_gc = sum(by.get(k, 0) for k in ("segments", "map_packs", "commits", "index")) if by else None
    live = ra.get("unique_clen")
    mp = by.get("map_packs")
    m = {
        "logical_gib": bw / GIB, "dedup": ra.get("dedup_ratio"), "compression": ra.get("compression_ratio"),
        "live_data_gib": live / GIB if live is not None else None, "map_gib": mp / GIB if mp is not None else None,
        "map_per_tib_mapped_gib": (mp / (mapped * chunk / TIB) / GIB) if mp is not None and mapped else None,
        "map_per_gib_written": (mp / (bw / GIB)) if mp is not None and bw else None,
        "map_seal_wa": (mwa[0] / bw) if bw and mwa[0] is not None else None, "map_seal_wa_basis": mwa[1],
        "store_after_gc_gib": store_gc / GIB if store_gc is not None else None,
        "stored_per_logical_gib": ((live + mp) / (bw / GIB) / GIB) if live is not None and mp is not None and bw else None,
        "store_per_logical_gib_after_gc": (store_gc / (bw / GIB) / GIB) if store_gc is not None and bw else None,
        "cpu_s": float(r.get("daemon_cpu_s") or 0), "cpu_s_per_gib": (float(r.get("daemon_cpu_s") or 0) / (bw / GIB)) if bw else None,
        "rmw_per_1000_writes": (rmw / opsw * 1000) if opsw else None, "secs": float(r.get("workload_secs") or 0),
        "live_bytes": live, "map_bytes": mp, "logical_bytes": bw, "fsck_rc": r.get("fsck_rc"),
    }
    j = jload(os.path.join(base, "fio.json"))
    if j and j.get("jobs"):
        w = [x for x in j["jobs"] if x["write"]["total_ios"]][-1]["write"]
        m["iops"], m["mb_s"] = w["iops"], w["bw_bytes"] / 1e6
        p = w["clat_ns"].get("percentile", {})
        m["p50_ms"], m["p99_ms"], m["p999_ms"] = (p.get(k, 0) / 1e6 for k in ("50.000000", "99.000000", "99.900000"))
    try:
        txt = open(os.path.join(base, "pgbench-run.txt")).read()
        for line in txt.splitlines():
            if line.startswith("tps =") and "without initial" in line:
                m["tps"] = float(line.split("=")[1].split()[0])
        pl = jload(os.path.join(base, "pglat.json")) or {}
        for k in ("p50_ms", "p90_ms", "p99_ms", "p99.9_ms"):
            if k in pl:
                m["pg_" + k] = pl[k]
        m["pg_version"] = open(os.path.join(base, "pg-version.txt")).read().strip()
    except OSError:
        pass
    for i in range(3):
        try:
            b = int(open(os.path.join(base, f"vm-img{i}-pkgs-before.txt")).read()); a = int(open(os.path.join(base, f"vm-img{i}-pkgs-after.txt")).read())
            m[f"vm_img{i}_new_pkgs"] = a - b
        except (OSError, ValueError):
            pass
    return m


def agg(vals):
    v = [x for x in vals if x is not None]
    if not v:
        return None
    return {"med": st.median(v), "min": min(v), "max": max(v), "n": len(v), "noisy": len(v) > 1 and min(v) > 0 and max(v) / min(v) > 1.2}


def f(a, nd=3):
    if a is None:
        return "n/a"
    s = f"{a['med']:.{nd}f}"
    if a["n"] > 1:
        s += f" ({a['min']:.{nd}f}-{a['max']:.{nd}f}{', NOISY' if a['noisy'] else ''})"
    return s


COLS = [("logical GiB written", "logical_gib", 1), ("dedup", "dedup", 3), ("compression", "compression", 3), ("live data GiB", "live_data_gib", 2),
        ("map GiB", "map_gib", 3), ("map GiB per TiB mapped", "map_per_tib_mapped_gib", 2), ("map bytes per GiB written", "map_per_gib_written", 4),
        ("map seal WA (map bytes written / bytes written)", "map_seal_wa", 4), ("store after GC GiB", "store_after_gc_gib", 2),
        ("stored (data+map) per logical GiB", "stored_per_logical_gib", 4), ("daemon CPU s per GiB", "cpu_s_per_gib", 2),
        ("cold RMW per 1000 writes", "rmw_per_1000_writes", 1), ("IOPS", "iops", 0), ("MB/s", "mb_s", 0), ("p99 ms", "p99_ms", 2), ("TPS", "tps", 0),
        ("pg p99 ms", "pg_p99_ms", 2)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--parta")
    ap.add_argument("--md")
    a = ap.parse_args()
    rs = [r for r in rows(a.dirs) if r["cell"].startswith("chunk/") and r.get("workload") and r["status"] != "skip"]
    cells = {}
    for r in rs:
        m = cell_metrics(r)
        if m:
            cells.setdefault((r["workload"], int(r["chunk"]), r["fs"]), []).append(m)
    out = ["# M16 Part B: chunk size 16 KiB against 64 KiB\n"]
    for w in sorted({k[0] for k in cells}):
        out.append(f"\n## {w}\n")
        keys = sorted(k for k in cells if k[0] == w)
        out.append("| fs | chunk KiB | runs | " + " | ".join(c[0] for c in COLS) + " |")
        out.append("|---|---|---|" + "---|" * len(COLS))
        for k in keys:
            ms = cells[k]
            out.append(f"| {k[2]} | {k[1] // 1024} | {len(ms)}{' (once: noise caveat)' if len(ms) == 1 else ''} | " + " | ".join(f(agg([m.get(c[1]) for m in ms]), c[2]) for c in COLS) + " |")
        proxied = sorted({f"{k[2]} {k[1] // 1024} KiB" for k in keys for m in cells[k] if m.get("map_seal_wa_basis") == "proxy"})
        if proxied:
            out.append(f"\nmap seal WA is a PROXY (space ledger map bytes over bytes written; no `stats.map_bytes_written` in the input) for: {', '.join(proxied)}")
        if w == "pgbench":
            vs = {m.get("pg_version") for ms in cells.values() for m in ms if m.get("pg_version")}
            out.append(f"\nPostgreSQL: {', '.join(sorted(vs)) or 'version not recorded'}")
    # the rule
    agg_by_chunk = {}
    per_w = {}
    for (w, ch, fs), ms in cells.items():
        if w not in RULE:
            continue
        a_ = agg_by_chunk.setdefault(ch, {"stored": 0.0, "logical": 0.0, "cpu": 0.0})
        for m in ms[:1] if len(ms) == 1 else ms:
            if m.get("live_bytes") is None or m.get("map_bytes") is None:
                continue
            a_["stored"] += (m["live_bytes"] + m["map_bytes"]) / len(ms)
            a_["logical"] += m["logical_bytes"] / len(ms)
            a_["cpu"] += m["cpu_s"] / len(ms)
        per_w.setdefault(w, {})[(ch, fs)] = agg([m.get("stored_per_logical_gib") for m in ms])
    out.append("\n## Advisory rule (ADR-5, fixed in advance)\n")
    c16, c64 = agg_by_chunk.get(16384), agg_by_chunk.get(65536)
    verdict = None
    if not (c16 and c64 and c16["logical"] and c64["logical"]):
        out.append("Not evaluable: the VM-image and pgbench cells of both chunk sizes are not all present.")
    else:
        s16, s64 = c16["stored"] / (c16["logical"] / GIB) / GIB, c64["stored"] / (c64["logical"] / GIB) / GIB
        p16, p64 = c16["cpu"] / (c16["logical"] / GIB), c64["cpu"] / (c64["logical"] / GIB)
        cl1 = s16 <= 0.85 * s64
        cl2 = p16 <= 1.3 * p64
        cl3 = None
        if a.parta and os.path.exists(a.parta):
            v = json.load(open(a.parta))
            v16, v64 = v.get("c16k", {}), v.get("c64k", {})
            miss = [k for k, ok in v64.items() if ok and not v16.get(k)]
            cl3 = not miss and bool(v64)
            c3t = "all rows that pass at 64 KiB pass at 16 KiB" if cl3 else f"rows that pass at 64 KiB but not at 16 KiB: {', '.join(miss) or 'Part A data missing'}"
        else:
            c3t = "not evaluated (no --parta verdict file)"
        out.append(f"1. stored bytes (data plus map) per logical GiB written, VM-image and pgbench workloads together: 16 KiB {s16:.4f}, 64 KiB {s64:.4f} "
                   f"-> 16 KiB is {100 * (1 - s16 / s64):.1f} % {'lower' if s16 <= s64 else 'higher'} (need >= 15 % lower): **{'holds' if cl1 else 'does not hold'}**")
        out.append(f"2. daemon CPU seconds per logical GiB: 16 KiB {p16:.3f}, 64 KiB {p64:.3f} -> {p16 / p64:.2f}x (need <= 1.3x): **{'holds' if cl2 else 'does not hold'}**")
        out.append(f"3. Part A: {c3t}: **{'holds' if cl3 else ('not evaluated' if cl3 is None else 'does not hold')}**")
        rec16 = cl1 and cl2 and bool(cl3)
        verdict = "16 KiB" if rec16 else "64 KiB"
        out.append(f"\n**Recommendation by the rule: {verdict}**" + ("" if cl3 is not None else " (clause 3 was not evaluated: complete the Part A comparison before using this)"))
        out.append("\nPer-workload stored bytes per logical GiB (data plus map), for the reader who wants to see whether the aggregate hides a split:\n")
        out.append("| workload | fs | 16 KiB | 64 KiB | 16 KiB vs 64 KiB |\n|---|---|---|---|---|")
        for w, d in sorted(per_w.items()):
            for fs in sorted({k[1] for k in d}):
                x16, x64 = d.get((16384, fs)), d.get((65536, fs))
                if x16 and x64:
                    out.append(f"| {w} | {fs} | {x16['med']:.4f} | {x64['med']:.4f} | {100 * (1 - x16['med'] / x64['med']):.1f} % lower |")
        out.append("\n### Proposed Gate outcome block for ADR-5 (the lead appends it after the maintainer decides)\n")
        out.append("```\n**Gate outcome (phase 1, M16):** advisory rule applied to the VM-image and pgbench workloads on ext4 and xfs volumes.\n"
                   f"Stored bytes per logical GiB written (data plus map): 16 KiB {s16:.4f}, 64 KiB {s64:.4f} ({100 * (1 - s16 / s64):.1f} % {'lower' if s16 <= s64 else 'higher'}; clause 1 {'holds' if cl1 else 'does not hold'}).\n"
                   f"Daemon CPU per GiB: {p16 / p64:.2f}x (clause 2 {'holds' if cl2 else 'does not hold'}). Part A: {c3t} (clause 3 {'holds' if cl3 else ('not evaluated' if cl3 is None else 'does not hold')}).\n"
                   f"Recommendation: {verdict} as the default; both sizes stay supported per volume. Evidence: bench/phase1/chunksize/RESULTS.md.\n```")
    text = "\n".join(out) + "\n"
    print(text)
    if a.md:
        open(a.md, "w").write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
