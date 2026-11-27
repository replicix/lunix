#!/usr/bin/env python3
"""pgracemix: per-block and per-arm tables of the mixed read/write cell under the short segment grace (arms C = 5000 ms, C2 = 2000 ms).

  pgracemix.py RESULTS_DIR [--json OUT.json] > tables.md

Blocks are b<NN>-<arm>-65536-w4kmix (tests/perf/pgracemix.sh). Per-repetition figures come from c5t6.py (throttle, stops, pressure windows);
the read and write sides of fio and the per-second latency maxima per direction are read here.
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


def side(f, d):
    j = f["jobs"][0][d]
    pc = j["clat_ns"].get("percentile", {})
    return dict(iops=j["iops"], ios=j["total_ios"], p99=pc.get("99.000000", 0) / 1e6, p999=pc.get("99.900000", 0) / 1e6, max=j["clat_ns"].get("max", 0) / 1e6)


def wmax_s(run, label):
    """longest per-second maximum of the completion latency of the writes (column 3 = 1) and of the reads (0), seconds"""
    out = {0: 0.0, 1: 0.0}
    for lf in glob.glob(os.path.join(run, "raw", f"lat-{label}_clat.*.log")):
        with open(lf) as fh:
            for line in fh:
                p = [x.strip() for x in line.split(",")]
                if len(p) >= 3:
                    try:
                        out[int(p[2])] = max(out[int(p[2])], float(p[1]) / 1e9)
                    except (ValueError, KeyError):
                        pass
    return out


def block(run):
    b = c5t6.load_block(run)
    if not b:
        return None
    reps = []
    for r in b["reps"]:
        f = c5t6.jl(os.path.join(run, "raw", f"fio-{r['label']}.json"))
        rd, wr = side(f, "read"), side(f, "write")
        lm = wmax_s(run, r["label"])
        reps.append(dict(rep=r["rep"], r_iops=rd["iops"], w_iops=wr["iops"], r_p99_ms=rd["p99"], r_p999_ms=rd["p999"], r_max_ms=rd["max"],
                         r_ios=rd["ios"], w_ios=wr["ios"], w_wait_s=lm[1], r_wait_s=lm[0], stops=r["stops"], throttled_s=r["throttled_ms"] / 1000,
                         raised_s=r["raised_s"], window_s=r["window_s"], err=r["err"], ok=r["ok"]))
    vr = c5t6.jl(os.path.join(run, "raw", "fio-verify-w4k.json"))
    v_reads = side(vr, "read")["ios"] if vr else 0
    fin = c5t6.jl(os.path.join(run, "raw", "status-final.json")) or {}
    stt = fin.get("stats", {})
    gc = c5t6.jl(os.path.join(run, "raw", "gc-final.json")) or {}
    rc = gc.get("reclaim", {})
    reads = sum(x["r_ios"] for x in reps) + v_reads
    log = "".join(open(p, errors="replace").read() for p in sorted(glob.glob(os.path.join(run, "lunixd.*.log"))))
    warn = [ln for ln in log.splitlines() if re.search(r"\bWARN\b|\bERROR\b", ln)]
    retries_dbg = log.count("read part retried with the block's new entry")
    giveup = log.count("giving up")
    ver = open(os.path.join(run, "verify.txt")).read().strip() if os.path.exists(os.path.join(run, "verify.txt")) else "?"
    b2 = dict(name=b["name"], arm=b["arm"], reps=reps, guest_reads=reads, verify_reads=v_reads,
              revalidated=stt.get("read_revalidated"), exhausted=stt.get("read_revalidate_exhausted"), auth=stt.get("extent_auth_failures"),
              stale_min=stt.get("read_stale_age_min_ms"), stale_max=stt.get("read_stale_age_max_ms"),
              pgrace_deletes=rc.get("pgrace_deletes"), pgrace_bytes=rc.get("pgrace_bytes"), pgrace_min_wait=rc.get("pgrace_min_wait_ms"),
              fsck=b["fsck_exit"], result=b["result"], eio=b["eio_log"], verify=ver, load=b["load_start"], warn=len(warn), warn_lines=warn[:5],
              retry_debug_lines=retries_dbg, giveup_lines=giveup)
    if b2["revalidated"] is not None and reads:
        b2["reval_per_m"] = 1e6 * b2["revalidated"] / reads
    # throttle stops over the whole block, longest write wait over reps 2-5
    b2["stops_total"] = sum(x["stops"] for x in reps)
    b2["raised_pct_2_5"] = 100 * sum(x["raised_s"] for x in reps if x["rep"] > 1) / max(1e-9, sum(x["window_s"] for x in reps if x["rep"] > 1))
    b2["raised_pct_1"] = next((100 * x["raised_s"] / x["window_s"] for x in reps if x["rep"] == 1), None)
    return b2


def med(v):
    v = [x for x in v if x is not None]
    return st.median(v) if v else None


def fmt(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def main():
    run = sys.argv[1]
    blocks = [b for b in (block(d) for d in sorted(glob.glob(os.path.join(run, "b*-w4kmix")))) if b]
    out = []
    p = out.append
    p("# pgracemix tables\n")
    p("## Per block (reps 2-5 medians unless noted)\n")
    p("| block | arm | start load 1/5/15 | fio err | verify | fsck | reads (all) | revalidated | per M | exhausted | auth fail | stale age min/max ms | pgrace deletes | pgrace min wait ms | raised % rep1 / 2-5 | stops (5 reps) | WARN/ERROR lines |")
    p("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        p(f"| {b['name']} | {b['arm']} | {'/'.join(str(x) for x in (b['load'] or []))} | {sum(r['err'] for r in b['reps'])} | {b['verify']} | {b['fsck']} | {b['guest_reads']:,} | {b['revalidated']} | "
          f"{fmt(b.get('reval_per_m'), 3)} | {b['exhausted']} | {b['auth']} | {b['stale_min']}/{b['stale_max']} | {b['pgrace_deletes']:,} | {b['pgrace_min_wait']} | "
          f"{fmt(b['raised_pct_1'], 0)} / {fmt(b['raised_pct_2_5'], 0)} | {b['stops_total']} | {b['warn']} |")
    p("\n## Per block, throughput and latency\n")
    p("| block | arm | group | write IOPS | read IOPS | read p99 ms | read p99.9 ms | read max ms | longest write wait s (per-second max) | longest read wait s | throttled s | stops |")
    p("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        for g, sel in (("rep 1", lambda r: r["rep"] == 1), ("reps 2-5 median (worst)", lambda r: r["rep"] > 1)):
            rs = [r for r in b["reps"] if sel(r)]
            if not rs:
                continue
            w = lambda k: fmt(med([r[k] for r in rs]), 0)
            p(f"| {b['name']} | {b['arm']} | {g} | {w('w_iops')} | {w('r_iops')} | {w('r_p99_ms')} | {w('r_p999_ms')} | {fmt(max(r['r_max_ms'] for r in rs), 0)} | "
              f"{fmt(max(r['w_wait_s'] for r in rs), 2)} | {fmt(max(r['r_wait_s'] for r in rs), 2)} | {fmt(med([r['throttled_s'] for r in rs]), 1)} | {sum(r['stops'] for r in rs)} |")
    arms = {}
    for b in blocks:
        arms.setdefault(b["arm"], []).append(b)
    p("\n## Per arm: median over blocks (worst block in brackets)\n")
    p("| metric | " + " | ".join(f"{a} (n={len(v)})" for a, v in sorted(arms.items())) + " |")
    p("|---|" + "---|" * len(arms))

    def row(name, fn, worst=max, d=1):
        cells = []
        for a, v in sorted(arms.items()):
            xs = [fn(b) for b in v]
            xs = [x for x in xs if x is not None]
            cells.append(f"{fmt(med(xs), d)} ({fmt(worst(xs), d)})" if xs else "-")
        p(f"| {name} | " + " | ".join(cells) + " |")

    def rmed(b, k, grp):
        return med([r[k] for r in b["reps"] if (r["rep"] > 1) == (grp == "2-5")])

    def rmax(b, k, grp):
        xs = [r[k] for r in b["reps"] if (r["rep"] > 1) == (grp == "2-5")]
        return max(xs) if xs else None
    row("read_revalidated per million guest reads (whole block)", lambda b: b.get("reval_per_m"), d=3)
    row("read_revalidated (count, whole block)", lambda b: b["revalidated"], d=0)
    row("read_revalidate_exhausted", lambda b: b["exhausted"], d=0)
    row("extent_auth_failures", lambda b: b["auth"], d=0)
    row("read_stale_age_min_ms (0 = no sample)", lambda b: b["stale_min"], worst=min, d=0)
    row("read_stale_age_max_ms", lambda b: b["stale_max"], d=0)
    row("guest write IOPS, reps 2-5", lambda b: rmed(b, "w_iops", "2-5"), worst=min, d=0)
    row("guest read IOPS, reps 2-5", lambda b: rmed(b, "r_iops", "2-5"), worst=min, d=0)
    row("guest write IOPS, rep 1", lambda b: rmed(b, "w_iops", "1"), worst=min, d=0)
    row("guest read IOPS, rep 1", lambda b: rmed(b, "r_iops", "1"), worst=min, d=0)
    row("read p99 ms, reps 2-5", lambda b: rmed(b, "r_p99_ms", "2-5"), d=0)
    row("read p99.9 ms, reps 2-5", lambda b: rmed(b, "r_p999_ms", "2-5"), d=0)
    row("read max ms, reps 1-5 (worst rep)", lambda b: max(r["r_max_ms"] for r in b["reps"]), d=0)
    row("longest write wait s, reps 1-5 (per-second max of fio write latency)", lambda b: max(r["w_wait_s"] for r in b["reps"]), d=2)
    row("longest write wait s, reps 2-5", lambda b: rmax(b, "w_wait_s", "2-5"), d=2)
    row("stops (5 reps)", lambda b: b["stops_total"], d=0)
    row("pgrace_deletes", lambda b: b["pgrace_deletes"], worst=min, d=0)
    row("pgrace_min_wait_ms", lambda b: b["pgrace_min_wait"], worst=min, d=0)
    row("pressure raised, % of reps 2-5", lambda b: b["raised_pct_2_5"], worst=min, d=0)
    row("pressure raised, % of rep 1", lambda b: b["raised_pct_1"], worst=min, d=0)
    row("`read part retried` debug lines", lambda b: b["retry_debug_lines"], d=0)
    p("\nWorst block: the largest value, except IOPS, pgrace and stale-age minimum and raised share (the smallest).\n")
    print("\n".join(out))
    if "--json" in sys.argv:
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as f:
            json.dump(blocks, f, indent=1)


main()
