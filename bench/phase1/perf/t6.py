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


def thr(r):
    """Throttle counters over a cell: throttled ms, stops and delayed writes (status-end minus status-before), gc pressure at the end."""
    d = os.path.join(CELLROOT, r["cell"].replace("/", "_"))
    try:
        a = json.load(open(os.path.join(d, "status-before.json")))["throttle"]
        b = json.load(open(os.path.join(d, "status-end.json")))["throttle"]
        return f"{b['throttled_ms'] - a['throttled_ms']} / {b['stops'] - a['stops']} / {b['delayed_writes'] - a['delayed_writes']} | {b['gc_pressure']}"
    except (OSError, KeyError, ValueError):
        return " | "


CELLROOT = "."


def fmt(x, d=0):
    return "-" if x is None else (f"{x:,.{d}f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", required=True, action="append", help="raw baseline dir(s) with rawbase.tsv; cells of several dirs are pooled")
    ap.add_argument("--lunix", required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--null", action="append", default=[], help="QUEUES:FILE of tests/perf/nulltarget.sh (repeatable)")
    ap.add_argument("--read-ref", default="r4k", help="raw cell the read cells are compared with (r4k, r4k-buffered)")
    ap.add_argument("--seqread-ref", default="seqr")
    a = ap.parse_args()
    global CELLROOT
    CELLROOT = os.path.join(a.lunix, "raw")
    raw = {}
    for d in a.raw:
        for c, v in raw_table(os.path.join(d, "rawbase.tsv")).items():
            raw.setdefault(c, []).extend(v)
    rawm = {c: med([r["iops"] for r in v]) for c, v in raw.items()}
    rawb = {c: med([r["bw"] for r in v]) for c, v in raw.items()}
    rows = [json.loads(line) for line in open(os.path.join(a.lunix, "results.jsonl")) if line.strip()]
    cells = {}
    for r in rows:
        if r.get("cell_name") in ("r4k", "w4k", "seqw", "seqr") and r.get("cell", "").startswith("partA/"):
            cells.setdefault((int(r["chunk_k"]), r["cell_name"], int(r.get("queues") or 4)), []).append(r)
    nullm = {}   # (queues, cell) -> (iops, bw)
    for spec in a.null:
        q, f = spec.split(":", 1)
        for line in open(f):
            p = line.split()
            if len(p) == 3:
                nullm[(int(q), p[0])] = (float(p[1]), float(p[2]))
    print(f"# T6 tables {a.label}\n")
    print("## raw baseline (fio on the file system that holds the journal and the store)\n")
    print("| cell | IOPS median (min-max) | MB/s median | p50 ms | p99 ms |")
    print("|---|---|---|---|---|")
    for c in ("w4k", "r4k", "seqw", "seqr", "commit64k", "commit4k", "commit4k-x8", "r4k-buffered", "seqr-buffered", "seqw-buffered"):
        if c in raw:
            m = rawm[c]; b = rawb[c]
            print(f"| {c} | {fmt(m[0])} ({fmt(m[1])}-{fmt(m[2])}) | {fmt(b[0]/1e6, 1)} | "
                  f"{fmt(med([r['p50'] for r in raw[c]])[0]/1000, 1)} | {fmt(med([r['p99'] for r in raw[c]])[0]/1000, 1)} |")
    print("\n## Lunix against raw\n")
    print("| chunk | queues | cell | working set | ring | Lunix median (min-max) | counterpart | ratio | p99 ms | max completion s | 1 s windows over 1 s | status probe max ms |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    pair = {"w4k": "w4k", "r4k": a.read_ref, "seqw": "seqw", "seqr": a.seqread_ref}
    for (ck, name, q), rs in sorted(cells.items()):
        bw = name in ("seqw", "seqr")
        val = [fnum(r.get("bw_bytes")) if bw else fnum(r.get("iops")) for r in rs]
        m = med(val)
        rm = (rawb if bw else rawm).get(pair[name])
        nl = nullm.get((q, name))
        if nl:
            rm = (nl[1], 0, 0, 1) if bw else (nl[0], 0, 0, 1)
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
        print(f"| {ck} KiB | {q} | {name} | {ws} GiB | {ring} MiB | {unit(m[0])} ({unit(m[1])}-{unit(m[2])}) | {unit(rm[0]) if rm else '-'} | "
              f"{fmt(ratio * 100, 1) + ' %' if ratio is not None else '-'} | {fmt((p99[0] if p99 else 0) / 1000, 1)} | {fmt(mxl, 1)} | "
              f"{'/'.join(str(x) for x in nst)} | {fmt(max(pm)) if pm else '-'} |")
    # write-amplification model (research note section 6): sustained = B / (2 r + U(n) cs / (cr n)) for 4 KiB random writes,
    # B / (2 + 1 / cr) per logical byte for sequential ones. B = the raw sequential write bandwidth of the same file system
    # (direct, 1 MiB, QD 32, 8 jobs); r = 4,160 bytes (a 4 KiB record); n = writes per cut (last cut bytes / r, median of the sampler);
    # cr = the volume's compression ratio. Journal, zero rewrite and store share one device; GC rewrites and cold read-modify-write
    # reads are not in the model.
    import math
    bwraw = rawb.get("seqw")
    print("\n## sustained writes against the model\n")
    print("| chunk | cell | B (raw seqw MB/s) | n writes per cut | cr | model | measured | measured / model |")
    print("|---|---|---|---|---|---|---|---|")
    for (ck, name, q), rs in sorted(cells.items()):
        if name not in ("w4k", "seqw") or not bwraw:
            continue
        ser = series(os.path.join(a.lunix, "raw", rs[0]["cell"].replace("/", "_")))
        cuts = sorted({(x["status"]["write_path"]["last_cut_bytes"]) for x in ser if x.get("status") and x["status"].get("write_path", {}).get("last_cut_bytes")})
        cr = None
        for r in rs:
            f = os.path.join(a.lunix, "raw", r["cell"].replace("/", "_"), "status-end.json")
            if os.path.exists(f):
                cr = json.load(open(f)).get("compression_ratio")
        if not cr:
            continue
        B = bwraw[0]
        cs = ck * 1024
        W = float(rs[0].get("ws_gib", 4)) * (1 << 30)
        if name == "w4k":
            r_ = 4160.0
            n = (st.median(cuts) / r_) if cuts else None
            if not n:
                continue
            U = (W / cs) * (1 - math.exp(-n * cs / W))
            model = B / (2 * r_ + U * cs / cr / n)
            val = med([fnum(r.get("iops")) for r in rs])
            print(f"| {ck} KiB | w4k | {B / 1e6:.0f} | {n:,.0f} | {cr:.2f} | {model:,.0f} IOPS | {val[0]:,.0f} IOPS | {val[0] / model * 100:.0f} % |")
        else:
            model = B / (2 + 1 / cr)
            val = med([fnum(r.get("bw_bytes")) for r in rs])
            print(f"| {ck} KiB | seqw | {B / 1e6:.0f} | - | {cr:.2f} | {model / 1e6:.1f} MB/s | {val[0] / 1e6:.1f} MB/s | {val[0] / model * 100:.0f} % |")
    # burst: the 4 KiB writes acknowledged between the start of fio and the first sampler reading (the sampler starts RAMP + 1 s
    # after fio, the first reading is SAMPLE_S s later), from the ops_write counter; the ring is not full yet
    print("\n## burst writes (ring not yet full), from the ops_write counter\n")
    print("| chunk | cell | ops before | ops at first sample | seconds since fio start | burst IOPS | sustained IOPS (fio) | journal used at first sample |")
    print("|---|---|---|---|---|---|---|---|")
    for (ck, name, q), rs in sorted(cells.items()):
        if name != "w4k":
            continue
        for r in rs:
            d = os.path.join(a.lunix, "raw", r["cell"].replace("/", "_"))
            ser = series(d)
            f0 = os.path.join(d, "status-before.json")
            if not ser or not os.path.exists(f0):
                continue
            before = json.load(open(f0)).get("stats", {}).get("ops_write", 0)
            x = ser[0]
            ops = (x.get("status") or {}).get("stats", {}).get("ops_write")
            if ops is None:
                continue
            t = 6 + float(x.get("t", 2))
            print(f"| {ck} KiB | {r['cell'].split('/')[-1]} | {before:,} | {ops:,} | {t:.0f} | {(ops - before) / t:,.0f} | {fmt(fnum(r.get('iops')))} | {(x.get('status') or {}).get('journal_used', '')} |")
    print("\n## per-cell detail\n")
    print("| cell | rep | IOPS | MB/s | commits | stage ms (hash; dedup; compress; seal; append) | ring waiters / longest wait ms (end) | window hits / misses | last cut bytes / ms (end) | journal used end | throttled ms / stops / delayed writes (over the cell) | gc pressure end |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for (ck, name, q), rs in sorted(cells.items()):
        for r in rs:
            wp = (r.get("write_path_end") or ",,,,,,").split(",")
            stg = [fnum(x) for x in (r.get("stage_ns_delta") or "").split(",") if x != ""]
            stgs = "; ".join(fmt(x / 1e6) for x in stg) if stg else "-"
            print(f"| {r['cell']} | {r['rep']} | {fmt(fnum(r.get('iops')))} | {fmt((fnum(r.get('bw_bytes')) or 0) / 1e6, 1)} | {r.get('commits_delta', '')} | {stgs} | "
                  f"{wp[0]} / {wp[1]} | {wp[3]} / {wp[4]} | {wp[5]} / {wp[6]} | {r.get('journal_used_end', '')} | {thr(r)} |")
    # time series: ring waiters, window, cut bytes and ms, per sample, first repetition of the write cells
    print("\n## counters over time (first repetition of the write cells, sampler)\n")
    for (ck, name, q), rs in sorted(cells.items()):
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
