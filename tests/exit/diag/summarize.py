#!/usr/bin/env python3
"""summarize.py DIAG_DIR CELL_RAW_DIR PREFIX: per profile of a fio-verify cell, what the daemon, the kernel and fio saw.

DIAG_DIR  = output of diag-sidecar.sh;  CELL_RAW_DIR = the cell's raw/ (fio-<PREFIX>-p<i>.json);  PREFIX like fio-verify_xfs_c16k_integrity.
For every profile (window = fio's timestamp minus its runtime): seconds per gate state, mean admitted rate, bytes written per second, kernel writes in flight
(of the tags fio can reach), the share of fio's outstanding writes that wait outside the device (tag wait), the daemon's own latency histogram
(arrival to acknowledgement) of the window, kernel dispatch->complete events over the limit, fio's completion latency maximum and percentiles."""
import json, os, re, sys, collections

diag, raw, prefix = sys.argv[1:4]


def load_samples():
    rows, cur = [], None
    for l in open(os.path.join(diag, "samples.txt"), errors="replace"):
        if l.startswith("T "):
            try:
                cur = {"t": float(l[2:]), "S": None, "I": None}
                rows.append(cur)
            except ValueError:
                cur = None
        elif cur is None:
            continue
        elif l.startswith("S "):
            try:
                cur["S"] = json.loads(l[2:])
            except Exception:
                pass
        elif l.startswith("I "):
            p = l.split()
            try:
                cur["I"] = (int(p[2]), int(p[3]))
            except Exception:
                pass
    return [r for r in rows if r["S"] and r["S"].get("bw") is not None]


def fio_job(p):
    t = open(os.path.join(raw, "fio-%s-p%d.json" % (prefix, p))).read()
    i = t.index('"fio version"'); i = t.rindex("{", 0, i)
    return json.loads(t[i:])


rows = load_samples()
t0 = rows[0]["t"]
bpf0 = float(os.popen("stat -c %W " + os.path.join(diag, "bpf.txt")).read().strip() or t0)
longs = []
for l in open(os.path.join(diag, "bpf.txt"), errors="replace"):
    m = re.match(r"(\d+) LONG dispatch_to_complete (\d+) ms", l)
    if m:
        longs.append((bpf0 + int(m.group(1)) / 1000.0, int(m.group(2))))
for p in range(6):
    try:
        d = fio_job(p)
    except Exception:
        break
    end = d["timestamp"] if "timestamp" in d else None
    jobs = [j for j in d["jobs"] if j["jobname"] == "main"]
    rt = max(j["job_runtime"] for j in jobs) / 1000.0
    outstanding = int(os.environ["OUTSTANDING"]) if os.environ.get("OUTSTANDING") else [1, 32, 128, 8, 256, 1024][p]
    lat = max(j["write"]["lat_ns"]["max"] for j in jobs) / 1e9
    pct = {}
    for k in ("99.000000", "99.900000", "99.990000"):
        pct[k] = max(j["write"]["clat_ns"]["percentile"].get(k, 0) for j in jobs if "percentile" in j["write"]["clat_ns"]) / 1e9
    ts = d["timestamp"]
    life = max(j["job_runtime"] for j in d["jobs"]) / 1000.0   # the trim companion keeps the process alive
    lo, hi = ts - life, ts - life + rt   # fio's timestamp is taken at the end of the process
    w = [r for r in rows if lo <= r["t"] <= hi]
    if len(w) < 2:
        print("p%d: no samples in the window" % p); continue
    st = collections.Counter(); sec = bw = rate = infl = 0.0; n = 0
    for a, b in zip(w, w[1:]):
        dt = b["t"] - a["t"]
        if dt <= 0 or dt > 3:
            continue
        st[b["S"]["state"]] += dt; sec += dt; bw += b["S"]["bw"] - a["S"]["bw"]; rate += (b["S"]["rate"] or 0) * dt
        if b["I"]:
            infl += b["I"][1] * dt
    h0, h1 = w[0]["S"]["lat"], w[-1]["S"]["lat"]
    hd = [x - y for x, y in zip(h1, h0)]
    top = max([i for i, v in enumerate(hd) if v > 0] or [0])
    ln = [m for t, m in longs if lo <= t <= hi]
    print("p%d  window %.0f s  states %s" % (p, sec, {k: round(v) for k, v in st.items()}))
    print("    written %.1f MB/s, gate rate (mean) %.1f MB/s, kernel writes in flight (mean) %.0f, fio max lat %.1f s, clat p99 %.2f p99.9 %.2f p99.99 %.2f s"
          % (bw / sec / 1e6, rate / sec / 1e6, infl / sec, lat, pct["99.000000"], pct["99.900000"], pct["99.990000"]))
    fl = infl / sec
    print("    outstanding writes of fio %d, in the device %.0f: %.0f %% wait outside the device for a tag (Little)" % (outstanding, fl, max(0.0, 100.0 * (1 - fl / outstanding))))
    print("    daemon histogram top bucket 2^%d us (%.1f-%.1f s), writes in buckets >= 2^22 us (4 s): %d of %d; kernel dispatch->complete over limit: %d events, max %d ms"
          % (top, 2 ** (top - 1) / 1e6, 2 ** top / 1e6, sum(hd[22:]), sum(hd), len(ln), max(ln) if ln else 0))
