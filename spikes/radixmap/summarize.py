#!/usr/bin/env python3
"""Summarise raw/*.csv into the medians (min-max) tables used in RESULTS.md."""
import collections, glob, os, statistics as S, sys

RAW = sys.argv[1] if len(sys.argv) > 1 else "raw"


def load(path):
    hdr, rows = None, []
    for line in open(path):
        line = line.rstrip("\n")
        if line.startswith(("system,", "tag,")):
            hdr = line.split(",")
            continue
        p = line.split(",", len(hdr) - 1) if hdr else []
        if hdr and len(p) == len(hdr) and p[0] != "flat-full":
            rows.append(dict(zip(hdr, p)))
    return rows


def seal(name):
    g = collections.defaultdict(list)
    for r in load(os.path.join(RAW, name)):
        g[(r["system"], r["pattern"], int(r["D"]))].append(r)
    print(f"\n### {name}\n")
    print("| system | pattern | D | seal ms med (min-max) | set ms | BLAKE3 share | leaves | interior | raw MiB | stored MiB | KiB/leaf | stored / dirty data | peak RSS MiB |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for (sy, pat, d), v in sorted(g.items(), key=lambda kv: (kv[0][1], kv[0][2], kv[0][0])):
        m = lambda c: S.median(float(x[c]) for x in v)
        lo = min(float(x["seal_ms"]) for x in v)
        hi = max(float(x["seal_ms"]) for x in v)
        print(f"| {sy} | {pat} | {d} | {m('seal_ms'):.1f} ({lo:.1f}-{hi:.1f}) | {m('set_ms'):.0f} | {m('hash_share'):.2f} | {m('leaves'):.0f} | {m('interior_nodes'):.0f} | {m('raw_mib'):.2f} | {m('stored_mib'):.2f} | {m('leaf_stored_kib_per_leaf'):.1f} | {m('amp_stored_over_dirty_data'):.3f} | {m('peak_rss_mib'):.0f} |")


def diff(name):
    rows = load(os.path.join(RAW, name))
    print(f"\n### {name}\n")
    print("| k | diff ms warm med (min-max) | nodes read | bound 2*k*depth | entry-level diff ms (cold leaves / warm) | flat compare (manifest ms; blocks read; block compare ms) |")
    print("|---|---|---|---|---|---|")
    g = collections.defaultdict(list)
    for r in rows:
        g[int(r["k"])].append(r)
    for k, v in sorted(g.items()):
        t = [float(x["diff_ms_warm"]) for x in v]
        # the last column holds "flatNB,manifest_ms,block_cmp_ms,blocks,rows;..." per block size
        extra = ",".join(v[0][h] for h in ("flat_manifest_cmp_ms", "flat_block_cmp_ms", "flat_blocks_read", "flat_rows_differing"))
        print(f"| {k} | {S.median(t):.3f} ({min(t):.3f}-{max(t):.3f}) | {v[0]['nodes_read']} | {v[0]['bound_2kd']} | {S.median(float(x['entries_diff_ms_first']) for x in v):.1f} / {S.median(float(x['entries_diff_ms_second']) for x in v):.1f} | {extra} |")


if __name__ == "__main__":
    for f in sorted(glob.glob(os.path.join(RAW, "seal-*.csv"))):
        seal(os.path.basename(f))
    for f in sorted(glob.glob(os.path.join(RAW, "diff-*.csv"))):
        diff(os.path.basename(f))
