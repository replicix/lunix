#!/usr/bin/env python3
"""refsoak: tables of the real-daemon shadow soak of the C5 reference table.

  refsoak.py RESULTS_DIR [--json OUT.json] > tables.md

RESULTS_DIR holds one directory per block, written by tests/perf/refsoak.sh:
  s<N>-c<K>-<chunk>   soak cell K (tests/perf/refsoak-cells.sh): reps.tsv, restart.tsv (cell 1), series.jsonl (5 s samples of
                      `lunix gc --status --json` and the status JSON, with the phase name), probe.tsv (timed `lunix status`),
                      raw/gc-final.json, raw/gc-before-restart.json, raw/fio-*.json, lunixd.N.log, results.md
  k<N>-<chunk>-<arm>  cost block (tests/perf/partA.sh, w4k, 3 repetitions): raw/partA_c<K>k_q4_w4k_rep<R>/series.jsonl, lunixd.1.log
Counters of the reference table are per daemon process; a block with a restart sums the two processes (the first one is read from
the snapshot taken immediately before the stop). Sizes are bytes, times milliseconds unless the name says otherwise.
"""
import glob
import json
import math
import os
import re
import statistics as st
import sys

SUM_KEYS = ["drift_segments", "rebases", "report_mismatches", "shadow_would_propose", "shadow_offered_idle", "shadow_offered_busy",
            "shadow_confirmed", "shadow_confirmed_lead", "shadow_lead_ms_sum", "shadow_confirmed_no_lead",
            "shadow_unconfirmed_live", "shadow_unconfirmed_kept", "shadow_unconfirmed_named", "shadow_unconfirmed_listed",
            "shadow_revived", "shadow_early_by_mark", "shadow_missed", "shadow_confirmed_bytes", "shadow_early_bytes",
            "shadow_missed_bytes", "shadow_lead_byte_ms", "condemn_lag_ms_sum", "condemn_lag_count", "idle_wait_ms_sum",
            "idle_wait_count"]
MAX_KEYS = ["shadow_lead_ms_max", "condemn_lag_ms_max", "idle_wait_ms_max"]


