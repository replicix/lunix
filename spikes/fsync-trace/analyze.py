#!/usr/bin/env python3
"""Analyse a block-event text trace (ftrace trace_pipe lines) for one device.
usage: analyze.py TRACE MAJ:MIN OUT_PREFIX KEY=VAL,... [label=filefrag.map ...]
Writes OUT_PREFIX.summary.csv (one row), OUT_PREFIX.flushes.csv, OUT_PREFIX.seq.txt, OUT_PREFIX.rwbs.txt"""
import re, sys, bisect, collections, csv

trace, dev, outp, meta = sys.argv[1], sys.argv[2], sys.argv[3], dict(kv.split("=", 1) for kv in sys.argv[4].split(",") if kv)
maps = dict(a.split("=", 1) for a in sys.argv[5:])
seq_label = meta.get("seqlabel", "small")

# extents: (start_sector, end_sector, label)
ext = []
for label, f in maps.items():
    try: txt = open(f).read()
    except OSError: continue
    for m in re.finditer(r"^\s*\d+:\s+\d+\.\.\s*\d+:\s+(\d+)\.\.\s*(\d+):", txt, re.M):
        ext.append((int(m.group(1)), int(m.group(2)) + 1, label))
ext.sort(); starts = [e[0] for e in ext]

def attribute(s, n):
    """split [s, s+n) sectors by label -> dict label->sectors"""
    out = collections.Counter(); e = s + n
    i = max(bisect.bisect_right(starts, s) - 1, 0)
    cur = s
    while i < len(ext) and ext[i][0] < e and cur < e:
        a, b, l = ext[i]
        if b > cur:
            if a > cur: out["other"] += a - cur; cur = a
            hi = min(b, e); out[l] += hi - cur; cur = hi
        i += 1
    if cur < e: out["other"] += e - cur
    return out

def parse_rwbs(r):
    pre = False
    if len(r) > 1 and r[0] == "F" and r[1] in "WRDZN": pre = True; r = r[1:]
    op = r[0] if r else "?"; rest = r[1:]
    fua = op == "W" and "F" in rest
    if op == "F": op = "FLUSH"
    return pre, op, fua

rx = re.compile(r"\]\s+\S+\s+(\d+\.\d+): (block_\w+): (\d+),(\d+) (\S+) (.*)$")
rq_tail = re.compile(r"(?:\d+ )?\(.*?\) (\d+) \+ (\d+)")
bio_tail = re.compile(r"(\d+) \+ (\d+)")
cm_tail = re.compile(r"\(.*?\) (\d+) \+ (\d+)")
want = tuple(int(x) for x in dev.split(":"))
ev = []  # (ts, kind, rwbs, sector, nsect)
rwbs_seen = collections.Counter()
for line in open(trace, errors="replace"):
    m = rx.search(line)
    if not m: continue
    ts, name, ma, mi, rw, tail = m.groups()
    if (int(ma), int(mi)) != want: continue
    t = (rq_tail if name == "block_rq_issue" else cm_tail if name == "block_rq_complete" else bio_tail).search(tail)
    if not t: continue
    kind = {"block_bio_queue": "bio", "block_rq_issue": "iss", "block_rq_complete": "cmp"}.get(name)
    if not kind: continue
    ev.append((float(ts), kind, rw, int(t.group(1)), int(t.group(2))))
    rwbs_seen[(kind, rw)] += 1
ev.sort(key=lambda e: e[0])
if not ev: sys.exit("no events")

def pct(v, p):
    if not v: return 0
    v = sorted(v); return v[min(len(v) - 1, int(round(p * (len(v) - 1))))]

c = collections.Counter()
points = []  # bio-level flush points (ts, kind)
prev_iss = None; paired = 0
comp = []    # (ts, bytes by label) for volatile data-write completions
for ts, kind, rw, s, n in ev:
    pre, op, fua = parse_rwbs(rw)
    empty = n == 0
    if kind == "bio":
        if pre and fua: c["bio_pf_fua"] += 1
        elif pre and (empty or op == "FLUSH"): c["bio_empty_flush"] += 1
        elif pre: c["bio_pf_data"] += 1
        elif fua: c["bio_fua_only"] += 1
        if pre or fua: points.append((ts, "PF+FUA" if pre and fua else "PF" if pre else "FUA"))
        if op == "W" and not empty: c["bio_write_bytes"] += n * 512
    elif kind == "iss":
        if op == "FLUSH" or (empty and "F" in rw): c["rq_flush"] += 1
        elif op == "W" and fua:
            c["rq_fua_w"] += 1
            if prev_iss and prev_iss[0] and ts - prev_iss[1] < 0.005: paired += 1
        elif op == "W": c["rq_plain_w"] += 1
        if op == "W" and not empty: c["rq_w_bytes"] += n * 512
        prev_iss = (op == "FLUSH" or (empty and "F" in rw), ts)
    else:
        if op == "W" and not fua and not empty:
            comp.append((ts, attribute(s, n)))
