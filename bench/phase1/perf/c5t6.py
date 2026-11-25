#!/usr/bin/env python3
"""T6 of active C5 (condemn-on-cut) on the real daemon: per-repetition metrics, per-gate tables and verdicts.

  c5t6.py RESULTS_DIR [--json OUT.json] > tables.md

One directory per block under RESULTS_DIR, written by tests/perf/c5t6.sh (tests/perf/c5t6-cells.sh):
  b<NN>-<arm>-<chunk bytes>-<kind>      arm: off | A (gc_condemn_on_cut = shadow) | B (on, 30 s grace floor)
                                        kind: w4k | w4kz (zipf:1.2) | restart (w4k, clean restart in rep 3) | pin (w4k, plain volume, pin held)
  lunixd.<N>.log (debug), reps.tsv, series.jsonl (every 2 s: gc + status), raw/fio-rep<R>-<job>.json,
  raw/rep<R>-<job>/{gc,status}-{before,end}.json, raw/lat-rep<R>-<job>_clat.*.log, raw/gc-final.json, raw/fsck-*.json, result.txt.

Fixes against t6recheck.py (the defects the earlier reviews listed):
  * a repetition's window starts at fio's own `job_start` (ms) and ends 0.5 s after the end of the json;
  * every pass is clipped to the window by its interval; urgent passes (empty `phases=`) count by their wall time;
  * the cost of a cut is read from the exact counters (`cuts`, `cut_bytes_sum`, `cut_us_sum`: deltas over a repetition, never
    sampled), as cost per journal byte at equal size and as a fit cut_us = a x cuts + b x bytes;
  * the pressure onset is the collector's own uncoalesced INFO line `gc: the engine's pressure level changed raised=true`;
  * the lag medians come from the per-event lines `refs: an offer was taken` (condemn lag) and `gc: a proposed segment was
    deleted` (delete lag), not from pass-line means.
Repetition 1 (the cold start after the fill) is always reported apart from repetitions 2-5.
"""
import glob
import json
import math
import os
import re
import statistics as st
import sys

RAMP_S = 5
ARM_MODE = {"off": "off", "A": "shadow", "B": "on"}
GiB = 2 ** 30


# ---- helpers ----------------------------------------------------------------------------------------------------------
def jl(p):
    """A json file; fio's output may have text in front of the object."""
    try:
        with open(p) as f:
            t = f.read()
    except OSError:
        return None
    i = t.find("{")
    if i < 0:
        return None
    try:
        return json.loads(t[i:])
    except ValueError:
        return None


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


def mean(v):
    v = [x for x in v if x is not None]
    return st.mean(v) if v else None


def mx(v):
    v = [x for x in v if x is not None]
    return max(v) if v else None