def jl(p):
    try:
        with open(p) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def rd(p):
    try:
        with open(p, errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def pct(v, p):
    v = sorted(x for x in v if x is not None)
    if not v:
        return None
    k = (len(v) - 1) * p / 100
    lo, hi = math.floor(k), math.ceil(k)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def med(v):
    v = [x for x in v if x is not None]
    return st.median(v) if v else None


def f1(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def share(a, b, d=2):
    return "-" if not b else f"{100.0 * a / b:.{d}f} %"


def human(n):
    if n is None:
        return "-"
    for u in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(n) < 1024 or u == "TiB":
            return f"{n:.1f} {u}" if u != "B" else f"{int(n)} B"
        n /= 1024.0


def kv(line):
    return {k: v for k, v in re.findall(r"(\w+)=(\S+)", line)}


def jsonl(p):
    out = []
    for line in rd(p).splitlines():
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def log_lines(d):
    ev = []
    for f in sorted(glob.glob(os.path.join(d, "lunixd.*.log"))):
        for line in rd(f).splitlines():
            m = re.match(r"(\d{13}) (\w+)\s+(\S+): (.*)", line)
            if m:
                ev.append((int(m.group(1)), m.group(2), m.group(3), m.group(4)))
    return ev


def passes_of(ev):
    out = []
    for ts, _l, _m, msg in ev:
        if msg.startswith("gc pass done"):
            k = kv(msg)
            ph = {n: (int(ms), int(u)) for n, ms, u in re.findall(r"(\w+)=(\d+)ms/(\d+)u@", msg)}
            out.append(dict(ts=ts, how=k.get("how", "").strip('"'), kind=k.get("kind", "?"), mark_ms=int(k.get("mark_ms", 0)),
                            wall_ms=int(k.get("wall_ms", 0)), phases=ph, deleted=int(k.get("segments_deleted", 0))))
    return out


def cut_pairs(series, lo=None, hi=None):
    """(ms, journal bytes) of the cuts the samples saw; the same cut seen twice in a row counts once."""
    out, prev = [], None
    for s in series:
        if lo is not None and not (lo <= s["t"] <= hi):
            continue
        w = (s.get("status") or {}).get("write_path") or {}
        if not w.get("last_cut_bytes"):
            continue
        cur = (w.get("last_cut_ms"), w.get("last_cut_bytes"), w.get("seals"))
        if cur != prev:
            out.append((w["last_cut_ms"], w["last_cut_bytes"]))
        prev = cur
    return out


def per_mib(pairs):
    """ms per MiB of journal: byte-weighted (sum ms / sum MiB) and the median of the per-cut values."""
    pairs = [(m, b) for m, b in pairs if b > 0]
    if not pairs:
        return None, None, 0
    tot_ms, tot_b = sum(m for m, _ in pairs), sum(b for _, b in pairs)
    return tot_ms / (tot_b / 2**20), med([m / (b / 2**20) for m, b in pairs]), len(pairs)


def refs_sum(incs):
    a = {k: sum(i.get(k, 0) or 0 for i in incs) for k in SUM_KEYS}
    for k in MAX_KEYS:
        a[k] = max((i.get(k, 0) or 0) for i in incs)
    return a


def derived(a):
    c, e, m = a["shadow_confirmed"], a["shadow_early_by_mark"], a["shadow_missed"]
    cb, eb, mb = a["shadow_confirmed_bytes"], a["shadow_early_bytes"], a["shadow_missed_bytes"]
    d = dict(recall_segs=c / (c + e + m) if c + e + m else None, recall_bytes=cb / (cb + eb + mb) if cb + eb + mb else None,
             live_share=a["shadow_unconfirmed_live"] / c if c else None,
             mean_lead_ms=a["shadow_lead_ms_sum"] / a["shadow_confirmed_lead"] if a["shadow_confirmed_lead"] else None,
             mean_idle_wait_ms=a["idle_wait_ms_sum"] / a["idle_wait_count"] if a["idle_wait_count"] else None,
             lead_share_of_confirmed=a["shadow_confirmed_lead"] / c if c else None,
             lead_gib_s=a["shadow_lead_byte_ms"] / 1000.0 / 2**30)
    return d


def fio_rows(d):
    rows = []
    for f in sorted(glob.glob(os.path.join(d, "raw", "fio-rep*.json"))):
        txt = rd(f)
        i = txt.find("{")
        try:
            j = json.loads(txt[i:])
        except ValueError:
            continue
        w = j["jobs"][0]["write"]
        rows.append(dict(name=os.path.basename(f)[4:-5], iops=w.get("iops"), bw=w.get("bw_bytes"),
                         err=sum(int(x.get("error", 0) or 0) for x in j["jobs"]),
                         p999=(w.get("clat_ns", {}).get("percentile", {}) or {}).get("99.900000", 0) / 1e9))
    return rows


def cell_block(d):
    tag = os.path.basename(d)
    res = dict(tag=tag)
    fin = jl(os.path.join(d, "raw", "gc-final.json")) or {}
    pre = jl(os.path.join(d, "raw", "gc-before-restart.json"))
    incs = ([pre["refs"]] if pre and pre.get("refs") else []) + ([fin["refs"]] if fin.get("refs") else [])
    res["incarnations"] = len(incs)
    a = refs_sum(incs)
    res["sum"] = a
    res["derived"] = derived(a)
    first = incs[0] if incs else {}
    res["rep1"] = dict(first_ready_ms=first.get("first_ready_ms"), first_mark_condemn_ms=first.get("first_mark_condemn_ms"))
    if len(incs) > 1:
        res["after_restart"] = dict(first_ready_ms=incs[1].get("first_ready_ms"), first_mark_condemn_ms=incs[1].get("first_mark_condemn_ms"),
                                    rebases=incs[1].get("rebases"), drift=incs[1].get("drift_segments"))
    res["final_state"] = {k: (fin.get("refs") or {}).get(k) for k in ("state", "segments", "zero_waiting", "idle_waiting", "ready")}
    ser = jsonl(os.path.join(d, "series.jsonl"))
    res["samples"] = len(ser)
    rs = [(s["t"], (s.get("gc") or {}).get("refs")) for s in ser if (s.get("gc") or {}).get("refs")]
    bound = {}
    for k in ("segments", "zero_waiting", "idle_waiting"):
        v = [r[k] for _, r in rs if r.get(k) is not None]
        h = len(v) // 2
        q = len(v) // 4
        bound[k] = dict(min=min(v) if v else None, median=med(v), max=max(v) if v else None, last=v[-1] if v else None,
                        max_first_half=max(v[:h]) if h else None, max_second_half=max(v[h:]) if h else None,
                        max_last_quarter=max(v[-q:]) if q else None)
    res["bounded"] = bound
    nr = [r for _, r in rs if r.get("state") != "ready"]
    res["not_ready_samples"] = dict(n=len(nr), of=len(rs), states=sorted({r.get("state") for r in nr}),
                                    seconds_est=len(nr) * 5)
    # series, per-rep maxima of the three bounded values
    reps = []
    for line in rd(os.path.join(d, "reps.tsv")).splitlines():
        p = line.split("\t")
        if len(p) >= 7:
            reps.append(dict(rep=p[0], label=p[1], t0=int(p[2]), t1=int(p[3]), load1=p[4], load5=p[5], ok=p[6]))
    res["reps"] = reps
    per = []
    for r in reps:
        w = [x for t, x in rs if r["t0"] <= t <= r["t1"]]
        per.append(dict(label=r["label"], seconds=(r["t1"] - r["t0"]) / 1000.0, ok=r["ok"],
                        segments_max=max([x["segments"] for x in w], default=None),
                        zero_waiting_max=max([x["zero_waiting"] for x in w], default=None),
                        idle_waiting_max=max([x["idle_waiting"] for x in w], default=None)))
    res["per_phase"] = per
    wnd = [(r["t0"], r["t1"]) for r in reps if r["label"].endswith("-w4k") or r["label"].endswith(("w64d", "comp", "w4kz"))]
    cp = []
    for lo, hi in wnd:
        cp += cut_pairs(ser, lo, hi + 5000)
    res["cut_ms_per_mib"] = dict(zip(("weighted", "median", "n"), per_mib(cp)))
    # passes and the collector's calls
    ps = passes_of(log_lines(d))
    how = {}
    for p in ps:
        how[p["how"]] = how.get(p["how"], 0) + 1
    res["passes_by_how"] = how
    res["passes_by_kind"] = {k: sum(1 for p in ps if p["kind"] == k) for k in {p["kind"] for p in ps}}
    res["requests_final"] = fin.get("requests")
    res["gc_stats_final"] = fin.get("stats")
    # status probe and gc-call latency around the discards
    pr = []
    for line in rd(os.path.join(d, "probe.tsv")).splitlines():
        p = line.split("\t")
        if len(p) >= 3:
            try:
                pr.append((int(p[0]), int(p[1]), int(p[2])))
            except ValueError:
                pass
    res["probe"] = dict(n=len(pr), median_ms=med([m for _, m, _ in pr]), p99_ms=pct([m for _, m, _ in pr], 99),
                        max_ms=max([m for _, m, _ in pr], default=None), failures=sum(1 for _, _, rc in pr if rc))
    disc = [r for r in reps if r["label"].endswith("-discard")]
    if disc:
        dd = []
        for r in disc:
            lo, hi = r["t0"], r["t0"] + 60000
            pw = [m for t, m, _ in pr if lo <= t <= hi]
            gw = [s["gc_call_ms"] for s in ser if lo <= s["t"] <= hi]
            zw = [x["zero_waiting"] for t, x in rs if lo <= t <= hi]
            dd.append(dict(rep=r["rep"], discard_ms=r["t1"] - r["t0"], probe_max_ms=max(pw, default=None), probe_median_ms=med(pw),
                           gc_call_max_ms=max(gw, default=None), zero_waiting_max=max(zw, default=None), samples=len(zw)))
        res["discard"] = dd
    res["fio"] = fio_rows(d)
    txt = rd(os.path.join(d, "results.md"))
    res["fsck_clean"] = bool(re.search(r"PASS lunix fsck --deep clean", txt))
    res["log_reference_table_lines"] = int(re.search(r"reference table log lines: (\d+)", txt).group(1)) if "reference table log lines" in txt else None
    res["log_eio_enospc_lines"] = int(re.search(r"EIO/ENOSPC log lines: (\d+)", txt).group(1)) if "EIO/ENOSPC log lines" in txt else None
    sf = jl(os.path.join(d, "raw", "status-final.json")) or {}
    res["throttle_fulls_enospc"] = [(sf.get("throttle") or {}).get("fulls"), (sf.get("throttle") or {}).get("enospc")]
    res["fio_errors"] = sum(r["err"] for r in res["fio"])
    res["rep_not_ok"] = [r["label"] for r in reps if r["ok"] != "ok"]
    rst = rd(os.path.join(d, "restart.tsv")).split()
    if len(rst) >= 5:
        res["restart"] = dict(stop_ms=int(rst[2]) - int(rst[1]), start_ms=int(rst[3]) - int(rst[2]), until_attached_ms=int(rst[4]) - int(rst[1]), state=rst[5])
    res["load_start"] = rd(os.path.join(d, "loadavg-block-start.txt")).strip()
    res["driver_rc"] = rd(os.path.join(d, "driver-rc.txt")).strip()
    return res


def cost_block(d):
    tag = os.path.basename(d)
    m = re.match(r"k(\d+)-(\d+)k-(\w+)", tag)
    res = dict(tag=tag, chunk_k=int(m.group(2)), arm=m.group(3))
    pairs, reps = [], []
    for sd in sorted(glob.glob(os.path.join(d, "raw", "partA_c*_w4k_rep*"))):
        ser = jsonl(os.path.join(sd, "series.jsonl"))
        cp = cut_pairs(ser)
        w, mdn, n = per_mib(cp)
        rep = int(sd.rsplit("rep", 1)[1])
        reps.append(dict(rep=rep, cut_ms_per_mib=w, median=mdn, n=n, journal_mib=sum(b for _, b in cp) / 2**20))
        pairs += [(rep, c) for c in cp]
    res["reps"] = reps
    res["pairs"] = pairs
    ev = log_lines(d)
    ps = passes_of(ev)
    reg = [p for p in ps if p["how"] != "urgent" and p["mark_ms"] > 0]
    res["mark_ms"] = [p["mark_ms"] for p in reg]
    res["mark_ms_recheck"] = [p["mark_ms"] for p in reg if p["kind"] == "Recheck"]
    res["mark_ms_regular"] = [p["mark_ms"] for p in reg if p["kind"] != "Recheck"]
    mk = [p["phases"]["mark"] for p in ps if "mark" in p["phases"] and p["phases"]["mark"][1] > 0]
    res["mark_units"] = [(ms, u) for ms, u in mk]
    fin = jl(os.path.join(d, "raw", "partA_c%dk_q4_w4k_rep3" % res["chunk_k"], "series.jsonl"))
    txt = rd(os.path.join(d, "results.md"))
    res["fsck_clean"] = bool(re.search(r"PASS lunix fsck --deep clean", txt))
    res["fio_ok"] = "FAIL" not in txt.split("overall")[-1] if "overall" in txt else None
    res["load_start"] = rd(os.path.join(d, "loadavg-block-start.txt")).strip()
    return res


def cost_tables(blocks, out):
    out.append("\n## Cost check: shadow against off (quiet host, w4k, 3 repetitions per block)\n")
    summary = {}
    for ck in sorted({b["chunk_k"] for b in blocks}):
        arms = {}
        for b in blocks:
            if b["chunk_k"] == ck:
                arms.setdefault(b["arm"], []).append(b)
        out.append(f"\n### {ck}K chunks\n")
        out.append("| arm | blocks (start load 1 / 5 / 15) | cuts | cut ms per MiB, byte-weighted (per block) | median per-cut ms per MiB (all cuts) | rep-wise weighted (reps 1,2,3 per block) |")
        out.append("|---|---|---|---|---|---|")
        row = {}
        for arm in ("off", "shadow"):
            bl = arms.get(arm, [])
            allp = [c for b in bl for _, c in b["pairs"]]
            # reps 2-3 only (rep 1 follows the fill)
            late = [c for b in bl for r, c in b["pairs"] if r >= 2]
            w, mdn, n = per_mib(allp)
            wl, ml, nl = per_mib(late)
            row[arm] = dict(weighted=w, median=mdn, n=n, weighted_reps23=wl, median_reps23=ml, n_reps23=nl,
                            per_block=[per_mib([c for _, c in b["pairs"]])[0] for b in bl])
            out.append(f"| {arm} | {'; '.join(b['tag'] + ' ' + b['load_start'] for b in bl)} | {n} | {f1(w, 3)} ({', '.join(f1(x, 3) for x in row[arm]['per_block'])}) | {f1(mdn, 3)} | "
                       + "; ".join(f"{b['tag']}: " + ", ".join(f1(r['cut_ms_per_mib'], 3) for r in b["reps"]) for b in bl) + " |")
        if "off" in row and "shadow" in row and row["off"]["weighted"]:
            r_w = row["shadow"]["weighted"] / row["off"]["weighted"] - 1
            r_m = row["shadow"]["median"] / row["off"]["median"] - 1
            r_l = (row["shadow"]["weighted_reps23"] / row["off"]["weighted_reps23"] - 1) if row["off"]["weighted_reps23"] else None
            out.append(f"\nCut time per journal byte, shadow against off: **{100 * r_w:+.1f} %** byte-weighted, {100 * r_m:+.1f} % median per cut"
                       + (f", {100 * r_l:+.1f} % over reps 2-3 only" if r_l is not None else "") + " (gate: within +3 %).")
            summary[f"{ck}k"] = dict(off=row["off"], shadow=row["shadow"], delta_weighted=r_w, delta_median=r_m, delta_reps23=r_l)
        out.append("\n| arm | passes with a mark (non-urgent) | mark_ms median / mean / p90 | regular passes: mark_ms median / mean | recheck passes: mark_ms median / mean | mark phase ms per unit (sum ms / sum units) |")
        out.append("|---|---|---|---|---|---|")
        mrow = {}
        for arm in ("off", "shadow"):
            bl = arms.get(arm, [])
            mk = [x for b in bl for x in b["mark_ms"]]
            rg = [x for b in bl for x in b["mark_ms_regular"]]
            rc = [x for b in bl for x in b["mark_ms_recheck"]]
            un = [x for b in bl for x in b["mark_units"]]
            ms_u = sum(a for a, _ in un) / sum(u for _, u in un) if un and sum(u for _, u in un) else None
            mrow[arm] = dict(n=len(mk), median=med(mk), mean=st.mean(mk) if mk else None, p90=pct(mk, 90), reg_median=med(rg),
                             reg_mean=st.mean(rg) if rg else None, rc_median=med(rc), rc_mean=st.mean(rc) if rc else None, ms_per_unit=ms_u)
            r = mrow[arm]
            out.append(f"| {arm} | {r['n']} | {f1(r['median'], 0)} / {f1(r['mean'], 1)} / {f1(r['p90'], 0)} | {f1(r['reg_median'], 0)} / {f1(r['reg_mean'], 1)} | {f1(r['rc_median'], 0)} / {f1(r['rc_mean'], 1)} | {f1(r['ms_per_unit'], 3)} |")
        if f"{ck}k" in summary:
            summary[f"{ck}k"]["mark"] = mrow
        out.append(f"\nfsck --deep clean: " + ", ".join(f"{b['tag']} {b['fsck_clean']}" for b in blocks if b["chunk_k"] == ck))
    return summary


def main():
    root = sys.argv[1]
    jout = sys.argv[sys.argv.index("--json") + 1] if "--json" in sys.argv else None
    cells = [cell_block(d) for d in sorted(glob.glob(os.path.join(root, "s*"))) if os.path.isdir(d)]
    costs = [cost_block(d) for d in sorted(glob.glob(os.path.join(root, "k*"))) if os.path.isdir(d)]
    out = ["# refsoak tables (generated by bench/phase1/perf/refsoak.py)\n"]
    names = {"c1": "1 64K w4k uniform (+1 restart)", "c2": "2 16K w4k", "c3": "3 64K zipf:1.2", "c4": "4 64K dedup 50 %",
             "c5": "5 64K compaction-heavy", "c6": "6 64K large discard"}

    def nm(c):
        m = re.search(r"-(c\d)-", c["tag"])
        return names.get(m.group(1), c["tag"]) if m else c["tag"]

    if cells:
        out.append("## 1. Table correctness (all processes of the block summed; restart block: two processes)\n")
        out.append("| cell | procs | state at end | drift_segments | report_mismatches | rebases | `reference table` log lines | named | fsck --deep | fio errors / reps not ok | EIO/ENOSPC log lines, fulls / enospc |")
        out.append("|---|---|---|---|---|---|---|---|---|---|---|")
        for c in cells:
            a = c["sum"]
            out.append(f"| {nm(c)} | {c['incarnations']} | {c['final_state']['state']} | {a['drift_segments']} | {a['report_mismatches']} | {a['rebases']} | {c['log_reference_table_lines']} | {a['shadow_unconfirmed_named']} | {'clean' if c['fsck_clean'] else 'NOT CLEAN'} | {c['fio_errors']} / {len(c['rep_not_ok'])} | {c['log_eio_enospc_lines']}, {c['throttle_fulls_enospc'][0]} / {c['throttle_fulls_enospc'][1]} |")
        out.append("\n## 3. Wrong-proposal forecast (milestone-3 gate: live below 5 % of confirmed)\n")
        out.append("| cell | confirmed | unconfirmed_live | live share | kept | listed | revived | would_propose |")
        out.append("|---|---|---|---|---|---|---|---|")
        for c in cells:
            a = c["sum"]
            out.append(f"| {nm(c)} | {a['shadow_confirmed']} | {a['shadow_unconfirmed_live']} | {share(a['shadow_unconfirmed_live'], a['shadow_confirmed'], 3)} | {a['shadow_unconfirmed_kept']} | {a['shadow_unconfirmed_listed']} | {a['shadow_revived']} | {a['shadow_would_propose']} |")
        out.append("\n## 4. Recall (confirmed / (confirmed + early by mark + missed))\n")
        out.append("| cell | confirmed segs | early by mark | missed | recall segs | confirmed bytes | early bytes | missed bytes | recall bytes |")
        out.append("|---|---|---|---|---|---|---|---|---|")
        for c in cells:
            a, d = c["sum"], c["derived"]
            out.append(f"| {nm(c)} | {a['shadow_confirmed']} | {a['shadow_early_by_mark']} | {a['shadow_missed']} | {share(d['recall_segs'] or 0, 1) if d['recall_segs'] is not None else '-'} | {human(a['shadow_confirmed_bytes'])} | {human(a['shadow_early_bytes'])} | {human(a['shadow_missed_bytes'])} | {share(d['recall_bytes'] or 0, 1) if d['recall_bytes'] is not None else '-'} |")
        out.append("\n## 5. Gain\n")
        out.append("| cell | confirmed with lead | without lead | share with lead | mean lead ms | max lead ms | lead byte-ms (GiB x s) | offered idle | offered busy | idle wait mean / max ms (n) | revived |")
        out.append("|---|---|---|---|---|---|---|---|---|---|---|")
        for c in cells:
            a, d = c["sum"], c["derived"]
            out.append(f"| {nm(c)} | {a['shadow_confirmed_lead']} | {a['shadow_confirmed_no_lead']} | {share(a['shadow_confirmed_lead'], a['shadow_confirmed'], 1)} | {f1(d['mean_lead_ms'], 0)} | {a['shadow_lead_ms_max']} | {f1(d['lead_gib_s'], 1)} | {a['shadow_offered_idle']} | {a['shadow_offered_busy']} | {f1(d['mean_idle_wait_ms'], 0)} / {a['idle_wait_ms_max']} ({a['idle_wait_count']}) | {a['shadow_revived']} |")
        out.append("\n## 6. Rep 1 (first offer is possible only after the first mark rebased the table)\n")
        out.append("| cell | first_ready_ms | first_mark_condemn_ms | after the restart: first_ready_ms / first_mark_condemn_ms / rebases / drift |")
        out.append("|---|---|---|---|")
        for c in cells:
            ar = c.get("after_restart")
            out.append(f"| {nm(c)} | {c['rep1']['first_ready_ms']} | {c['rep1']['first_mark_condemn_ms']} | " + (f"{ar['first_ready_ms']} / {ar['first_mark_condemn_ms']} / {ar['rebases']} / {ar['drift']}" if ar else "-") + " |")
        out.append("\n## 7. Cost seen in the cells (cut ms per MiB of journal over the cell's measured windows; the off-against-shadow comparison is in the cost check)\n")
        out.append("| cell | cuts seen | cut ms per MiB, weighted | median per cut | status probe median / p99 / max ms (n, failures) |")
        out.append("|---|---|---|---|---|")
        for c in cells:
            q, p = c["cut_ms_per_mib"], c["probe"]
            out.append(f"| {nm(c)} | {q['n']} | {f1(q['weighted'], 3)} | {f1(q['median'], 3)} | {f1(p['median_ms'], 0)} / {f1(p['p99_ms'], 0)} / {p['max_ms']} ({p['n']}, {p['failures']}) |")
        for c in cells:
            if c.get("discard"):
                out.append(f"\nDiscard cell: the discards (6 GiB each) and what the 60 s after each one looked like (`ready()` itself is not exported; these are the proxies: the timed `lunix status` and `lunix gc --status` calls, which take the coordinator, and `zero_waiting`):\n")
                out.append("| rep | discard call ms | status probe median / max ms | `gc --status` call max ms | zero_waiting max | samples |")
                out.append("|---|---|---|---|---|---|")
                for r in c["discard"]:
                    out.append(f"| {r['rep']} | {r['discard_ms']} | {f1(r['probe_median_ms'], 0)} / {r['probe_max_ms']} | {r['gc_call_max_ms']} | {r['zero_waiting_max']} | {r['samples']} |")
        out.append("\n## 8. Bounded state (5 s samples)\n")
        out.append("| cell | samples | segments min / median / max / last | zero_waiting max (1st half / 2nd half / last quarter) | idle_waiting max | not Ready: samples (est. s), states | passes by how | requests (final) |")
        out.append("|---|---|---|---|---|---|---|---|")
        for c in cells:
            b, n = c["bounded"], c["not_ready_samples"]
            out.append(f"| {nm(c)} | {c['samples']} | {b['segments']['min']} / {f1(b['segments']['median'], 0)} / {b['segments']['max']} / {b['segments']['last']} | {b['zero_waiting']['max']} ({b['zero_waiting']['max_first_half']} / {b['zero_waiting']['max_second_half']} / {b['zero_waiting']['max_last_quarter']}) | {b['idle_waiting']['max']} | {n['n']} of {n['of']} (~{n['seconds_est']} s) {n['states']} | {c['passes_by_how']} | {c['requests_final']} |")
        out.append("\n### Per phase (maxima of the samples inside the phase)\n")
        out.append("| cell | phase | seconds | segments max | zero_waiting max | idle_waiting max | ok |")
        out.append("|---|---|---|---|---|---|---|")
        for c in cells:
            for p in c["per_phase"]:
                out.append(f"| {nm(c)} | {p['label']} | {p['seconds']:.0f} | {p['segments_max']} | {p['zero_waiting_max']} | {p['idle_waiting_max']} | {p['ok']} |")
        out.append("\n### fio per repetition (write IOPS of the first job group, p99.9 s, errors)\n")
        out.append("| cell | " + " | ".join(f"rep{i}" for i in range(1, 6)) + " |")
        out.append("|---|---|---|---|---|---|")
        for c in cells:
            by = {}
            for r in c["fio"]:
                m = re.match(r"rep(\d)-(w4k|w4kz|w64d|comp)$", r["name"])
                if m:
                    by[int(m.group(1))] = r
            out.append(f"| {nm(c)} | " + " | ".join((f"{by[i]['iops']:,.0f} / {by[i]['p999']:.2f} / {by[i]['err']}" if i in by else "-") for i in range(1, 6)) + " |")
        rs = [c for c in cells if c.get("restart")]
        for c in rs:
            r = c["restart"]
            out.append(f"\nRestart in {nm(c)}: stop {r['stop_ms']} ms, start {r['start_ms']} ms, attached again after {r['until_attached_ms']} ms ({r['state']}).")
    summary = {}
    if costs:
        summary = cost_tables(costs, out)
    ct = rd(os.path.join(root, "costtests.txt"))
    if ct:
        out.append("\n## Cost: map_ns, snapshot, ready(), rebase (ignored cost tests of the engine, on the host)\n\n```\n" + ct.strip() + "\n```")
    print("\n".join(out))
    if jout:
        with open(jout, "w") as f:
            json.dump(dict(cells=cells, cost=summary, cost_blocks=[{k: v for k, v in b.items() if k != "pairs"} for b in costs]), f, indent=1, default=str)


if __name__ == "__main__":
    main()
