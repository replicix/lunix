#!/usr/bin/env python3
"""analyze.py OUTDIR: a per-second timeline of a diag-sidecar run: daemon status, thread states, kernel long completions.
Columns: t (s from the first sample), gate state, admitted rate MB/s, writes/s, commits delta, last cut ms, journal used MiB, ublk inflight (read write),
coord thread state/wchan, number of threads in D, kernel completions longer than the limit that ended in this second (count, max ms),
daemon histogram's bucket delta for >= 2^22 us (4.2 s+)."""
import json, os, re, sys, collections

d = sys.argv[1]
samples = []
cur = None
for line in open(os.path.join(d, "samples.txt"), errors="replace"):
    line = line.rstrip("\n")
    if line.startswith("T "):
        cur = {"t": float(line[2:]), "S": None, "H": [], "I": None, "D": []}
        samples.append(cur)
    elif cur is None:
        continue
    elif line.startswith("S "):
        try:
            cur["S"] = json.loads(line[2:])
        except Exception:
            pass
    elif line.startswith("H "):
        p = line.split()
        try:
            cur["H"].append((p[1], p[2], p[3], int(p[4]), p[5] if len(p) > 5 else ""))
        except (IndexError, ValueError):
            pass
    elif line.startswith("I "):
        cur["I"] = line.split()[2:]
    elif line.startswith("D "):
        cur["D"].append(line.split()[1:])
t0 = samples[0]["t"]
# bpftrace events: time is ms since its start; its start = birth of bpf.txt, or the first TICK matched to the first sample
bpf0 = None
try:
    bpf0 = float(os.popen("stat -c %W " + os.path.join(d, "bpf.txt")).read().strip())
except Exception:
    pass
longs = []
ticks = []
for line in open(os.path.join(d, "bpf.txt"), errors="replace"):
    m = re.match(r"(\d+) LONG (\w+) (\d+) ms(.*)", line)
    if m:
        longs.append((int(m.group(1)) / 1000.0, m.group(2), int(m.group(3)), m.group(4).strip()))
    m = re.match(r"(\d+) TICK inflight (-?\d+) tagwaiting (-?\d+)", line)
    if m:
        ticks.append((int(m.group(1)) / 1000.0, int(m.group(2)), int(m.group(3))))
off = (bpf0 if bpf0 else t0) - t0  # bpftrace second 0 in sample time
print("# bpftrace start offset vs first sample: %.1f s; %d long events (dispatch_to_complete %d, tagwait %d)" % (off, len(longs),
      sum(1 for l in longs if l[1] == "dispatch_to_complete"), sum(1 for l in longs if l[1] == "tagwait")))
byt = collections.defaultdict(list)
for t, kind, ms, rest in longs:
    byt[int(t + off)].append((kind, ms))
prev = None
print("t  state rateMB/s wr/s dCommits cutms jMiB inflight coord thrD  kLongDisp(n,max) kLongTag(n,max)  dHist>=4.2s")
for s in samples:
    S = s["S"] or {}
    t = int(s["t"] - t0)
    coord = next((("%s/%s" % (h[2], h[4])) for h in s["H"] if h[1] == "lunix-coord"), "-")
    nd = sum(1 for h in s["H"] if h[2] == "D")
    dwr = dc = 0
    hb = 0
    if prev and prev["S"] and S:
        dt = max(0.001, s["t"] - prev["t"])
        dwr = (S.get("wr", 0) - prev["S"].get("wr", 0)) / dt
        dc = S.get("commits", 0) - prev["S"].get("commits", 0)
        a, b = S.get("lat") or [], prev["S"].get("lat") or []
        if a and b:
            hb = sum(a[i] - b[i] for i in range(22, len(a)))
    ev = byt.get(t, [])
    ld = [m for k, m in ev if k == "dispatch_to_complete"]
    lt = [m for k, m in ev if k == "tagwait"]
    print("%d %s %.0f %.0f %d %s %d %s %s %d (%d,%s) (%d,%s) %d" % (
        t, S.get("state", "-"), (S.get("rate") or 0) / 1e6, dwr, dc, S.get("cut_ms"), (S.get("ju") or 0) >> 20,
        "/".join(s["I"] or []), coord, nd, len(ld), max(ld) if ld else "-", len(lt), max(lt) if lt else "-", hb))
    prev = s