def f1(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def wmed(pairs):
    """Median of (value, weight) pairs."""
    pairs = sorted((v, w) for v, w in pairs if w > 0)
    tot = sum(w for _, w in pairs)
    if not pairs:
        return None
    acc = 0
    for v, w in pairs:
        acc += w
        if acc * 2 >= tot:
            return v
    return pairs[-1][0]


def kv(line):
    return {k: v for k, v in re.findall(r'(\w+)=(\S+)', line)}


def num(x, default=0):
    try:
        return int(x)
    except (TypeError, ValueError):
        try:
            return float(x)
        except (TypeError, ValueError):
            return default


def load_log(path):
    ev = []
    try:
        with open(path, errors='replace') as f:
            lines = f.read().splitlines()
    except OSError:
        return ev
    for line in lines:
        m = re.match(r'(\d{13}) (\w+)\s+(\S+): (.*)', line)
        if m:
            ev.append((int(m.group(1)), m.group(2), m.group(3), m.group(4)))
    return ev


def parse_pass(ts, msg):
    k = kv(msg)
    m = re.search(r'phases=(.*?) condemned=', msg)
    ph = {}
    if m:
        for name, ms, units in re.findall(r'(\w+)=(\d+)ms/(\d+)u@', m.group(1)):
            ph[name] = (int(ms), int(units))
    how = k.get("how", "").strip('"')
    wall = num(k.get("wall_ms"))
    if how == "urgent" and not ph:      # an urgent pass line has an empty `phases=`: its wall time is collector time
        ph["urgent"] = (wall, 0)
    return dict(ts=ts, how=how, kind=k.get("kind", "?"), pass_=num(k.get("pass")), mark_ms=num(k.get("mark_ms")),
                lag_count=num(k.get("lag_count")), lag_mean_ms=num(k.get("lag_mean_ms")), lag_max_ms=num(k.get("lag_max_ms")),
                wall_ms=wall, phases=ph, segments_deleted=num(k.get("segments_deleted")), packs_deleted=num(k.get("packs_deleted")),
                compacted=num(k.get("compacted")), bytes_freed=num(k.get("bytes_freed")), bytes_moved=num(k.get("bytes_moved")))


def events_of(log):
    """Pass lines and the per-event lines of one daemon log."""
    passes, offers, deletes, uncond, press = [], [], [], [], []
    for ts, lvl, mod, msg in log:
        if msg.startswith("gc pass done"):
            passes.append(parse_pass(ts, msg))
        elif msg.startswith("refs: an offer was taken"):
            k = kv(msg)
            offers.append(dict(ts=ts, n=num(k.get("segments")), med=num(k.get("lag_median_ms")), max=num(k.get("lag_max_ms"))))
        elif msg.startswith("gc: a proposed segment was deleted"):
            k = kv(msg)
            deletes.append(dict(ts=ts, bytes=num(k.get("bytes")), lag=num(k.get("lag_ms"))))
        elif msg.startswith("gc: a proposed segment was un-condemned"):
            uncond.append(dict(ts=ts, reason=kv(msg).get("reason", "?")))
        elif msg.startswith("gc: the engine's pressure level changed"):
            press.append((ts, kv(msg).get("raised") == "true"))
    return passes, offers, deletes, uncond, press


def series_of(d):
    out = []
    try:
        with open(os.path.join(d, "series.jsonl")) as f:
            for line in f:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return out


def delta(b, e, path):
    """e - b of a (possibly nested) numeric field; None when either is missing."""
    def g(o):
        for p in path:
            if not isinstance(o, dict) or p not in o:
                return None
            o = o[p]
        return o
    x, y = g(b), g(e)
    return None if x is None or y is None else y - x


# ---- one repetition ---------------------------------------------------------------------------------------------------
def rep_metrics(run, row, procs, series):
    rep, label = int(row[0]), row[1]
    f = jl(os.path.join(run, "raw", f"fio-{label}.json"))
    if not f:
        return None
    rd = os.path.join(run, "raw", label)
    gb, ge = jl(os.path.join(rd, "gc-before.json")), jl(os.path.join(rd, "gc-end.json"))
    sb, se = jl(os.path.join(rd, "status-before.json")), jl(os.path.join(rd, "status-end.json"))
    j = f["jobs"][0]["write"]
    pc = j["clat_ns"].get("percentile", {})
    m = dict(rep=rep, label=label, iops=j["iops"], p99_s=pc.get("99.000000", 0) / 1e9, p999_s=pc.get("99.900000", 0) / 1e9,
             err=sum(int(x.get("error", 0) or 0) for x in f["jobs"]), ok=row[6] == "ok")
    # the window: fio's own job_start to the end of the json (+ 0.5 s)
    end = f.get("timestamp_ms", 0)
    js = f["jobs"][0].get("job_start") or (end - int((f["jobs"][0].get("elapsed") or 66) * 1000))
    lo, hi = js, end + 500
    m["lo"], m["hi"], m["window_s"] = lo, hi, (hi - lo) / 1000
    # completion latency: per-second maxima of the fio log; the longest is the longest wait, runs of seconds >= 2 s are stops
    secs = []
    for lf in glob.glob(os.path.join(run, "raw", f"lat-{label}_clat.*.log")):
        with open(lf) as fh:
            for line in fh:
                p = line.split(",")
                if len(p) >= 2:
                    try:
                        secs.append((int(p[0]), float(p[1]) / 1e9))
                    except ValueError:
                        pass
    m["max_s"] = mx([v for _, v in secs]) or 0.0
    bad = sorted({t for t, v in secs if v >= 2.0})
    m["stall_intervals"] = [(lo + t - 1000, lo + t + 5000) for t in bad]   # a second with a completion of 2 s or more, and 5 s after it
    # throttle and cut counters
    tb, te = (sb or {}).get("throttle", {}), (se or {}).get("throttle", {})
    for k in ("stops", "throttled_ms", "delayed_writes", "delayed_ms", "delay_capped", "fulls", "enospc"):
        m[k] = te.get(k, 0) - tb.get(k, 0)
    m["restarted"] = False
    wb, we = (sb or {}).get("write_path", {}), (se or {}).get("write_path", {})
    for k in ("cuts", "cut_bytes_sum", "cut_us_sum", "cut_map_ns_sum", "cut_chunks_sum"):
        d = we.get(k, 0) - wb.get(k, 0)
        if d < 0:                      # the daemon restarted inside the repetition: the counters begin again
            m["restarted"] = True
        m[k] = d
    if m["restarted"]:
        for k in ("cuts", "cut_bytes_sum", "cut_us_sum", "cut_map_ns_sum", "cut_chunks_sum"):
            m[k] = None
        for k in ("stops", "throttled_ms", "delayed_writes", "delayed_ms", "delay_capped", "fulls", "enospc"):
            m[k] = 0                   # the engine's counters began again: no delta for this repetition (the fio figures stand)
    # collector counters: deltas of the reclaim block; cumulative ones (max, segments) at the end
    rb, re_ = (gb or {}).get("reclaim", {}), (ge or {}).get("reclaim", {})
    rf_b, rf_e = (gb or {}).get("refs", {}) or {}, (ge or {}).get("refs", {}) or {}
    R = {}
    for k, v in re_.items():
        if k in ("propose_us_max", "recheck_lag_ms_max", "fast_lag_ms_max"):
            R[k] = v
        else:
            R[k] = v - rb.get(k, 0)
    if m["restarted"] or any(v < 0 for v in R.values()):
        R = {k: None for k in R}
    m["R"] = R
    m["refs_segments_end"] = rf_e.get("segments")
    m["refs_ready_end"] = rf_e.get("state")
    sb2, se2 = (gb or {}).get("stats", {}), (ge or {}).get("stats", {})
    m["compaction_read"] = se2.get("compaction_read", 0) - sb2.get("compaction_read", 0)
    m["compaction_written"] = se2.get("compaction_written", 0) - sb2.get("compaction_written", 0)
    m["segments_compacted"] = se2.get("segments_compacted", 0) - sb2.get("segments_compacted", 0)
    # per-event lines and pass lines inside the window (all processes)
    ps = [p for p in procs["passes"] if lo <= p["ts"] <= hi]
    m["passes"] = ps
    offers = [o for o in procs["offers"] if lo <= o["ts"] <= hi]
    dels = [d for d in procs["deletes"] if lo <= d["ts"] <= hi]
    m["offers"], m["deletes"] = offers, dels
    m["uncondemned_lines"] = sum(1 for u in procs["uncond"] if lo <= u["ts"] <= hi)
    # pressure onset: the collector's own line (not coalesced); first delete (any pass) and first proposal delete after it
    on = next((t for t, r in procs["press"] if r and lo <= t <= hi), None)
    m["onset_s"] = (on - lo) / 1000 if on else None
    m["onset_first_delete_s"] = m["onset_first_proposal_delete_s"] = None
    if on:
        fd = next((p for p in procs["passes"] if p["ts"] >= on and (p["segments_deleted"] > 0 or p["packs_deleted"] > 0)), None)
        if fd:
            m["onset_first_delete_s"] = (fd["ts"] - on) / 1000
        pd = next((d for d in procs["deletes"] if d["ts"] >= on), None)
        if pd:
            m["onset_first_proposal_delete_s"] = (pd["ts"] - on) / 1000
    m["raised_s"] = sum(max(0, min(b, hi) - max(a, lo)) for a, b in procs["raised_intervals"]) / 1000
    # coordinator share of the collector (passes clipped to the window; urgent counted by wall time)
    def clip(p):
        a, b = p["ts"] - p["wall_ms"], p["ts"]
        if b <= a:
            return 1.0 if lo <= b <= hi else 0.0
        return max(0.0, min(b, hi) - max(a, lo)) / (b - a)
    groups = dict(mark=("mark",), compact=("compact",), delete=("delete",), urgent=("urgent",), other=("condemn", "grace", "recheck", "finish"))
    sh = {g: 0.0 for g in groups}
    for p in procs["passes"]:
        fr = clip(p)
        if fr <= 0:
            continue
        for g, names in groups.items():
            sh[g] += fr * sum(p["phases"].get(n, (0, 0))[0] for n in names)
    for g in groups:
        m[f"share_{g}_pct"] = 100.0 * sh[g] / (hi - lo)
    m["share_pct"] = sum(m[f"share_{g}_pct"] for g in groups)
    # evidence objects: the pass line's number is its evidence object number
    stamps = sorted(p["ts"] for p in ps if p["pass_"] > 0)
    best = 0
    for i, t in enumerate(stamps):
        best = max(best, sum(1 for x in stamps[i:] if x < t + 60000))
    m["evidence_objects"], m["evidence_max_per_min"] = len(stamps), best
    # rep 1 figures against fio's job start: first proposal / first delete (the engine's own clock from the attach), first delete of
    # any kind, bytes freed and proposal bytes deleted up to 43 s, proposal_condemned_bytes at attach + 30 s
    proc = procs["proc_of"](lo)
    born = proc["born"] if proc else None
    m["born_to_job_s"] = (lo - born) / 1000 if born else None
    m["first_proposal_job_s"] = m["first_proposal_delete_job_s"] = None
    if born:
        if rf_e.get("first_proposal_ms") is not None:
            m["first_proposal_job_s"] = (born + rf_e["first_proposal_ms"] - lo) / 1000
        if rf_e.get("first_proposal_delete_ms") is not None:
            m["first_proposal_delete_job_s"] = (born + rf_e["first_proposal_delete_ms"] - lo) / 1000
    fdel = next((p for p in procs["passes"] if p["ts"] >= lo and (p["segments_deleted"] > 0 or p["packs_deleted"] > 0)), None)
    m["first_delete_job_s"] = (fdel["ts"] - lo) / 1000 if fdel else None
    m["first_delete_pass_wall_ms"] = fdel["wall_ms"] if fdel else None
    m["freed_43_gib"] = sum(p["bytes_freed"] for p in procs["passes"] if lo <= p["ts"] <= lo + 43000) / GiB
    m["prop_deleted_43_gib"] = sum(d["bytes"] for d in procs["deletes"] if lo <= d["ts"] <= lo + 43000) / GiB
    if born:
        s30 = [s for s in series if s.get("gc") and s["t"] <= born + 30000]
        m["prop_condemned_bytes_attach30_gib"] = (s30[-1]["gc"]["reclaim"].get("proposal_condemned_bytes", 0) / GiB) if s30 else None
    else:
        m["prop_condemned_bytes_attach30_gib"] = None
    m["load1"], m["load5"] = num(row[4], None), num(row[5], None)
    return m


# ---- one block --------------------------------------------------------------------------------------------------------
def load_block(run):
    name = os.path.basename(run)
    parts = name.split("-")
    if len(parts) != 4:
        return None
    blk, arm, ckb, kind = parts
    b = dict(name=name, blk=blk, arm=arm, ck=int(ckb) // 1024, kind=kind)
    logs = sorted(glob.glob(os.path.join(run, "lunixd.*.log")))
    allp, allo, alld, allu, allpr, procs = [], [], [], [], [], []
    for lp in logs:
        log = load_log(lp)
        p, o, d, u, pr = events_of(log)
        born = min((x["ts"] - x["wall_ms"] for x in p), default=None)
        end = max((t for t, *_ in log), default=None)
        procs.append(dict(born=born, end=end, log=log))
        allp += p
        allo += o
        alld += d
        allu += u
        allpr += pr
    allpr.sort()
    raised, start = [], None
    for t, r in allpr:
        if r and start is None:
            start = t
        elif not r and start is not None:
            raised.append((start, t))
            start = None
    if start is not None:
        raised.append((start, procs[-1]["end"] or start))

    def proc_of(ts):
        c = [p for p in procs if p["born"] and p["born"] <= ts]
        return c[-1] if c else None
    P = dict(passes=sorted(allp, key=lambda x: x["ts"]), offers=allo, deletes=alld, uncond=allu, press=allpr, raised_intervals=raised, proc_of=proc_of)
    series = series_of(run)
    rows = []
    try:
        with open(os.path.join(run, "reps.tsv")) as fh:
            rows = [ln.rstrip("\n").split("\t") for ln in fh if ln.strip()]
    except OSError:
        pass
    b["reps"] = [m for m in (rep_metrics(run, r, P, series) for r in rows if len(r) >= 7 and r[1].startswith("rep") and "-w4k" in r[1]) if m]
    b["offers"], b["deletes"], b["uncond"], b["passes"] = allo, alld, allu, P["passes"]
    b["procs"] = len(procs)
    b["series"] = series
    # block-level final counters (all processes: the figures of the process before a restart are in raw/gc-before-restart.json)
    fin = jl(os.path.join(run, "raw", "gc-final.json")) or {}
    pre = jl(os.path.join(run, "raw", "gc-before-restart.json"))
    gs = [g for g in (pre, fin) if g]
    def tot(sect, k):
        return sum((g.get(sect) or {}).get(k, 0) or 0 for g in gs)
    b["final"] = fin
    b["T"] = {k: tot("reclaim", k) for k in (fin.get("reclaim") or {}) if k not in ("propose_us_max", "recheck_lag_ms_max", "fast_lag_ms_max")}
    b["T"]["propose_us_max"] = max([(g.get("reclaim") or {}).get("propose_us_max", 0) for g in gs] or [0])
    for k in ("drift_segments", "rebases", "report_mismatches", "shadow_unconfirmed_named", "shadow_unconfirmed_live", "shadow_unconfirmed_kept",
              "shadow_would_propose", "shadow_confirmed", "pin_blocked", "proposals_taken"):
        b["T"][k] = tot("refs", k)
    b["refs_final"] = fin.get("refs") or {}
    fsck = None
    for fp in glob.glob(os.path.join(run, "raw", "fsck-*.json")):
        fsck = jl(fp)
    b["fsck_exit"] = (fsck or {}).get("exit")
    try:
        b["result"] = open(os.path.join(run, "result.txt")).read().strip()
    except OSError:
        b["result"] = "?"
    allmsg = [(p_["log"]) for p_ in procs]
    b["eio_log"] = sum(1 for lg in allmsg for _, _, _, msg in lg if re.search(r"EIO|ENOSPC|No space left|Input/output error", msg))
    b["ref_table_lines"] = sum(1 for lg in allmsg for _, _, _, msg in lg if "reference table" in msg)
    try:
        lb = open(os.path.join(run, "loadavg-block-start.txt")).read().split()
        b["load_start"] = [float(x) for x in lb[:3]]
    except (OSError, ValueError):
        b["load_start"] = None
    b["restart"] = None
    try:
        with open(os.path.join(run, "restart.tsv")) as fh:
            r = fh.readline().rstrip("\n").split("\t")
            b["restart"] = dict(stop_ms=int(r[2]) - int(r[1]), start_ms=int(r[3]) - int(r[2]), attached_after_ms=int(r[4]) - int(r[1]), state=r[5].replace("state=", ""))
    except (OSError, ValueError, IndexError):
        pass
    for p_ in procs:
        p_.pop("log", None)
    b["quiet"] = bool(b["load_start"] and b["load_start"][1] <= 2.0)
    return b


# ---- selection and fits -----------------------------------------------------------------------------------------------
def blocks_of(blocks, arm, ck, kind="w4k"):
    return [b for b in blocks if b["arm"] == arm and b["ck"] == ck and b["kind"] == kind]


def reps_of(blocks, arm, ck, kind, grp):
    out = []
    for b in blocks_of(blocks, arm, ck, kind):
        for r in b["reps"]:
            if grp == "1" and r["rep"] != 1:
                continue
            if grp == "2-5" and r["rep"] == 1:
                continue
            out.append(r)
    return out


def fit2(rows):
    """Least squares y = a x1 + b x2 (no intercept) over rows (x1, x2, y); None when singular. Returns (a, b)."""
    s11 = sum(x1 * x1 for x1, _, _ in rows)
    s12 = sum(x1 * x2 for x1, x2, _ in rows)
    s22 = sum(x2 * x2 for _, x2, _ in rows)
    t1 = sum(x1 * y for x1, _, y in rows)
    t2 = sum(x2 * y for _, x2, y in rows)
    det = s11 * s22 - s12 * s12
    if len(rows) < 3 or abs(det) < 1e-9 * max(1.0, s11 * s22):
        return None
    return ((t1 * s22 - t2 * s12) / det, (s11 * t2 - s12 * t1) / det)


def cost(rs):
    rs = [r for r in rs if r["cuts"]]
    if not rs:
        return None
    cuts, by, us = sum(r["cuts"] for r in rs), sum(r["cut_bytes_sum"] for r in rs), sum(r["cut_us_sum"] for r in rs)
    mapns, chunks = sum(r["cut_map_ns_sum"] for r in rs), sum(r["cut_chunks_sum"] for r in rs)
    fit = fit2([(r["cuts"], r["cut_bytes_sum"] / 2 ** 20, r["cut_us_sum"]) for r in rs])
    return dict(n=len(rs), cuts=cuts, mib=by / 2 ** 20, us=us, mean_cut_mib=by / 2 ** 20 / cuts, mean_cut_ms=us / 1000 / cuts,
                us_per_mib=us / (by / 2 ** 20) if by else None, a_us_per_cut=fit[0] if fit else None, b_us_per_mib=fit[1] if fit else None,
                map_ns_per_chunk=mapns / chunks if chunks else None,
                per_rep_us_per_mib=[r["cut_us_sum"] / (r["cut_bytes_sum"] / 2 ** 20) for r in rs if r["cut_bytes_sum"]])


def stat_line(vals, d=0, lower=False):
    vals = [v for v in vals if v is not None]
    if not vals:
        return "-"
    if lower:
        return f"{f1(st.median(vals), d)} / {f1(pct(vals, 10), d)} / {f1(min(vals), d)}"
    return f"{f1(st.median(vals), d)} / {f1(pct(vals, 90), d)} / {f1(max(vals), d)}"


def verdict(ok):
    return "PASS" if ok else "FAIL"


# ---- report -----------------------------------------------------------------------------------------------------------
def main():
    root = sys.argv[1]
    blocks = [b for b in (load_block(d) for d in sorted(glob.glob(os.path.join(root, "b*-*-*-*"))) if os.path.isdir(d)) if b]
    out = []
    A = out.append
    S = {"gates": {}}
    G = S["gates"]
    A("# C5 T6: active condemn-on-cut on the real daemon, tables\n")
    A("Generated by `bench/phase1/perf/c5t6.py`. Arms: `off`, **A** = `gc_condemn_on_cut = shadow`, **B** = `on` (30 s grace floor); in every arm "
      "`throttle_pace_store_bytes = true`, `gc_fast_recheck = true`, `log_level = debug`. Arm C (5 s pressure grace) was not run. Rep 1 is always "
      "apart from reps 2-5. IOPS tails are the lower decile and the minimum; latency tails the upper decile and the maximum.\n")

    # ---- blocks and load ------------------------------------------------------------------------------------------------
    A("## Blocks\n")
    A("| block | arm | chunk | kind | load 1 / 5 / 15 min at start | reps | result.txt | fsck exit |")
    A("|---|---|---|---|---|---|---|---|")
    for b in blocks:
        ls = b["load_start"]
        A(f"| {b['name']} | {b['arm']} | {b['ck']}K | {b['kind']} | {' / '.join(f1(x, 2) for x in ls) if ls else '-'}"
          f"{'' if b['quiet'] else ' NOT QUIET'} | {len(b['reps'])} | {b['result']} | {b['fsck_exit']} |")
    S["blocks"] = {b["name"]: dict(load_start=b["load_start"], quiet=b["quiet"], result=b["result"], fsck_exit=b["fsck_exit"]) for b in blocks}

    # ---- gate 1: safety ----------------------------------------------------------------------------------------------
    A("\n## Gate 1: safety (every block, any miss stops)\n")
    A("Counters are the block totals (both processes in the restart block). `proposal_named` is expected 0 in B, `shadow_unconfirmed_named` 0 in A. "
      "`un-cond` = `proposal_uncondemned` over `proposal_condemned` (must stay below 5 %, each with its logged reason). Kill loops and the gc oracles: "
      "see the report.\n")
    A("| block | fsck --deep | fio errors | EIO/ENOSPC lines | 'reference table' lines | fulls+enospc | drift | mismatches | rebases | "
      "rej_early | prop_errors | named (B) | shadow_unconf_named (A) | un-cond / condemned (lines) | hold_footer_errors | verdict |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    safety = {}
    for b in blocks:
        T = b["T"]
        fio_err = sum(r["err"] for r in b["reps"]) + (0 if all(r["ok"] for r in b["reps"]) else 1)
        fe = sum((r["fulls"] or 0) + (r["enospc"] or 0) for r in b["reps"])
        cond, unc = T.get("proposal_condemned", 0), T.get("proposal_uncondemned", 0)
        lines = len(b["uncond"])
        checks = dict(
            fsck=b["fsck_exit"] == 0, fio=fio_err == 0, eio=b["eio_log"] == 0, reftable=b["ref_table_lines"] == 0, space=fe == 0,
            drift=T["drift_segments"] == 0, mism=T["report_mismatches"] == 0, rebases=T["rebases"] > 0,
            early=T.get("proposal_rejected_early", 0) == 0, perr=T.get("propose_errors", 0) == 0,
            named=(T.get("proposal_named", 0) == 0) if b["arm"] == "B" else True,
            shadow=(T["shadow_unconfirmed_named"] == 0) if b["arm"] == "A" else True,
            uncond=(unc * 20 < cond or unc == 0) and lines == unc, hold=T.get("hold_footer_errors", 0) == 0,
            result=b["result"] == "0")
        safety[b["name"]] = checks
        A(f"| {b['name']} | {'clean' if checks['fsck'] else 'EXIT ' + str(b['fsck_exit'])} | {fio_err} | {b['eio_log']} | {b['ref_table_lines']} | {fe} | "
          f"{T['drift_segments']} | {T['report_mismatches']} | {T['rebases']} | {T.get('proposal_rejected_early', 0)} | {T.get('propose_errors', 0)} | "
          f"{T.get('proposal_named', 0) if b['arm'] == 'B' else '-'} | {T['shadow_unconfirmed_named'] if b['arm'] == 'A' else '-'} | {unc} / {cond} ({lines}) | "
          f"{T.get('hold_footer_errors', 0)} | {verdict(all(checks.values()))}{'' if all(checks.values()) else ' ' + ','.join(k for k, v in checks.items() if not v)} |")
    G["1_safety"] = dict(verdict=verdict(all(all(c.values()) for c in safety.values())), blocks=safety)
    unc_reasons = {}
    for b in blocks:
        for u in b["uncond"]:
            unc_reasons[u["reason"]] = unc_reasons.get(u["reason"], 0) + 1
    A(f"\nUn-condemn reasons over all blocks: {unc_reasons or 'none'}.")

    # ---- gate 2: mechanism ------------------------------------------------------------------------------------------
    A("\n## Gate 2: mechanism, B\n")
    A("Share = `proposal_condemned_bytes / (proposal_condemned_bytes + mark_condemned_bytes)` over the block (target 45 % at 64K w4k; the 90 % dedup cell "
      "was not run). Condemn lag = the per-event lines `refs: an offer was taken` (`lag_median_ms` of each offer weighted by its segments; target median "
      "at most 3 s) and the longest `lag_max_ms` outside a stop (a stop = a second with a completion of 2 s or more in the fio log, from 1 s before it to 5 s "
      "after; target at most 15 s).\n")
    A("| block | chunk | kind | share % | offers | segments | condemn lag median s | max s (all) | max s outside a stop | proposal delete lag median / max s (lines) | verdict |")
    A("|---|---|---|---|---|---|---|---|---|---|---|")
    g2 = {}
    for b in blocks:
        if b["arm"] != "B":
            continue
        T = b["T"]
        pc, mc = T.get("proposal_condemned_bytes", 0), T.get("mark_condemned_bytes", 0)
        share = 100.0 * pc / (pc + mc) if pc + mc else None
        stalls = [iv for r in b["reps"] for iv in r["stall_intervals"]]
        offers = b["offers"]
        outside = [o for o in offers if not any(a <= o["ts"] <= z for a, z in stalls)]
        lagmed = wmed([(o["med"] / 1000, o["n"]) for o in offers])
        lagmax_all, lagmax_out = mx([o["max"] / 1000 for o in offers]), mx([o["max"] / 1000 for o in outside])
        dl = [d["lag"] / 1000 for d in b["deletes"]]
        ok = None
        if b["kind"] == "w4k" and b["ck"] == 64:
            ok = share is not None and share >= 45 and lagmed is not None and lagmed <= 3 and (lagmax_out or 0) <= 15
        g2[b["name"]] = dict(share=share, lag_median_s=lagmed, lag_max_all_s=lagmax_all, lag_max_outside_s=lagmax_out, offers=len(offers),
                             segments=sum(o["n"] for o in offers), delete_lag_median_s=med(dl), delete_lag_max_s=mx(dl), ok=ok)
        A(f"| {b['name']} | {b['ck']}K | {b['kind']} | {f1(share)} | {len(offers)} | {sum(o['n'] for o in offers)} | {f1(lagmed, 2)} | {f1(lagmax_all, 1)} | "
          f"{f1(lagmax_out, 1)} | {f1(med(dl), 2)} / {f1(mx(dl), 1)} ({len(dl)}) | {'-' if ok is None else verdict(ok)} |")
    gated = [v["ok"] for v in g2.values() if v["ok"] is not None]
    G["2_mechanism"] = dict(verdict=verdict(all(gated)) if gated else "n/a", blocks=g2)

    # ---- gate 3: rep 1, 64K w4k, B ----------------------------------------------------------------------------------
    A("\n## Gate 3: rep 1 at 64K w4k, B against A (n = 4 blocks per arm)\n")
    A("Times are seconds from fio's `job_start` of rep 1 (the `refs` figures are the engine's own clock from the attach, shifted by the block's attach "
      "time = the first pass line of the log). Pass lines are written at the pass end: a first delete of any kind is an upper bound by that pass's wall. "
      "`prop cond GiB by attach+30 s` = `proposal_condemned_bytes` in the last 2 s sample at or before 30 s after the attach. "
      "Plan targets for B: first proposal within 5 s; at least 6 GiB condemned by attach + 30 s; first proposal delete within 36 s (A: 54.6 s); at least "
      "3 GiB proposal-deleted by 43 s; longest wait at most 5 s in 3 of 4 blocks; throttled at most 22 s.\n")
    A("| block | arm | job start after attach s | first proposal s | prop cond GiB by attach+30 s | first proposal delete s | first delete (any) s | "
      "proposal GiB deleted by 43 s | all GiB freed by 43 s | stops (n) | longest wait s | throttled s | onset s | IOPS | p99.9 s |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    g3 = {}
    for arm in ("A", "B", "off"):
        for b in blocks_of(blocks, arm, 64, "w4k"):
            r = next((x for x in b["reps"] if x["rep"] == 1), None)
            if not r:
                continue
            g3.setdefault(arm, {})[b["name"]] = dict(
                job_after_attach_s=r["born_to_job_s"], first_proposal_s=r["first_proposal_job_s"], prop_cond_gib_attach30=r["prop_condemned_bytes_attach30_gib"],
                first_proposal_delete_s=r["first_proposal_delete_job_s"], first_delete_s=r["first_delete_job_s"], prop_deleted_43_gib=r["prop_deleted_43_gib"],
                freed_43_gib=r["freed_43_gib"], stops=r["stops"], longest_wait_s=r["max_s"], throttled_s=r["throttled_ms"] / 1000, onset_s=r["onset_s"],
                iops=r["iops"], p999_s=r["p999_s"])
            A(f"| {b['name']} | {arm} | {f1(r['born_to_job_s'])} | {f1(r['first_proposal_job_s'])} | {f1(r['prop_condemned_bytes_attach30_gib'], 2)} | "
              f"{f1(r['first_proposal_delete_job_s'])} | {f1(r['first_delete_job_s'])} | {f1(r['prop_deleted_43_gib'], 2)} | {f1(r['freed_43_gib'], 2)} | "
              f"{r['stops']} | {f1(r['max_s'])} | {f1(r['throttled_ms'] / 1000)} | {f1(r['onset_s'])} | {r['iops']:,.0f} | {f1(r['p999_s'], 2)} |")
    Bs = list(g3.get("B", {}).values())
    chk = {}
    if Bs:
        chk["first_proposal_within_5s"] = sum(1 for x in Bs if x["first_proposal_s"] is not None and x["first_proposal_s"] <= 5)
        chk["6gib_condemned_by_attach30"] = sum(1 for x in Bs if (x["prop_cond_gib_attach30"] or 0) >= 6)
        chk["first_proposal_delete_within_36s"] = sum(1 for x in Bs if x["first_proposal_delete_s"] is not None and x["first_proposal_delete_s"] <= 36)
        chk["3gib_proposal_deleted_by_43s"] = sum(1 for x in Bs if x["prop_deleted_43_gib"] >= 3)
        chk["longest_wait_le_5s"] = sum(1 for x in Bs if x["longest_wait_s"] <= 5)
        chk["throttled_le_22s"] = sum(1 for x in Bs if x["throttled_s"] <= 22)
        n = len(Bs)
        need = {"first_proposal_within_5s": n, "6gib_condemned_by_attach30": n, "first_proposal_delete_within_36s": n, "3gib_proposal_deleted_by_43s": n,
                "longest_wait_le_5s": math.ceil(n * 3 / 4), "throttled_le_22s": n}
        A("\nGate 3 checks (B blocks meeting the target, of n = " + str(n) + "): " + "; ".join(f"{k}: {v} (need {need[k]})" for k, v in chk.items()) + ".")
        G["3_rep1_64k"] = dict(verdict=verdict(all(chk[k] >= need[k] for k in chk)), counts=chk, need=need, n=n,
                               items={k: verdict(chk[k] >= need[k]) for k in chk}, blocks=g3)
    else:
        G["3_rep1_64k"] = dict(verdict="n/a", blocks=g3)
    for arm in ("A", "B"):
        v = list(g3.get(arm, {}).values())
        if v:
            A(f"\n{arm} rep 1 medians (n = {len(v)}): first proposal {f1(med([x['first_proposal_s'] for x in v]))} s, first proposal delete "
              f"{f1(med([x['first_proposal_delete_s'] for x in v]))} s, first delete (any) {f1(med([x['first_delete_s'] for x in v]))} s, "
              f"all freed by 43 s {f1(med([x['freed_43_gib'] for x in v]), 2)} GiB, proposal freed by 43 s {f1(med([x['prop_deleted_43_gib'] for x in v]), 2)} GiB, "
              f"longest wait {f1(med([x['longest_wait_s'] for x in v]))} s, stops {f1(med([x['stops'] for x in v]), 0)}, throttled {f1(med([x['throttled_s'] for x in v]))} s.")

    # ---- gate 4 -----------------------------------------------------------------------------------------------------
    A("\n## Gate 4: arm C (5 s pressure grace)\n\nNot run: it awaits a maintainer decision and has no implementation. n/a.")
    G["4_armC"] = dict(verdict="n/a (not run)")

    # ---- per-group comparison tables (gates 5 and 6) --------------------------------------------------------------------------
    def cmp_table(title, ck, kind, grp_list=("1", "2-5")):
        A(f"\n### {title}\n")
        A("| arm | group | reps | stops (reps with a stop) | longest wait s (med / p90 / worst) | IOPS (med / p10 / worst) | p99.9 s (med / p90 / worst) | "
          "p99 s (med) | throttled s (med) | capped max | onset s (med) | onset to first delete s (med) | "
          "recheck lag (pass-line mean) ms (med) |")
        A("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        res = {}
        for grp in grp_list:
            for arm in ("off", "A", "B"):
                rs = reps_of(blocks, arm, ck, kind, grp)
                if not rs:
                    continue
                pl = [p for r in rs for p in r["passes"] if p["kind"] == "Recheck" and p["how"] != "urgent" and p["lag_count"] > 0]
                A(f"| {arm} | {'rep 1' if grp == '1' else 'reps 2-5'} | {len(rs)} | {sum(r['stops'] for r in rs)} ({sum(1 for r in rs if r['stops'] > 0)} of {len(rs)}) | "
                  f"{stat_line([r['max_s'] for r in rs], 1)} | {stat_line([r['iops'] for r in rs], 0, True)} | {stat_line([r['p999_s'] for r in rs], 2)} | "
                  f"{f1(med([r['p99_s'] for r in rs]), 2)} | {f1(med([r['throttled_ms'] / 1000 for r in rs]))} | {max(r['delay_capped'] for r in rs)} | "
                  f"{f1(med([r['onset_s'] for r in rs]))} | {f1(med([r['onset_first_delete_s'] for r in rs]))} | {f1(med([p['lag_mean_ms'] for p in pl]), 0)} ({len(pl)}) |")
                res[(arm, grp)] = dict(
                    reps=len(rs), stops=sum(r["stops"] for r in rs), reps_with_stop=sum(1 for r in rs if r["stops"] > 0),
                    max_s=dict(med=med([r["max_s"] for r in rs]), p90=pct([r["max_s"] for r in rs], 90), worst=mx([r["max_s"] for r in rs])),
                    iops=dict(med=med([r["iops"] for r in rs]), p10=pct([r["iops"] for r in rs], 10), worst=min(r["iops"] for r in rs)),
                    p999_s=dict(med=med([r["p999_s"] for r in rs]), p90=pct([r["p999_s"] for r in rs], 90), worst=mx([r["p999_s"] for r in rs])),
                    p99_s_med=med([r["p99_s"] for r in rs]), throttled_s_med=med([r["throttled_ms"] / 1000 for r in rs]),
                    onset_first_delete_s_med=med([r["onset_first_delete_s"] for r in rs]),
                    mark_share_pct=mean([r["share_mark_pct"] for r in rs]), gc_share_pct=mean([r["share_pct"] for r in rs]))
        return res

    A("\n## Gate 5: reps 2-5 at 64K w4k (and rep 1), per arm\n")
    c64 = cmp_table("64K w4k", 64, "w4k")
    # delete lag lines
    A("\nProposal delete lag (`gc: a proposed segment was deleted`, `lag_ms`) over the lines inside the repetition windows, B only:\n")
    A("| group | blocks | lines | median s | p90 s | max s | per block: reps with a stop among reps 2-5 |")
    A("|---|---|---|---|---|---|---|")
    g5 = {}
    for grp in ("1", "2-5"):
        rs = reps_of(blocks, "B", 64, "w4k", grp)
        lags = [d["lag"] / 1000 for r in rs for d in r["deletes"]]
        per = [sum(1 for r in b["reps"] if r["rep"] != 1 and r["stops"] > 0) for b in blocks_of(blocks, "B", 64)]
        A(f"| {'rep 1' if grp == '1' else 'reps 2-5'} | {len(blocks_of(blocks, 'B', 64))} | {len(lags)} | {f1(med(lags), 2)} | {f1(pct(lags, 90), 2)} | {f1(mx(lags), 1)} | "
          f"{', '.join(map(str, per)) if grp == '2-5' else '-'} |")
        g5[grp] = dict(lines=len(lags), median_s=med(lags), p90_s=pct(lags, 90), max_s=mx(lags), reps_with_stop_per_block=per if grp == "2-5" else None)
    a25, b25 = c64.get(("A", "2-5")), c64.get(("B", "2-5"))
    if a25 and b25 and g5["2-5"]["lines"]:
        checks = dict(
            delete_lag_median_le_6s=(g5["2-5"]["median_s"] or 99) <= 6, delete_lag_max_le_13s=(g5["2-5"]["max_s"] or 99) <= 13,
            stops_at_most_1_rep_per_block=all(x <= 1 for x in g5["2-5"]["reps_with_stop_per_block"]),
            reps_with_stop_not_above_A=b25["reps_with_stop"] <= a25["reps_with_stop"],
            longest_wait_not_above_A=(b25["max_s"]["med"] or 0) <= (a25["max_s"]["med"] or 0) * 1.0 + 1e-9,
            iops_within_3pct_of_A=b25["iops"]["med"] >= 0.97 * a25["iops"]["med"],
            p999_not_above_A=(b25["p999_s"]["med"] or 0) <= (a25["p999_s"]["med"] or 0) + 1e-9)
        A("\nGate 5 checks (B reps 2-5 against A): " + "; ".join(f"{k}: {verdict(v)}" for k, v in checks.items()) + ".")
        A(f"IOPS median B {b25['iops']['med']:,.0f} against A {a25['iops']['med']:,.0f} ({100 * (b25['iops']['med'] / a25['iops']['med'] - 1):+.1f} %); "
          f"p99.9 median B {f1(b25['p999_s']['med'], 3)} s against A {f1(a25['p999_s']['med'], 3)} s; longest wait median B {f1(b25['max_s']['med'], 2)} s against A "
          f"{f1(a25['max_s']['med'], 2)} s.")
        G["5_reps2_5_64k"] = dict(verdict=verdict(all(checks.values())), items={k: verdict(v) for k, v in checks.items()},
                                  B=b25, A=a25, delete_lag=g5)
    else:
        G["5_reps2_5_64k"] = dict(verdict="n/a")
    S["cmp_64k"] = {f"{a}|{g}": v for (a, g), v in c64.items()}

    # ---- gate 6: 16K and zipf -----------------------------------------------------------------------------------------
    A("\n## Gate 6: 16K and zipf, B against A\n")
    c16 = cmp_table("16K w4k", 16, "w4k")
    cz = cmp_table("64K zipf (w4kz, zipf:1.2)", 64, "w4kz")
    A("\nCompaction volume (`compaction_written` and segments compacted over the repetition windows, reps 1-5) and evidence objects (the pass line's number; "
      "max in any 60 s window, and per minute over the windows):\n")
    A("| cell | arm | blocks | compaction written GiB (sum) | segments compacted (sum) | evidence objects / min (mean over reps) | max in 60 s | mark share % (reps 2-5 mean) | GC share % (reps 2-5 mean) |")
    A("|---|---|---|---|---|---|---|---|---|")
    g6 = {}
    for title, ck, kind in (("16K w4k", 16, "w4k"), ("64K zipf", 64, "w4kz"), ("64K w4k", 64, "w4k")):
        for arm in ("off", "A", "B"):
            rs = reps_of(blocks, arm, ck, kind, None)
            if not rs:
                continue
            r25 = [r for r in rs if r["rep"] != 1]
            cw = sum(r["compaction_written"] for r in rs) / GiB
            ev = [60.0 * r["evidence_objects"] / r["window_s"] for r in rs]
            g6[(title, arm)] = dict(compaction_written_gib=cw, segments_compacted=sum(r["segments_compacted"] for r in rs), evidence_per_min=mean(ev),
                                    evidence_max60=mx([r["evidence_max_per_min"] for r in rs]), mark_share_pct=mean([r["share_mark_pct"] for r in r25]),
                                    gc_share_pct=mean([r["share_pct"] for r in r25]), blocks=len(blocks_of(blocks, arm, ck, kind)))
            x = g6[(title, arm)]
            A(f"| {title} | {arm} | {x['blocks']} | {f1(cw, 1)} | {x['segments_compacted']} | {f1(x['evidence_per_min'])} | {x['evidence_max60']} | {f1(x['mark_share_pct'])} | {f1(x['gc_share_pct'])} |")
    items6 = {}
    for title, c in (("16K w4k", c16), ("64K zipf", cz)):
        for grp in ("1", "2-5"):
            a_, b_ = c.get(("A", grp)), c.get(("B", grp))
            if a_ and b_:
                items6[f"{title} {'rep 1' if grp == '1' else 'reps 2-5'} IOPS B within 3 % of A"] = (b_["iops"]["med"] >= 0.97 * a_["iops"]["med"],
                                                                                              f"B {b_['iops']['med']:,.0f} vs A {a_['iops']['med']:,.0f} ({100 * (b_['iops']['med'] / a_['iops']['med'] - 1):+.1f} %)")
    if ("16K w4k", "B") in g6:
        items6["16K mark share of the coordinator at most 12 % (B, reps 2-5)"] = (g6[("16K w4k", "B")]["mark_share_pct"] <= 12, f"{f1(g6[('16K w4k', 'B')]['mark_share_pct'])} % (A {f1(g6.get(('16K w4k', 'A'), {}).get('mark_share_pct'))} %)")
    if ("64K zipf", "B") in g6 and ("64K zipf", "A") in g6:
        ra = g6[("64K zipf", "B")]["compaction_written_gib"] / g6[("64K zipf", "A")]["compaction_written_gib"] if g6[("64K zipf", "A")]["compaction_written_gib"] else None
        items6["zipf compaction volume at least 80 % of A"] = (ra is not None and ra >= 0.8, f"{f1(100 * ra) if ra is not None else '-'} % of A")
    for (title, arm), x in g6.items():
        if arm == "B":
            items6[f"evidence objects at most 60 per minute ({title} B)"] = (max(x["evidence_per_min"] or 0, x["evidence_max60"] or 0) <= 60,
                                                                           f"mean {f1(x['evidence_per_min'])}, max in 60 s {x['evidence_max60']}")
    A("\nGate 6 checks:\n")
    for k, (ok, txt) in items6.items():
        A(f"* {verdict(ok)}: {k}: {txt}")
    G["6_16k_zipf"] = dict(verdict=verdict(all(ok for ok, _ in items6.values())) if items6 else "n/a", items={k: dict(verdict=verdict(ok), detail=t) for k, (ok, t) in items6.items()})
    S["cmp_16k"] = {f"{a}|{g}": v for (a, g), v in c16.items()}
    S["cmp_zipf"] = {f"{a}|{g}": v for (a, g), v in cz.items()}
    S["g6"] = {f"{t}|{a}": v for (t, a), v in g6.items()}

    # ---- gate 7: cost ------------------------------------------------------------------------------------------------
    A("\n## Gate 7: cost\n")
    A("### Cut cost per journal byte, from the exact counters (`cuts`, `cut_bytes_sum`, `cut_us_sum`: deltas over the repetition windows)\n")
    A("`us / MiB` = summed cut time over summed journal MiB (cost per journal byte at equal size; mean cut size shown). `a`, `b` = least squares "
      "`cut_us = a x cuts + b x MiB` over the repetitions (no intercept); the gate reads `b`. `map ns / chunk` = `cut_map_ns_sum / cut_chunks_sum`. "
      "A repetition with a restart inside has no delta and is left out.\n")
    A("| chunk | arm | group | reps | cuts | mean cut MiB | mean cut ms | us / MiB | vs off | a us per cut | b us / MiB | b vs off | map ns / chunk |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    g7 = {}
    for ck in (16, 64):
        for grp, lab in (("1", "rep 1"), ("2-5", "reps 2-5"), (None, "reps 1-5")):
            base = cost(reps_of(blocks, "off", ck, "w4k", grp))
            for arm in ("off", "A", "B"):
                c = cost(reps_of(blocks, arm, ck, "w4k", grp))
                if not c:
                    continue
                rel = (c["us_per_mib"] / base["us_per_mib"] - 1) * 100 if base and base["us_per_mib"] else None
                relb = (c["b_us_per_mib"] / base["b_us_per_mib"] - 1) * 100 if base and base["b_us_per_mib"] and c["b_us_per_mib"] else None
                g7[f"{ck}K|{arm}|{lab}"] = dict(c, us_per_mib_vs_off_pct=rel, b_vs_off_pct=relb)
                A(f"| {ck}K | {arm} | {lab} | {c['n']} | {c['cuts']} | {f1(c['mean_cut_mib'], 0)} | {f1(c['mean_cut_ms'], 0)} | {f1(c['us_per_mib'], 0)} | "
                  f"{'-' if rel is None or arm == 'off' else f'{rel:+.1f} %'} | {f1(c['a_us_per_cut'], 0)} | {f1(c['b_us_per_mib'], 0)} | "
                  f"{'-' if relb is None or arm == 'off' else f'{relb:+.1f} %'} | {f1(c['map_ns_per_chunk'], 0)} |")
    items7 = {}
    for ck in (16, 64):
        for arm in ("A", "B"):
            x = g7.get(f"{ck}K|{arm}|reps 1-5")
            if x and x["b_vs_off_pct"] is not None:
                items7[f"b of {arm} within 3 % of off at {ck}K (reps 1-5 fit)"] = (abs(x["b_vs_off_pct"]) <= 3, f"{x['b_vs_off_pct']:+.1f} % (us per MiB {x['us_per_mib_vs_off_pct']:+.1f} %)")
    A("\n### Proposal cost (B; `propose_us_sum`, `propose_calls` deltas per repetition, `propose_us_max` since the attach)\n")
    A("| block | group | propose calls | propose_us_sum / window wall % (max over reps) | mean ms per call (reps pooled) | propose_us_max ms (end of block) | segments at the end |")
    A("|---|---|---|---|---|---|---|")
    g7p = {}
    for b in blocks:
        if b["arm"] != "B":
            continue
        for grp, rs in (("rep 1", [r for r in b["reps"] if r["rep"] == 1]), ("reps 2-5", [r for r in b["reps"] if r["rep"] != 1])):
            rs = [r for r in rs if r["R"].get("propose_calls") is not None]
            if not rs:
                continue
            calls = sum(r["R"]["propose_calls"] for r in rs)
            us = sum(r["R"]["propose_us_sum"] for r in rs)
            wall = max(100.0 * r["R"]["propose_us_sum"] / 1e3 / (r["hi"] - r["lo"]) for r in rs)
            pm = rs[-1]["R"].get("propose_us_max", 0) / 1000
            segs = rs[-1]["refs_segments_end"]
            g7p[f"{b['name']}|{grp}"] = dict(calls=calls, wall_pct_max=wall, mean_ms=us / calls / 1000 if calls else None, max_ms=pm, segments=segs)
            A(f"| {b['name']} | {grp} | {calls} | {f1(wall, 3)} | {f1(us / calls / 1000 if calls else None, 2)} | {f1(pm, 1)} | {segs} |")
    if g7p:
        items7["propose_us_sum at most 2 % of the wall time (every rep of every B block)"] = (max(v["wall_pct_max"] for v in g7p.values()) <= 2, f"max {f1(max(v['wall_pct_max'] for v in g7p.values()), 3)} %")
        items7["mean propose time at most 20 ms per call"] = (max(v["mean_ms"] or 0 for v in g7p.values()) <= 20, f"max of the groups {f1(max(v['mean_ms'] or 0 for v in g7p.values()), 2)} ms")
        A(f"\n`propose_us_max` (reported, not gated) over all B blocks: {f1(max(v['max_ms'] for v in g7p.values()), 1)} ms, at up to {max(v['segments'] or 0 for v in g7p.values())} segments.")
    A("\nGate 7 checks:\n")
    for k, (ok, txt) in items7.items():
        A(f"* {verdict(ok)}: {k}: {txt}")
    G["7_cost"] = dict(verdict=verdict(all(ok for ok, _ in items7.values())) if items7 else "n/a", items={k: dict(verdict=verdict(ok), detail=t) for k, (ok, t) in items7.items()},
                       cut=g7, propose=g7p)

    # ---- gate 8: orphan ------------------------------------------------------------------------------------------------
    A("\n## Gate 8: orphan rule, every arm and cell\n")
    A("`orphan_deferrals` over `segment_delete_verdicts` (deltas over the repetition windows, summed per block; more than 1 % is a finding). "
      "`cuts_requested` = cuts the collector asked the engine for. `hold_*` = the `Hold` stage of the mark (marks that ran it, footers read, footers failed).\n")
    A("| block | arm | kind | orphan deferrals | delete verdicts | share % | max share % in a rep | cuts requested | hold marks | hold footers | hold footer errors |")
    A("|---|---|---|---|---|---|---|---|---|---|---|")
    g8 = {}
    for b in blocks:
        rs = [r for r in b["reps"] if r["R"].get("orphan_deferrals") is not None]
        od = sum(r["R"]["orphan_deferrals"] for r in rs)
        dv = sum(r["R"]["segment_delete_verdicts"] for r in rs)
        shs = [100.0 * r["R"]["orphan_deferrals"] / r["R"]["segment_delete_verdicts"] for r in rs if r["R"]["segment_delete_verdicts"]]
        g8[b["name"]] = dict(orphan_deferrals=od, delete_verdicts=dv, share_pct=100.0 * od / dv if dv else None, max_rep_share_pct=mx(shs),
                             cuts_requested=sum(r["R"]["cuts_requested"] for r in rs), hold_marks=sum(r["R"]["hold_marks"] for r in rs),
                             hold_footers=sum(r["R"]["hold_footers"] for r in rs), hold_footer_errors=sum(r["R"]["hold_footer_errors"] for r in rs),
                             block_total=dict(orphan_deferrals=b["T"].get("orphan_deferrals"), verdicts=b["T"].get("segment_delete_verdicts")))
        x = g8[b["name"]]
        A(f"| {b['name']} | {b['arm']} | {b['kind']} | {od} | {dv} | {f1(x['share_pct'], 2)} | {f1(x['max_rep_share_pct'], 2)} | {x['cuts_requested']} | {x['hold_marks']} | {x['hold_footers']} | {x['hold_footer_errors']} |")
    counted = [v for k, v in g8.items() if not k.endswith(("restart", "pin"))]
    over = [k for k, v in g8.items() if not k.endswith(("restart", "pin")) and v["share_pct"] is not None and v["share_pct"] > 1.0]
    G["8_orphan"] = dict(verdict=verdict(not over) + (f" (over 1 %: {', '.join(over)})" if over else ""), blocks=g8)

    # ---- restart and pin cells -------------------------------------------------------------------------------------------
    A("\n## Reported apart, not gated: the restart cell and the pinned cell\n")
    for b in blocks:
        if b["kind"] not in ("restart", "pin"):
            continue
        A(f"\n### {b['name']}\n")
        if b["restart"]:
            A(f"Restart: stop {b['restart']['stop_ms']} ms, start {b['restart']['start_ms']} ms, attached again after {b['restart']['attached_after_ms']} ms (state `{b['restart']['state']}`); "
              f"{b['procs']} daemon processes.")
        T, RF = b["T"], b["refs_final"]
        A(f"Block totals: proposals offered {T.get('proposals_offered')}, condemned {T.get('proposal_condemned')} ({f1((T.get('proposal_condemned_bytes', 0)) / GiB, 2)} GiB), "
          f"deleted {T.get('proposal_deleted')}, `pin_blocked` {T.get('pin_blocked')}, `proposals_taken` {T.get('proposals_taken')}, drift {T['drift_segments']}, "
          f"rebases {T['rebases']}, orphan deferrals {T.get('orphan_deferrals')} of {T.get('segment_delete_verdicts')} delete verdicts, cuts requested {T.get('cuts_requested')}, "
          f"hold marks {T.get('hold_marks')}, footers {T.get('hold_footers')}, footer errors {T.get('hold_footer_errors')}.")
        A("\n| rep | IOPS | p99.9 s | longest wait s | stops | throttled s | segments deleted (pass lines) | GiB freed (pass lines) | proposal deletes | orphan deferrals | verdicts | cuts requested | hold marks | segments (table) |")
        A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for r in b["reps"]:
            R = r["R"]
            A(f"| {r['rep']}{' (restart)' if r['restarted'] else ''} | {r['iops']:,.0f} | {f1(r['p999_s'], 2)} | {f1(r['max_s'])} | {r['stops']} | {f1(r['throttled_ms'] / 1000)} | "
              f"{sum(p['segments_deleted'] for p in r['passes'])} | {f1(sum(p['bytes_freed'] for p in r['passes']) / GiB, 2)} | {len(r['deletes'])} | "
              f"{R.get('orphan_deferrals')} | {R.get('segment_delete_verdicts')} | {R.get('cuts_requested')} | {R.get('hold_marks')} | {r['refs_segments_end']} |")
        mins = []
        for r in b["reps"]:
            mins.append(sum(p["segments_deleted"] for p in r["passes"]) / (r["window_s"] / 60))
        A(f"\nSegment deletes per minute by repetition (pass lines): {', '.join(f1(x, 0) for x in mins)}.")
    S["g_apart"] = {b["name"]: dict(restart=b["restart"], T=b["T"]) for b in blocks if b["kind"] in ("restart", "pin")}

    # ---- per repetition -------------------------------------------------------------------------------------------------
    A("\n## Per repetition\n")
    A("onset = the collector's `pressure level changed raised=true` line (s after job start); raised = seconds the level was raised inside the window; "
      "first del = first pass line that deleted anything after the onset; GC % = collector time over the window (mark / compact / delete / urgent / other).\n")
    A("| block | rep | IOPS | p99.9 s | longest wait s | stops | throttled s | raised s | onset s | onset to first delete s | onset to first proposal delete s | "
      "cuts | cut MiB | cut us/MiB | GC % (mark/compact/delete/urgent/other) | proposals condemned / deleted | orphan defer / verdicts | load 1/5 |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        for r in b["reps"]:
            R = r["R"]
            cm = f1(r["cut_bytes_sum"] / 2 ** 20, 0) if r["cut_bytes_sum"] is not None else "-"
            cu = f1(r["cut_us_sum"] / (r["cut_bytes_sum"] / 2 ** 20), 0) if r["cut_bytes_sum"] else "-"
            A(f"| {b['name']} | {r['rep']} | {r['iops']:,.0f} | {f1(r['p999_s'], 2)} | {f1(r['max_s'])} | {r['stops']} | {f1(r['throttled_ms'] / 1000)} | {f1(r['raised_s'])} | "
              f"{f1(r['onset_s'])} | {f1(r['onset_first_delete_s'])} | {f1(r['onset_first_proposal_delete_s'])} | {r['cuts']} | {cm} | {cu} | "
              f"{r['share_pct']:.1f} ({r['share_mark_pct']:.1f}/{r['share_compact_pct']:.1f}/{r['share_delete_pct']:.1f}/{r['share_urgent_pct']:.1f}/{r['share_other_pct']:.1f}) | "
              f"{R.get('proposal_condemned')} / {R.get('proposal_deleted')} | {R.get('orphan_deferrals')} / {R.get('segment_delete_verdicts')} | {f1(r['load1'], 1)}/{f1(r['load5'], 1)} |")

    print("\n".join(out))
    if "--json" in sys.argv:
        S["per_rep"] = {b["name"]: [{k: v for k, v in r.items() if k not in ("passes", "offers", "deletes", "stall_intervals")} for r in b["reps"]] for b in blocks}
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as fh:
            json.dump(S, fh, indent=1, default=str)


if __name__ == "__main__":
    main()
