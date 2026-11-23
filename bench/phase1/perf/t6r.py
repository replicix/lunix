#!/usr/bin/env python3
"""ADR-33 T6 re-run: per-repetition write-cell metrics (completion latency, EIO, write_path counters) and the before/after table.

  t6r.py --new RESULTS/t6r/writes-disk [--old RESULTS/t6/writes-disk] [--rawnew DIR --rawold DIR]

Reads raw/fio-<cell>.json, raw/<cell>/{lat_clat.*.log,probe.tsv,series.jsonl,status-before.json,status-end.json} of tests/perf/partA.sh.
"""
import argparse
import glob
import json
import os
import statistics as st


def jl(p):
    try:
        return json.load(open(p))
    except (OSError, ValueError):
        return None


def cell_metrics(root, ck, name, rep):
    cell = f"partA_c{ck}k_q4_{name}_rep{rep}"
    f = jl(os.path.join(root, "raw", f"fio-{cell}.json"))
    if f is None:
        p = os.path.join(root, "raw", f"fio-{cell}.stdout")
        try:
            txt = open(p).read()
            f = json.loads(txt[txt.index("{"):])
        except (OSError, ValueError):
            return None
    d = os.path.join(root, "raw", cell)
    j = f["jobs"][0]
    w = j["write"]
    pc = w["clat_ns"].get("percentile", {})
    m = {"cell": cell, "iops": w["iops"], "bw": w["bw_bytes"], "p99_s": pc.get("99.000000", 0) / 1e9, "p999_s": pc.get("99.900000", 0) / 1e9,
         "max_s": w["clat_ns"].get("max", 0) / 1e9, "err": sum(int(x.get("error", 0) or 0) for x in f["jobs"])}
    # per-second maxima from the logs: seconds above 1 s and above 5 s
    sec = {}
    for lf in glob.glob(os.path.join(d, "lat_clat.*.log")):
        for line in open(lf):
            p = [x.strip() for x in line.split(",")]
            if len(p) >= 2:
                t, v = int(p[0]) // 1000, float(p[1]) / 1e9
                sec[t] = max(sec.get(t, 0), v)
    m["win"] = len(sec)
    m["win_gt1"] = sum(1 for v in sec.values() if v > 1)
    m["win_gt5"] = sum(1 for v in sec.values() if v > 5)
    m["logmax_s"] = max(sec.values()) if sec else None
    try:
        rows = [float(l.split("\t")[1]) for l in open(os.path.join(d, "probe.tsv")) if l.strip()]
        m["probe_max_ms"] = max(rows)
    except (OSError, ValueError, IndexError):
        m["probe_max_ms"] = None
    b, e = jl(os.path.join(d, "status-before.json")), jl(os.path.join(d, "status-end.json"))
    if e:
        m["commits"] = e["stats"]["commits"] - (b["stats"]["commits"] if b else 0)
        m["last_cut_ms"] = e.get("write_path", {}).get("last_cut_ms")
        wp, wb = e.get("write_path"), (b or {}).get("write_path")
        if wp and "ring_wait_max_since_attach_ms" in wp:
            m["ring_wait_since_attach_ms"] = wp["ring_wait_max_since_attach_ms"]
            m["ring_wait_60s_ms"] = wp["ring_wait_max_60s_ms"]
            m["freed_by_lane"] = [x - y for x, y in zip(wp["freed_files_by_lane"], (wb or wp)["freed_files_by_lane"])] if wb and wb.get("freed_files_by_lane") else wp["freed_files_by_lane"]
            for k in ("ring_paced_writes", "ring_paced_ms", "ring_pace_capped", "ring_pace_bypassed_fua"):
                m[k] = wp[k] - (wb[k] if wb else 0)
            m["ring_pace_max_ms"] = wp["ring_pace_max_ms"]
            h = wp["write_latency_log2_us"]
            hb = (wb or {}).get("write_latency_log2_us") or [0] * len(h)
            dh = [x - y for x, y in zip(h, hb)]
            m["hist_ge1s"] = sum(dh[21:])      # bucket b: 2^(b-1) <= t < 2^b us; b >= 21 is >= 1.05 s
            m["hist_ge4s"] = sum(dh[23:])      # >= 4.2 s
            m["hist_ge16s"] = sum(dh[25:])     # >= 16.8 s
            m["hist_total"] = sum(dh)
            m["hist"] = dh
        elif e.get("journal_used") is not None:
            pass
    # series maxima
    pm, pr, rw = 0, 0, 0
    for line in open(os.path.join(d, "series.jsonl")) if os.path.exists(os.path.join(d, "series.jsonl")) else []:
        try:
            s = json.loads(line)["status"]
        except (ValueError, KeyError):
            continue
        w2 = s.get("write_path") or {}
        pm = max(pm, w2.get("ring_parked_writes", 0))
        pr = max(pr, w2.get("ring_pace_rate", 0))
        rw = max(rw, w2.get("ring_waiters", 0))
    m["parked_max"], m["pace_rate_max"], m["waiters_max"] = pm, pr, rw
    return m


