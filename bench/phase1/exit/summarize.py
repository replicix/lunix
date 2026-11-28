#!/usr/bin/env python3
"""RESULTS.md tables of the M15 exit runs.

  summarize.py OUT_DIR [OUT_DIR ...] > tables.md

Each OUT_DIR is the output directory of one script of tests/exit/ (results.jsonl, raw/). The generator prints:
fio verify cells, crash loops (phases and stall times), crash points (per point and hit), the ratio tables with the
gate of item 4 applied, negative controls, and the overall verdict line per section. Nothing is hidden: failures,
skips and `unreached` points are rows. The scratch directory path is replaced by <scratch> in the output.
"""
import glob
import json
import os
import statistics
import sys


def load(d):
    rows = []
    p = os.path.join(d, "results.jsonl")
    if os.path.exists(p):
        for line in open(p):
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def jload(p):
    try:
        t = open(p).read()
        i = t.find("{")
        j = t.find("[")
        k = min(x for x in (i, j) if x >= 0) if (i >= 0 or j >= 0) else -1
        return json.loads(t[k:]) if k >= 0 else None
    except (OSError, ValueError):
        return None


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))] if v else float("nan")


def gib(n):
    return f"{n / 2**30:.2f}" if n is not None else "n/a"


def table(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def section_fio(rows):
    r = [x for x in rows if x["cell"].startswith("fio-verify/")]
    if not r:
        return ""
    t = [(x["cell"], x["status"], x.get("profiles", ""), x.get("runtime", ""), x.get("secs", ""),
          gib(int(x["store_bytes_after_runs"])) if x.get("store_bytes_after_runs") else "", x.get("notes", "")) for x in r]
    verdict = "pass" if all(x["status"] == "pass" for x in r) else "FAIL"
    return f"## fio verify (item 1): {verdict}\n\n" + table(["cell", "status", "profiles done", "runtime s", "secs", "store GiB after runs", "notes"], t) + "\n"


def section_crash(rows, d):
    r = [x for x in rows if x["cell"].startswith("crash-")]
    if not r:
        return ""
    t = [(x["cell"], x["status"], x.get("cycles", ""), x.get("fio_rc", ""), x.get("fio_errors", ""), x.get("guest_fsck", ""),
          x.get("fsyncer_verify", ""), x.get("lunix_fsck", ""), x.get("aborted", "")) for x in r]
    out = "## crash loops (item 3a, 3c)\n\n" + table(["phase", "status", "cycles", "fio rc", "fio errors", "guest fsck", "fsyncer", "lunix fsck", "aborted"], t) + "\n"
    st = os.path.join(d, "stall-table.md")
    if os.path.exists(st):
        out += "\nStall times (heartbeat gaps joined with the kill and ready times):\n\n" + open(st).read()
    sm = os.path.join(d, "daemon-stall-ms.txt")
    if os.path.exists(sm):
        v = [float(x) for x in open(sm).read().split()]
        if v:
            out += f"\nDaemon-logged recovery stall (ms), n={len(v)}: p50 {pct(v, 50):.0f}, p99 {pct(v, 99):.0f}, max {max(v):.0f}\n"
    return out


def section_points(rows):
    r = [x for x in rows if x["cell"].startswith("crashpoint/")]
    if not r:
        return ""
    t = [(x.get("point", x["cell"]), x.get("kind", ""), x.get("hit", ""), x.get("fs", ""), x["status"], x.get("crash_after_s", ""),
          x.get("recovery_stall_ms", ""), x.get("secs", ""), x.get("notes", x.get("why", ""))) for x in r]
    n = {}
    for x in r:
        n[x["status"]] = n.get(x["status"], 0) + 1
    return ("## crash points (item 3b)\n\nruns by status: " + ", ".join(f"{k} {v}" for k, v in sorted(n.items())) + "\n\n"
            + table(["point", "kind", "hit", "fs", "status", "crashed after s", "recovery stall ms", "secs", "notes"], t) + "\n")


def ratio_row(x, d):
    cd = x.get("dir")
    base = os.path.join(d, "raw", os.path.basename(cd.rstrip("/"))) if cd else None
    if not base or not os.path.isdir(base):
        return None
    fsck = jload(os.path.join(base, "fsck.json")) or {}
    ra = fsck.get("ratios") or {}
    st = jload(os.path.join(base, "status-attached.json")) or {}
    stats = st.get("stats") or {}
    cs = jload(os.path.join(base, "chunkstat.json")) or {}
    name = x.get("cell_name", "")
    chunk = int(x.get("chunk", 0))
    logical = stats.get("bytes_written")
    mapped = ra.get("mapped_chunks")
    seg = (ra.get("bytes") or {}).get("segments")
    store = None
    try:
        store = int(open(os.path.join(base, "store-bytes.txt")).read().strip())
    except (OSError, ValueError):
        pass
    c = dd = None
    if name.startswith("fio-c"):
        c = int(name.split("-c")[1].split("-")[0])
        dd = int(name.split("-d")[1])
    exp_d = 1 / (1 - dd / 100) if dd is not None else None
    exp_c = 1 / (1 - c / 100) if c is not None else None
    md = ra.get("dedup_ratio")
    mc = ra.get("compression_ratio")
    okd = (abs(md / exp_d - 1) <= 0.10) if (md and exp_d) else None
    okc = (abs(mc / exp_c - 1) <= 0.15) if (mc and exp_c) else None
    rawstored = None
    if name == "random" and seg and mapped:
        rawstored = seg / (mapped * chunk)
    return {"chunk": chunk, "name": name, "c": c, "d": dd, "exp_d": exp_d, "exp_c": exp_c, "md": md, "mc": mc,
            "okd": okd, "okc": okc, "cs_d": cs.get("dedup_ratio"), "cs_c": cs.get("zlib1_compression_ratio_sample"),
            "st_d": st.get("dedup_ratio"), "st_c": st.get("compression_ratio"), "logical": logical, "mapped": mapped,
            "seg": seg, "store": store, "rawstored": rawstored, "status": x["status"]}


def f(v, n=3):
    return "n/a" if v is None else f"{v:.{n}f}"


def section_ratios(rows, d):
    r = [x for x in rows if x["cell"].startswith("ratios/")]
    if not r:
        return ""
    data = [z for z in (ratio_row(x, d) for x in r if x["status"] != "skip") if z]
    out = ["## ratios (item 4)\n"]
    fails = 0
    for chunk in sorted({z["chunk"] for z in data}):
        out.append(f"\n### chunk {chunk // 1024} KiB\n")
        rows_ = []
        for z in sorted((z for z in data if z["chunk"] == chunk), key=lambda z: z["name"]):
            gate = ""
            if z["okd"] is not None:
                gate += "dedup ok" if z["okd"] else "dedup OUT"
            if z["okc"] is not None:
                gate += (", " if gate else "") + ("compress ok" if z["okc"] else "compress OUT")
            if z["rawstored"] is not None:
                gate = "random stored raw ok" if z["rawstored"] <= 1.01 else "random NOT raw"
                fails += z["rawstored"] > 1.01
            fails += (z["okd"] is False) + (z["okc"] is False)
            rows_.append((z["name"], gib(z["logical"]), f(z["exp_d"], 2), f(z["md"]), f(z["cs_d"]), f(z["st_d"]), f(z["exp_c"], 2),
                          f(z["mc"]), f(z["cs_c"]), f(z["st_c"]), gib(z["seg"]), gib(z["store"]),
                          f(z["rawstored"], 4) if z["rawstored"] is not None else "", gate, z["status"]))
        out.append(table(["cell", "logical GiB (status)", "nominal dedup 1/(1-p)", "dedup (fsck)", "dedup (data read back)", "dedup (status)",
                          "nominal compr", "compr (fsck)", "compr (zlib-1 sample)", "compr (status)", "segment GiB (fsck)", "store GiB (du)",
                          "stored/logical (random)", "gate", "cell status"], rows_))
    skipped = [x for x in r if x["status"] == "skip"]
    if skipped:
        out.append("\nSkipped: " + "; ".join(f"{x['cell']} ({x.get('why', '')})" for x in skipped))
    out.append(f"\nGate of item 4: {'met' if fails == 0 else str(fails) + ' threshold(s) not met (explain in the report)'}; dedup within 10 % of 1/(1-p), compression within 15 % of the knob, random data stored at most 1.01x.\n")
    return "\n".join(out)


def section_neg(rows):
    r = [x for x in rows if x["cell"].startswith("negctl/")]
    if not r:
        return ""
    return "## negative controls\n\n" + table(["control", "status"], [(x["cell"], x["status"] + " (pass = the control tripped as designed)") for x in r]) + "\n"


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 64
    out = []
    scratch = []
    for d in sys.argv[1:]:
        rows = load(d)
        scratch.append(os.path.dirname(os.path.abspath(d)))
        out.append(f"# results of {os.path.basename(os.path.abspath(d))}\n")
        for fn in (lambda: section_fio(rows), lambda: section_crash(rows, d), lambda: section_points(rows),
                   lambda: section_ratios(rows, d), lambda: section_neg(rows)):
            s = fn()
            if s:
                out.append(s)
        lo = os.path.join(d, "leftovers-end.txt")
        if os.path.exists(lo):
            out.append("Leftovers check at the end:\n\n```\n" + open(lo).read().strip() + "\n```\n")
    text = "\n".join(out)
    for s in set(scratch):
        text = text.replace(s, "<scratch>")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
