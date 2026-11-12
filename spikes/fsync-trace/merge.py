#!/usr/bin/env python3
"""merge.py RAW_DIR : combine *.summary.csv into all.csv and print markdown tables."""
import csv, glob, sys, statistics as st, collections
d = sys.argv[1]
rows = []
for f in sorted(glob.glob(d + "/*.summary.csv")): rows += list(csv.DictReader(open(f)))
keys = []
for r in rows:
    for k in r:
        if k not in keys: keys.append(k)
with open(d + "/all.csv", "w") as f:
    w = csv.DictWriter(f, fieldnames=keys, restval=0); w.writeheader(); w.writerows(rows)
g = collections.defaultdict(list)
for r in rows: g[(r["scen"], r["fs"])].append(r)
fl = lambda r, k: float(r.get(k) or 0)
def mean(rs, k): return st.mean(fl(r, k) for r in rs)
def med(rs, k): return st.median(fl(r, k) for r in rs)
def mx(rs, k): return max(fl(r, k) for r in rs)
kib = lambda v: "%.1f" % (v / 1024)
print("| scen | fs | reps | PF+FUA bios/s | PF-only bios/s | FUA-w/o-PF bios/s | FLUSH rq/s | FUA wr rq/s | FUA rq w/o preceding FLUSH rq | write MB/s |")
print("|---|---|---|---|---|---|---|---|---|---|")
for (s, fs), rs in sorted(g.items()):
    pf_only = mean(rs, "bio_empty_flush_per_s") + mean(rs, "bio_pf_data_per_s")
    print("| %s | %s | %d | %.1f | %.1f | %.1f | %.1f | %.1f | %.0f | %.1f |" % (s, fs, len(rs), mean(rs, "bio_pf_fua_per_s"), pf_only,
          mean(rs, "bio_fua_only_per_s"), mean(rs, "rq_flush_per_s"), mean(rs, "rq_fua_w_per_s"), mean(rs, "rq_fua_alone"), mean(rs, "write_MBps")))
print()
print("KiB completed (non-FUA writes) between consecutive flush points; p50 / p90 / max (p50,p90 = median over reps, max = max over reps)")
print("| scen | fs | total | small/db | stream/wal | other (journal+meta) |")
print("|---|---|---|---|---|---|")
for (s, fs), rs in sorted(g.items()):
    cell = lambda n: "%s / %s / %s" % (kib(med(rs, "B_%s_p50" % n)), kib(med(rs, "B_%s_p90" % n)), kib(mx(rs, "B_%s_max" % n)))
    print("| %s | %s | %s | %s | %s | %s |" % (s, fs, cell("total"), cell("small"), cell("stream"), cell("other")))

print()
print("KiB completed between consecutive PREFLUSH-bearing bios only (FUA-only bios ignored); p50 / p90 / max")
print("| scen | fs | total | small/db | stream/wal | other (journal+meta) |")
print("|---|---|---|---|---|---|")
for (s, fs), rs in sorted(g.items()):
    if not any(fl(r, "n_pf_intervals") for r in rs): print("| %s | %s | n/a (no PREFLUSH) | | | |" % (s, fs)); continue
    cell = lambda n: "%s / %s / %s" % (kib(med(rs, "PF_%s_p50" % n)), kib(med(rs, "PF_%s_p90" % n)), kib(mx(rs, "PF_%s_max" % n)))
    print("| %s | %s | %s | %s | %s | %s |" % (s, fs, cell("total"), cell("small"), cell("stream"), cell("other")))