def med(v):
    v = [x for x in v if x is not None]
    return st.median(v) if v else None


def f(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def collect(root):
    out = {}
    for ck in (16, 64):
        for name in ("w4k", "seqw"):
            ms = [cell_metrics(root, ck, name, r) for r in (1, 2, 3)]
            out[(ck, name)] = [m for m in ms if m]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--new", required=True)
    ap.add_argument("--old")
    a = ap.parse_args()
    new = collect(a.new)
    old = collect(a.old) if a.old else {}
    print("## per repetition, new run\n")
    print("| cell | rep | IOPS | MB/s | p99 s | p99.9 s | max s | EIO | secs max>1 s / >5 s (of) | ring wait max since attach s | ring wait max 60 s s | parked max | pace rate max MB/s | paced writes / ms / max ms / capped | freed by lane | writes >=1 s / >=4 s / >=17 s (of) | commits | status probe max ms |")
    print("|" + "---|" * 18)
    for (ck, name), ms in sorted(new.items()):
        for i, m in enumerate(ms, 1):
            fl = m.get("freed_by_lane")
            print(f"| {ck} KiB {name} | {i} | {f(m['iops'], 0)} | {f(m['bw'] / 1e6)} | {f(m['p99_s'])} | {f(m['p999_s'])} | {f(m['max_s'])} | {m['err']} | {m['win_gt1']} / {m['win_gt5']} ({m['win']}) | "
                  f"{f((m.get('ring_wait_since_attach_ms') or 0) / 1000)} | {f((m.get('ring_wait_60s_ms') or 0) / 1000)} | {m['parked_max']} | {f(m['pace_rate_max'] / 1e6, 0)} | "
                  f"{m.get('ring_paced_writes', '-')} / {m.get('ring_paced_ms', '-')} / {m.get('ring_pace_max_ms', '-')} / {m.get('ring_pace_capped', '-')} | {fl} | "
                  f"{m.get('hist_ge1s', '-')} / {m.get('hist_ge4s', '-')} / {m.get('hist_ge16s', '-')} ({m.get('hist_total', '-')}) | {m.get('commits', '-')} | {f(m['probe_max_ms'], 0)} |")
    if old:
        print("\n## per repetition, first T6 (same fields that existed)\n")
        print("| cell | rep | IOPS | MB/s | p99 s | p99.9 s | max s | EIO | secs max>1 s / >5 s (of) | commits | status probe max ms |")
        print("|" + "---|" * 11)
        for (ck, name), ms in sorted(old.items()):
            for i, m in enumerate(ms, 1):
                print(f"| {ck} KiB {name} | {i} | {f(m['iops'], 0)} | {f(m['bw'] / 1e6)} | {f(m['p99_s'])} | {f(m['p999_s'])} | {f(m['max_s'])} | {m['err']} | {m['win_gt1']} / {m['win_gt5']} ({m['win']}) | {m.get('commits', '-')} | {f(m['probe_max_ms'], 0)} |")


if __name__ == "__main__":
    main()
