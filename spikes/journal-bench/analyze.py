#!/usr/bin/env python3
"""Turn results.jsonl into the P0.5 tables and evaluate the plan's decision rule.
Usage: analyze.py results.jsonl > tables.md"""
import collections, json, sys

rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
by = collections.defaultdict(list)
for r in rows:
    by[r["label"]].append(r)

def ident(r):
    return f'{r["name"]}' + (f' B={r["batch"]}' if r["strategy"] in ("group", "linked") else "") + (f' W={r["window_us"]}' if r["window_us"] else "")

def sel(rs, rec, rate, growing=False, bg=False):
    return [r for r in rs if r["rec"] == rec and r["offered"] == rate and bool(r["growing"]) == growing and bool(r["bg_read"]) == bg]

print("# Generated tables\n")
decision = []
for label, rs in by.items():
    print(f"## {label}\n")
    for rec in (4096, 65536):
        for rate in (50000, 0):
            cs = sel(rs, rec, rate)
            if not cs:
                continue
            print(f"### R={rec // 1024} KiB, {'offered 50 K rec/s' if rate else 'closed-loop saturation'}\n")
            print("| strategy | rec/s | MB/s | dur p50 us | p99 | p99.9 | flush p50 us | p99 | flushes/s | rec/flush | CPU % | note |")
            print("|---|---|---|---|---|---|---|---|---|---|---|---|")
            for r in cs:
                print(f'| {ident(r)} | {r["rec_per_s"]:.0f} | {r["mb_per_s"]:.0f} | {r["dur_p50_us"]:.0f} | {r["dur_p99_us"]:.0f} | {r["dur_p999_us"]:.0f} | '
                      f'{r["flush_p50_us"]:.0f} | {r["flush_p99_us"]:.0f} | {r["flushes_per_s"]:.0f} | {r["rec_per_flush"]:.1f} | {r["cpu_pct"]:.0f} | {"SATURATED" if r["saturated"] else ""} |')
            print()
    # decision rule inputs
    rates = sorted({r["offered"] for r in rs if r["offered"] and not r["growing"] and not r["bg_read"]})
    for rec, orate in [(rec, rt) for rec in (4096, 65536) for rt in rates]:
        cs = [r for r in sel(rs, rec, orate) if not r["saturated"]]
        grp = [r for r in cs if r["strategy"] == "group"]
        lnk = [r for r in cs if r["strategy"] == "linked"]
        sat = sel(rs, rec, 0)
        gsat = [r for r in sat if r["strategy"] == "group"]
        lsat = [r for r in sat if r["strategy"] == "linked"]
        if not grp:
            continue
        g = min(grp, key=lambda r: r["dur_p50_us"])
        # linked that cannot sustain the offered load loses by definition: use its best (saturated) cell for the table
        l = min(lnk or [r for r in sel(rs, rec, orate) if r["strategy"] == "linked"], key=lambda r: r["dur_p50_us"])
        lat_ok = bool(lnk) and l["dur_p50_us"] <= 0.9 * g["dur_p50_us"] and l["dur_p99_us"] <= 1.10 * g["dur_p99_us"]
        gs = max(gsat, key=lambda r: r["rec_per_s"]) if gsat else None
        ls = max(lsat, key=lambda r: r["rec_per_s"]) if lsat else None
        thr_ok = bool(gs and ls and ls["rec_per_s"] >= 1.10 * gs["rec_per_s"] and ls["dur_p99_us"] <= gs["dur_p99_us"])
        dsy = [r for r in cs if r["strategy"] == "dsync"]
        b1 = [r for r in grp if r["batch"] == 1 and r["window_us"] == 0]
        dsync_ok = bool(dsy and b1 and dsy[0]["dur_p50_us"] <= 1.10 * b1[0]["dur_p50_us"])
        gr = [r for r in sel(rs, rec, 50000, growing=True) if r["strategy"] == "group"]
        pg = [r for r in sel(rs, rec, 50000) if r["strategy"] == "group" and r["batch"] == 16 and not r["window_us"]]
        grow = (gr[0]["dur_p99_us"] / pg[0]["dur_p99_us"]) if gr and pg and pg[0]["dur_p99_us"] else None
        decision.append((label, str(rec // 1024) + 'K@' + str(orate), ident(g), g, ident(l), l, lat_ok, thr_ok, ls, gs, dsync_ok, grow))

print("# Decision rule inputs (P0.5)\n")
print("| fs config | R @ offered rec/s | best group (p50/p99 us) | best linked (p50/p99 us) | linked p50/group p50 | latency clause | saturation clause (linked vs group rec/s, p99) | O_DSYNC/RWF_DSYNC within 10% of group B=1 | growing-file p99 / prealloc p99 (group B=16) |")
print("|---|---|---|---|---|---|---|---|---|")
for (label, rec, gi, g, li, l, lat_ok, thr_ok, ls, gs, dsync_ok, grow) in decision:
    print(f'| {label} | {rec} | {gi} ({g["dur_p50_us"]:.0f}/{g["dur_p99_us"]:.0f}) | {li} ({l["dur_p50_us"]:.0f}/{l["dur_p99_us"]:.0f}) | '
          f'{l["dur_p50_us"] / g["dur_p50_us"]:.2f}{"" if l["saturated"] is False else " (linked saturated)"} | {"yes" if lat_ok else "no"} | '
          f'{"yes" if thr_ok else "no"} ({(ls["rec_per_s"] / gs["rec_per_s"]) if ls and gs else float("nan"):.2f}x, {ls["dur_p99_us"] if ls else float("nan"):.0f} vs {gs["dur_p99_us"] if gs else float("nan"):.0f}) | '
          f'{"yes" if dsync_ok else "no"} | {("%.2f" % grow) if grow else "n/a"} |')
print("\n# Recommended primitive per file system\n")
print("| fs config | linked wins at all of R=4 KiB and 64 KiB (latency or throughput clause) | recommendation |")
print("|---|---|---|")
for label in by:
    ds = [d for d in decision if d[0] == label]
    if len(ds) < 2:
        continue
    win = all(d[6] or d[7] for d in ds)
    print(f"| {label} | {'yes' if win else 'no'} | {'linked WRITE->FSYNC' if win else 'group commit'} |")

print("\n# Saturation comparison (closed loop; rule: linked if >= 10 % more records/s at equal p99)\n")
print("| fs config | R | best group (rec/s, p50/p99 us) | best linked (rec/s, p50/p99 us) | linked/group rec/s | linked p99 <= group p99 | clause met |")
print("|---|---|---|---|---|---|---|")
for label, rs in by.items():
    for rec in (4096, 65536):
        sat = sel(rs, rec, 0)
        gs = [r for r in sat if r["strategy"] == "group"]
        ls = [r for r in sat if r["strategy"] == "linked"]
        if not gs or not ls:
            continue
        g = max(gs, key=lambda r: r["rec_per_s"])
        l = max(ls, key=lambda r: r["rec_per_s"])
        ratio = l["rec_per_s"] / g["rec_per_s"]
        p99ok = l["dur_p99_us"] <= g["dur_p99_us"]
        print(f'| {label} | {rec // 1024} KiB | {ident(g)} ({g["rec_per_s"]:.0f}, {g["dur_p50_us"]:.0f}/{g["dur_p99_us"]:.0f}) | {ident(l)} ({l["rec_per_s"]:.0f}, {l["dur_p50_us"]:.0f}/{l["dur_p99_us"]:.0f}) | '
              f'{ratio:.2f} | {"yes" if p99ok else "no"} | {"YES" if ratio >= 1.10 and p99ok else "no"} |')
