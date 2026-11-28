#!/usr/bin/env python3
"""defcheck: per-block and per-arm tables of the write-only w4k cell under the arms D (shipped defaults), C2 (defaults + condemn-on-cut on) and O (old defaults).

  defcheck.py RESULTS_DIR [--json OUT.json] > tables.md

Blocks are b<NN>-<arm>-65536-w4k (tests/perf/defcheck.sh); per-repetition figures come from c5t6.py (onset, stops, throttle, passes).
"""
import glob
import json
import os
import re
import statistics as st
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import c5t6  # noqa: E402

GiB = 2 ** 30


def med(v):
    v = [x for x in v if x is not None]
    return st.median(v) if v else None


def fmt(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def rep1(r):
    on_ms = r["lo"] + r["onset_s"] * 1000 if r["onset_s"] is not None else None
    cands = [x for x in (r["onset_first_delete_s"], r["onset_first_proposal_delete_s"]) if x is not None]
    gib5 = None
    if on_ms is not None:
        gib5 = sum(p["bytes_freed"] for p in r["passes"] if on_ms <= p["ts"] <= on_ms + 5000) / GiB
    return dict(onset_s=r["onset_s"], first_delete_s=min(cands) if cands else None, stops=r["stops"], wait_s=r["max_s"],
                throttled_s=r["throttled_ms"] / 1000, gib5=gib5, iops=r["iops"], p999_s=r["p999_s"])


def block(run):
    b = c5t6.load_block(run)
    if not b:
        return None
    reps = b["reps"]
    r1 = next((r for r in reps if r["rep"] == 1), None)
    r25 = [r for r in reps if r["rep"] > 1]
    fin = c5t6.jl(os.path.join(run, "raw", "status-final.json")) or {}
    stt = fin.get("stats", {})
    rc = (c5t6.jl(os.path.join(run, "raw", "gc-final.json")) or {}).get("reclaim", {})
    ver = open(os.path.join(run, "verify.txt")).read().strip() if os.path.exists(os.path.join(run, "verify.txt")) else "?"
    log = "".join(open(p, errors="replace").read() for p in sorted(glob.glob(os.path.join(run, "lunixd.*.log"))))
    warn = [ln for ln in log.splitlines() if re.search(r"\bWARN\b|\bERROR\b", ln)]
    return dict(name=b["name"], arm=b["arm"], load=b["load_start"], rep1=rep1(r1) if r1 else None,
                iops25=med([r["iops"] for r in r25]), p999_25=med([r["p999_s"] for r in r25]), wait25=med([r["max_s"] for r in r25]),
                wait25_worst=max([r["max_s"] for r in r25] or [0]), stops25=sum(r["stops"] for r in r25),
                stops_reps25=sum(1 for r in r25 if r["stops"]), throttled25=med([r["throttled_ms"] / 1000 for r in r25]),
                mark_share=st.mean([r["share_mark_pct"] for r in reps]) if reps else None,
                mark_share25=st.mean([r["share_mark_pct"] for r in r25]) if r25 else None,
                gc_share25=st.mean([r["share_pct"] for r in r25]) if r25 else None,
                err=sum(r["err"] for r in reps), verify=ver, fsck=b["fsck_exit"], result=b["result"], eio=b["eio_log"],
                exhausted=stt.get("read_revalidate_exhausted"), auth=stt.get("extent_auth_failures"), revalidated=stt.get("read_revalidated"),
                pgrace_deletes=rc.get("pgrace_deletes"), pgrace_min_wait=rc.get("pgrace_min_wait_ms"), pgrace_bytes=rc.get("pgrace_bytes"),
                warn=len(warn), warn_lines=warn[:4], fulls=sum(r["fulls"] + r["enospc"] for r in reps))


def main():
    run = sys.argv[1]
    blocks = [b for b in (block(d) for d in sorted(glob.glob(os.path.join(run, "b*-w4k")))) if b]
    out = []
    p = out.append
    p("# defcheck tables\n")
    p("## Per block\n")
    p("| block | arm | start load 1/5/15 | rep 1: onset s | first delete s | stops | longest wait s | throttled s | GiB <= 5 s | IOPS | p99.9 s |")
    p("|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        r = b["rep1"] or {}
        p(f"| {b['name']} | {b['arm']} | {'/'.join(str(x) for x in (b['load'] or []))} | {fmt(r.get('onset_s'))} | {fmt(r.get('first_delete_s'), 2)} | {r.get('stops')} | "
          f"{fmt(r.get('wait_s'))} | {fmt(r.get('throttled_s'))} | {fmt(r.get('gib5'), 2)} | {fmt(r.get('iops'), 0)} | {fmt(r.get('p999_s'), 2)} |")
    p("\n| block | arm | reps 2-5 IOPS (med) | p99.9 s (med) | longest wait s (med / worst rep) | stops (reps with a stop) | throttled s (med) | pgrace deletes | pgrace min wait ms | mark share % (all / 2-5) |")
    p("|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        p(f"| {b['name']} | {b['arm']} | {fmt(b['iops25'], 0)} | {fmt(b['p999_25'], 2)} | {fmt(b['wait25'])} / {fmt(b['wait25_worst'])} | {b['stops25']} ({b['stops_reps25']}) | "
          f"{fmt(b['throttled25'])} | {b['pgrace_deletes']} | {b['pgrace_min_wait']} | {fmt(b['mark_share'], 2)} / {fmt(b['mark_share25'], 2)} |")
    p("\n## Safety per block\n")
    p("| block | arm | fio errors | verify (crc32c read-back) | fsck exit | exhausted | auth failures | revalidated | EIO/ENOSPC lines | fulls+enospc | driver result | WARN/ERROR lines |")
    p("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        p(f"| {b['name']} | {b['arm']} | {b['err']} | {b['verify']} | {b['fsck']} | {b['exhausted']} | {b['auth']} | {b['revalidated']} | {b['eio']} | {b['fulls']} | {b['result']} | {b['warn']} |")
    for b in blocks:
        for ln in b["warn_lines"]:
            p(f"\n- {b['name']}: `{ln[:200]}`")
    arms = {}
    for b in blocks:
        arms.setdefault(b["arm"], []).append(b)
    p("\n## Per arm: median over blocks (worst block in brackets)\n")
    p("| metric | " + " | ".join(f"{a} (n={len(v)})" for a, v in sorted(arms.items())) + " |")
    p("|---|" + "---|" * len(arms))

    def row(name, fn, worst=max, d=1):
        cells = []
        for a, v in sorted(arms.items()):
            xs = [x for x in (fn(b) for b in v) if x is not None]
            cells.append(f"{fmt(med(xs), d)} ({fmt(worst(xs), d)})" if xs else "-")
        p(f"| {name} | " + " | ".join(cells) + " |")
    R = lambda k: (lambda b: (b["rep1"] or {}).get(k))
    row("rep 1: first delete after onset s", R("first_delete_s"), d=2)
    row("rep 1: stops", R("stops"), d=0)
    row("rep 1: longest write wait s", R("wait_s"))
    row("rep 1: throttled s", R("throttled_s"))
    row("rep 1: GiB deleted within 5 s of onset", R("gib5"), worst=min, d=2)
    row("rep 1: IOPS", R("iops"), worst=min, d=0)
    row("rep 1: p99.9 s", R("p999_s"), d=2)
    row("reps 2-5: IOPS (median of reps)", lambda b: b["iops25"], worst=min, d=0)
    row("reps 2-5: p99.9 s (median of reps)", lambda b: b["p999_25"], d=2)
    row("reps 2-5: longest write wait s (median of reps)", lambda b: b["wait25"])
    row("reps 2-5: longest write wait s (worst rep)", lambda b: b["wait25_worst"])
    row("reps 2-5: stops (4 reps)", lambda b: b["stops25"], d=0)
    row("reps 2-5: throttled s (median of reps)", lambda b: b["throttled25"])
    row("pgrace_deletes", lambda b: b["pgrace_deletes"], worst=min, d=0)
    row("pgrace_min_wait_ms (least)", lambda b: b["pgrace_min_wait"], worst=min, d=0)
    row("mark share of the coordinator, all reps %", lambda b: b["mark_share"], d=2)
    row("mark share of the coordinator, reps 2-5 %", lambda b: b["mark_share25"], d=2)
    # the pgracet6 rep-1 gate, per D block
    p("\n## pgracet6 rep-1 gate for the short grace, per block\n")
    p("| block | arm | first delete <= 3 s | 0 stops | longest wait <= 5 s | throttled <= 22 s | >= 6 GiB in 5 s | all |")
    p("|---|---|---|---|---|---|---|---|")
    for b in blocks:
        r = b["rep1"] or {}
        c = [r.get("first_delete_s") is not None and r["first_delete_s"] <= 3, r.get("stops") == 0, (r.get("wait_s") or 99) <= 5,
             (r.get("throttled_s") if r.get("throttled_s") is not None else 99) <= 22, (r.get("gib5") or 0) >= 6]
        p(f"| {b['name']} | {b['arm']} | " + " | ".join("pass" if x else "FAIL" for x in c) + f" | {'PASS' if all(c) else 'FAIL'} |")
    if "--json" in sys.argv:
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as f:
            json.dump(blocks, f, indent=1, default=str)
    print("\n".join(out))


main()