c["rq_fua_after_flush"] = paired
c["rq_fua_alone"] = c["rq_fua_w"] - paired

# bytes completed between consecutive flush points
cts = [x[0] for x in comp]
rows = []
for i in range(1, len(points)):
    lo = bisect.bisect_right(cts, points[i - 1][0]); hi = bisect.bisect_right(cts, points[i][0])
    tot = collections.Counter()
    for _, a in comp[lo:hi]: tot.update(a)
    rows.append((points[i][0], points[i][1], sum(tot.values()) * 512, tot))
labs = sorted({l for r in rows for l in r[3]} | {"other"})
with open(outp + ".flushes.csv", "w") as f:
    w = csv.writer(f); w.writerow(["t_s", "kind", "bytes_total"] + ["bytes_" + l for l in labs])
    t0 = ev[0][0]
    for ts, k, tb, tot in rows: w.writerow(["%.6f" % (ts - t0), k, tb] + [tot[l] * 512 for l in labs])

secs = float(meta.get("secs", 1))
res = dict(meta); res.update({k: c[k] for k in ["bio_pf_fua", "bio_empty_flush", "bio_pf_data", "bio_fua_only", "rq_flush", "rq_fua_w", "rq_fua_after_flush", "rq_fua_alone", "rq_plain_w"]})
for k in ["bio_pf_fua", "bio_empty_flush", "bio_pf_data", "bio_fua_only", "rq_flush", "rq_fua_w"]: res[k + "_per_s"] = round(c[k] / secs, 1)
res["flush_points_per_s"] = round(len(points) / secs, 1)
res["write_MBps"] = round(c["rq_w_bytes"] / secs / 1e6, 1)
for name, sel in [("total", lambda r: r[2]), ] + [(l, (lambda l: lambda r: r[3][l] * 512)(l)) for l in labs]:
    v = [sel(r) for r in rows]
    res["B_%s_p50" % name] = pct(v, .5); res["B_%s_p90" % name] = pct(v, .9); res["B_%s_max" % name] = max(v) if v else 0
res["n_intervals"] = len(rows)
# same distribution but only between PREFLUSH-bearing points (= cache flushes the device must honour); FUA-only bios ignored
pfpts = [p for p in points if p[1] != "FUA"]
prow = []
for i in range(1, len(pfpts)):
    lo = bisect.bisect_right(cts, pfpts[i - 1][0]); hi = bisect.bisect_right(cts, pfpts[i][0])
    tot = collections.Counter()
    for _, a in comp[lo:hi]: tot.update(a)
    prow.append(tot)
for name in ["total"] + labs:
    v = [(sum(t.values()) if name == "total" else t[name]) * 512 for t in prow]
    res["PF_%s_p50" % name] = pct(v, .5); res["PF_%s_p90" % name] = pct(v, .9); res["PF_%s_max" % name] = max(v) if v else 0
res["n_pf_intervals"] = len(prow)
with open(outp + ".summary.csv", "w") as f:
    w = csv.DictWriter(f, fieldnames=list(res)); w.writeheader(); w.writerow(res)
with open(outp + ".rwbs.txt", "w") as f:
    for (k, r), n in sorted(rwbs_seen.items()): f.write("%s %s %d\n" % (k, r, n))

# one representative cycle: from just after one PREFLUSH point to just after the next (middle of the run)
lab_of = lambda s, n: ",".join(sorted(attribute(s, n))) if n else "-"
lines = []
pfs = [p[0] for p in points if p[1] != "FUA"]
if len(pfs) > 20:
    k = len(pfs) // 2; a, b = pfs[k - 1] + 5e-4, pfs[k] + 5e-4
    for ts, kind, rw, s, n in ev:
        if a <= ts < b:
            pre, op, fua = parse_rwbs(rw)
            if kind == "cmp" and not (pre or fua or op == "FLUSH" or n == 0): continue
            lines.append("%8.0fus %-3s %-5s sec=%-9d n=%-5d %s" % ((ts - a) * 1e6, kind, rw, s, n, lab_of(s, n)))
with open(outp + ".seq.txt", "w") as f: f.write("\n".join(lines[:70]) + "\n")
