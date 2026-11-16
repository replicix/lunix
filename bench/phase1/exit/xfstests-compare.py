#!/usr/bin/env python3
"""Compare xfstests runs of tests/exit/xfstests.sh: Lunix against the loop reference of the same session.

  xfstests-compare.py --ref REF_OUT_DIR --lunix LUNIX_OUT_DIR [--lunix DIR ...] [--md OUT.md] [--json OUT.json]

Each directory is the OUT directory of one xfstests.sh run (it holds check.stdout, local.config, results/). A Lunix
run is compared with the reference run of the same file system. Gate (plan M15 item 2, Spec gaps 17): every test that
passes on the reference passes on Lunix; tests that fail on both are listed with their reasons; a test that fails only
on Lunix is a defect. A test that is `not run` on Lunix but ran on the reference is reported separately (a missing
capability is not a pass). Exit status 0 when no Lunix-only failure and no test that passed on the reference is missing.
"""
import argparse
import json
import os
import re
import sys


def summary_line(text, key):
    m = re.findall(r"^%s:(.*)$" % re.escape(key), text, re.M)
    return m[-1].split() if m else []


def parse(d):
    out = open(os.path.join(d, "check.stdout"), errors="replace").read()
    cfg = open(os.path.join(d, "local.config"), errors="replace").read()
    fs = re.search(r"FSTYP=(\S+)", cfg).group(1)
    ran, notrun, bad = summary_line(out, "Ran"), summary_line(out, "Not run"), summary_line(out, "Failures")
    res = os.path.join(d, "results")
    reasons = {}
    for t in bad:
        base = os.path.join(res, t)
        txt = ""
        for suffix, n in ((".out.bad", 14), (".full", 0)):
            p = base + suffix
            if os.path.exists(p) and not txt:
                lines = open(p, errors="replace").read().splitlines()
                txt = "\n".join(lines[:n]) if n else ""
        if not txt:
            m = re.search(r"^%s\s+(.*)$" % re.escape(t), out, re.M)
            txt = m.group(1) if m else ""
        reasons[t] = txt
    nreasons = {}
    for t in notrun:
        p = os.path.join(res, t + ".notrun")
        nreasons[t] = open(p, errors="replace").read().strip() if os.path.exists(p) else ""
    return {"fs": fs, "ran": set(ran), "notrun": set(notrun), "fail": set(bad), "reasons": reasons, "nreasons": nreasons,
            "dir": d, "complete": bool(re.search(r"^(Passed all|Failed \d+ of)", out, re.M))}


def one_line(s, n=160):
    s = " ".join(s.split())
    return s if len(s) <= n else s[: n - 1] + "..."


def compare(ref, lun):
    both_pass = (ref["ran"] - ref["fail"]) & (lun["ran"] - lun["fail"])
    ref_pass = ref["ran"] - ref["fail"]
    lunix_only = sorted(lun["fail"] - ref["fail"] - (ref["notrun"] - ref["ran"]))
    both_fail = sorted(lun["fail"] & ref["fail"])
    ref_only = sorted(ref["fail"] - lun["fail"])
    missing = sorted(ref_pass - (lun["ran"] - lun["fail"]) - set(lunix_only))
    return {"both_pass": len(both_pass), "both_fail": both_fail, "lunix_only": lunix_only, "ref_only": ref_only,
            "missing_on_lunix": missing, "ref_ran": len(ref["ran"]), "lunix_ran": len(lun["ran"])}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ref", required=True, action="append", help="reference OUT dir (once per file system)")
    ap.add_argument("--lunix", required=True, action="append")
    ap.add_argument("--md")
    ap.add_argument("--json")
    ap.add_argument("--write-expected", metavar="DIR", help="write expected-fail.<fs> files from the reference failures")
    a = ap.parse_args()
    refs = {}
    for d in a.ref:
        r = parse(d)
        refs[r["fs"]] = r
    md, js, rc = [], {}, 0
    md.append("| lunix run | fs | reference ran / failed | lunix ran / failed | both pass | both fail | lunix only (defects) | passes on ref, not on lunix | complete |")
    md.append("|---|---|---|---|---|---|---|---|---|")
    detail = []
    for d in a.lunix:
        l = parse(d)
        r = refs.get(l["fs"])
        if r is None:
            md.append(f"| {os.path.basename(d)} | {l['fs']} | (no reference run) | | | | | | |")
            rc = 1
            continue
        c = compare(r, l)
        js[d] = c
        name = os.path.basename(d.rstrip("/"))
        md.append(f"| {name} | {l['fs']} | {c['ref_ran']} / {len(r['fail'])} | {c['lunix_ran']} / {len(l['fail'])} | {c['both_pass']} | "
                  f"{len(c['both_fail'])} | {len(c['lunix_only'])} | {len(c['missing_on_lunix'])} | {l['complete'] and r['complete']} |")
        if c["lunix_only"] or c["missing_on_lunix"] or not (l["complete"] and r["complete"]):
            rc = 1
        detail.append(f"\n### {name} ({l['fs']})\n")
        if c["lunix_only"]:
            detail.append("**Fails only on Lunix (defects; attach the `results/` excerpts):**\n")
            for t in c["lunix_only"]:
                detail.append(f"- `{t}`: {one_line(l['reasons'].get(t, ''))}")
        if c["missing_on_lunix"]:
            detail.append("\n**Passes on the reference but did not run or pass on Lunix:**\n")
            for t in c["missing_on_lunix"]:
                detail.append(f"- `{t}`: {one_line(l['nreasons'].get(t, 'not run'))}")
        if c["both_fail"]:
            detail.append("\n**Fails on both (reasons from the reference run):**\n")
            for t in c["both_fail"]:
                detail.append(f"- `{t}`: {one_line(r['reasons'].get(t, ''))}")
        if c["ref_only"]:
            detail.append("\nFails on the reference only (noise or a difference of the reference): " + ", ".join(f"`{t}`" for t in c["ref_only"]))
    text = "\n".join(md + detail) + "\n"
    print(text)
    if a.md:
        open(a.md, "w").write(text)
    if a.json:
        json.dump(js, open(a.json, "w"), indent=1)
    if a.write_expected:
        for fs, r in refs.items():
            with open(os.path.join(a.write_expected, f"expected-fail.{fs}"), "w") as f:
                f.write(f"# failing on the loop reference ({fs}), from {r['dir']}\n")
                for t in sorted(r["fail"]):
                    f.write(f"{t}  # {one_line(r['reasons'].get(t, ''), 100)}\n")
    return rc


if __name__ == "__main__":
    sys.exit(main())
