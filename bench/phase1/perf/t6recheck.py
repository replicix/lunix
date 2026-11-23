#!/usr/bin/env python3
"""T6 fastrecheck A/B: per-repetition metrics, per-cell tables and the gate figures of the runs under results/t6recheck/.

  t6recheck.py RESULTS_DIR [--json OUT.json] > tables.md

Each run directory is one block (one daemon, one fresh volume, five repetitions per cell) written by tests/perf/t6recheck.sh:
  b<N>-<arm>-<chunk bytes>-<set>   arm: off | on (gc_fast_recheck); set: main (w4k + seqw), ctl (w4k), zipf (w4kz)
  raw/fio-partA_c<K>k_q4_<cell>_rep<R>.json, raw/<cell dir>/{lat_clat.*.log,series.jsonl,status-before.json,status-end.json},
  raw/fsck-parta-c<K>k-q4.json, lunixd.1.log (debug level), results.jsonl, results.md, loadavg-block-start.txt.

Differences from t6pacea.py (the findings of the review of the P1' A/B):
  * the IOPS tail is the LOWER decile (p10), the bad tail; latencies keep the upper decile;
  * the per-cut window of a repetition starts at the repetition's own start (fio end time minus its elapsed time), so the
    fill cuts of repetition 1 are not pulled in;
  * `store_factor_milli` is A_hat, the engine's estimate, and is labelled so; the measured factor is a_real_milli of the
    cut log line (store bytes / journal bytes of that cut);
  * the A_hat error statistics are given per cut and weighted by the journal bytes of the cut;
  * the 1- and 5-minute load average of every block start and of every repetition start is tabulated, and a block whose
    start was not quiet (5-minute load above 2, the harness rule) is flagged.
Repetition 1 (the cold start after the fill) is always reported apart from repetitions 2-5.
"""
import glob
import json
import math
import os
import re
import statistics as st
import sys

CELL_ORDER = ("w4k", "w4kz", "seqw")
RECHECK_KIND = "Recheck"
RAMP_S = 5


def jl(p):
    try:
        with open(p) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def pct(v, p):
    v = sorted(x for x in v if x is not None)
    if not v:
        return None
    k = (len(v) - 1) * p / 100
    lo, hi = math.floor(k), math.ceil(k)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def wpct(pairs, p):
    """Weighted percentile: pairs of (value, weight); the smallest value whose cumulative weight reaches p %."""
    pairs = sorted((v, w) for v, w in pairs if w > 0)
    tot = sum(w for _, w in pairs)
    if not pairs or tot <= 0:
        return None
    acc = 0.0
    for v, w in pairs:
        acc += w
        if acc >= tot * p / 100:
            return v
    return pairs[-1][0]


def med(v):
    v = [x for x in v if x is not None]
    return st.median(v) if v else None


def f1(x, d=1):
    return "-" if x is None else f"{x:,.{d}f}"


def triple(vals, d=1, lower=False):
    """median / tail decile / worst. For latencies and durations the tail is the upper decile (p90) and the worst the
    maximum; for IOPS (lower=True) the bad tail is the LOWER decile (p10) and the worst the minimum."""
    vals = [v for v in vals if v is not None]
    if not vals:
        return "-"
    if lower:
        return f"{f1(st.median(vals), d)} / {f1(pct(vals, 10), d)} / {f1(min(vals), d)}"
    return f"{f1(st.median(vals), d)} / {f1(pct(vals, 90), d)} / {f1(max(vals), d)}"


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
    active = 0
    if m:
        active = sum(int(a) for a in re.findall(r'=(\d+)ms/', m.group(1)))
    return dict(
        ts=ts, how=k.get("how", "").strip('"'), kind=k.get("kind", "?"), pass_=num(k.get("pass")), mark_ms=num(k.get("mark_ms")),
        lag_count=num(k.get("lag_count")), lag_mean_ms=num(k.get("lag_mean_ms")), lag_max_ms=num(k.get("lag_max_ms")),
        wall_ms=num(k.get("wall_ms")), active_ms=active, condemned=num(k.get("condemned")),
        segments_deleted=num(k.get("segments_deleted")), packs_deleted=num(k.get("packs_deleted")),
        compacted=num(k.get("compacted")), bytes_freed=num(k.get("bytes_freed")), bytes_moved=num(k.get("bytes_moved")),
        first_after_onset=False)


def gc_events(log):
    """Pass lines, throttle transitions and the warning lines of one block's log."""
    passes, throttle, warn = [], [], dict(abandoned=[], urgent_early=[], urgent_compaction=[], ladder_urgent=[], ladder_failed=[])
    for ts, lvl, mod, msg in log:
        if msg.startswith("gc pass done"):
            passes.append(parse_pass(ts, msg))
        elif msg.startswith("write throttle on"):
            throttle.append((ts, "on"))
        elif msg.startswith("write throttle off"):
            throttle.append((ts, "off"))
        elif "gc pass abandoned" in msg:
            warn["abandoned"].append(ts)
        elif "gc urgent pass stopped early" in msg:
            warn["urgent_early"].append(ts)
        elif "gc urgent compaction stopped" in msg:
            warn["urgent_compaction"].append(ts)
        elif msg.startswith("space ladder: gc urgent pass failed"):
            warn["ladder_failed"].append(ts)
        elif msg.startswith("space ladder: gc urgent pass"):
            warn["ladder_urgent"].append(ts)
    # a pass line is written at the pass's end; the first Recheck line after each pressure onset is flagged
    onsets, state = [], "off"
    for ts, what in throttle:
        if what == "on" and state == "off":
            onsets.append(ts)
        state = what
    for o in onsets:
        for p in passes:
            if p["ts"] >= o and p["kind"] == RECHECK_KIND and p["how"] != "urgent":
                p["first_after_onset"] = True
                break
    return passes, onsets, warn


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


def cell(run, ck, name, rep, passes, onsets, cuts_all, results_rows):
    cn = f"partA_c{ck}k_q4_{name}_rep{rep}"
    f = jl(os.path.join(run, "raw", f"fio-{cn}.json"))
    if not f:
        return None
    d = os.path.join(run, "raw", cn)
    j = f["jobs"][0]["write"]
    pc = j["clat_ns"].get("percentile", {})
    m = dict(cell=name, rep=rep, iops=j["iops"], p99_s=pc.get("99.000000", 0) / 1e9, p999_s=pc.get("99.900000", 0) / 1e9,
             err=sum(int(x.get("error", 0) or 0) for x in f["jobs"]))
    mx = 0.0
    for lf in glob.glob(os.path.join(d, "lat_clat.*.log")):
        with open(lf) as fh:
            for line in fh:
                p = line.split(",")
                if len(p) >= 2:
                    mx = max(mx, float(p[1]) / 1e9)
    m["max_s"] = mx
    b, e = jl(os.path.join(d, "status-before.json")), jl(os.path.join(d, "status-end.json"))
    tb, te = (b or {}).get("throttle", {}), (e or {}).get("throttle", {})
    for k in ("stops", "throttled_ms", "delayed_writes", "delayed_ms", "delay_capped", "fulls", "enospc"):
        m[k] = te.get(k, 0) - tb.get(k, 0)
    sb, se = (b or {}).get("stats", {}), (e or {}).get("stats", {})
    m["commits"] = se.get("commits", 0) - sb.get("commits", 0)
    ser = series_of(d)
    w = [(s.get("status") or {}).get("write_path") or {} for s in ser]

    def mxs(k):
        v = [x[k] for x in w if k in x and x[k] is not None]
        return max(v) if v else None
    # A_hat is the engine's estimate (store_factor_milli), not a measurement: named so in every table
    m["a_hat_max_milli"], m["a_hat_biased_max_milli"], m["uncut_max"] = mxs("store_factor_milli"), mxs("store_factor_biased_milli"), mxs("store_journal_uncut")
    cm = [x["last_cut_ms"] for x in w if x.get("last_cut_ms") is not None]
    m["cut_ms_med"], m["cut_ms_max"] = (med(cm), max(cm) if cm else None)
    m["cut_samples"] = cm
    m["above_soft_ms"] = (e or {}).get("write_path", {}).get("store_above_soft_ms", 0) - (b or {}).get("write_path", {}).get("store_above_soft_ms", 0)
    m["probe_max_ms"] = None
    try:
        with open(os.path.join(d, "probe.tsv")) as fh:
            ms = [int(x.split("\t")[1]) for x in fh if x.count("\t") >= 2]
        m["probe_max_ms"] = max(ms) if ms else None
    except (OSError, ValueError):
        pass
    # the repetition's own window: from the fio start (end minus elapsed, one second slack) to its end; never earlier
    end = f.get("timestamp_ms", 0)
    elapsed = f["jobs"][0].get("elapsed")
    if not elapsed:
        elapsed = RAMP_S + (60 if name != "seqw" else 30) + 1
    lo, hi = end - (elapsed + 1) * 1000, end + 500
    m["window"] = (lo, hi)
    m["window_s"] = (hi - lo) / 1000
    cuts = [c for c in cuts_all if lo <= c["ts"] <= hi]
    m["cuts"] = cuts
    ps = [p for p in passes if lo <= p["ts"] <= hi]
    m["passes"] = ps
    # pressure onset to the first delete: the first throttle-on transition in the window; the first pass line that deleted
    # something after it (a pass line is written at the pass's end, so this is an upper bound), and the first Recheck pass
    on = next((o for o in onsets if lo <= o <= hi), None)
    m["onset_ts"] = on
    m["onset_s"] = (on - lo) / 1000 if on else None
    m["onset_first_delete_s"] = m["onset_first_delete_kind"] = m["onset_first_delete_wall_ms"] = None
    m["onset_first_recheck_s"] = m["onset_first_recheck_wall_ms"] = m["onset_first_recheck_deleted"] = None
    if on:
        for p in passes:
            if p["ts"] >= on and (p["segments_deleted"] > 0 or p["packs_deleted"] > 0):
                m["onset_first_delete_s"] = (p["ts"] - on) / 1000
                m["onset_first_delete_kind"] = p["kind"] + (":urgent" if p["how"] == "urgent" else "")
                m["onset_first_delete_wall_ms"] = p["wall_ms"]
                break
        for p in passes:
            if p["ts"] >= on and p["kind"] == RECHECK_KIND:
                m["onset_first_recheck_s"] = (p["ts"] - on) / 1000
                m["onset_first_recheck_wall_ms"] = p["wall_ms"]
                m["onset_first_recheck_deleted"] = p["segments_deleted"]
                break
    # GC share of the coordinator: the time the passes spent inside collector units (profile "active", real clock) over the
    # repetition's wall time; recheck-only passes apart
    m["gc_active_ms"] = sum(p["active_ms"] for p in ps)
    m["gc_active_recheck_ms"] = sum(p["active_ms"] for p in ps if p["kind"] == RECHECK_KIND)
    m["gc_share_pct"] = 100.0 * m["gc_active_ms"] / (hi - lo)
    m["recheck_passes"] = sum(1 for p in ps if p["kind"] == RECHECK_KIND)
    m["regular_passes"] = sum(1 for p in ps if p["kind"] != RECHECK_KIND)
    m["compacted"] = sum(p["compacted"] for p in ps)
    m["bytes_moved"] = sum(p["bytes_moved"] for p in ps)
    m["segments_deleted"] = sum(p["segments_deleted"] for p in ps)
    m["bytes_freed"] = sum(p["bytes_freed"] for p in ps)
    # evidence objects: the pass line's number is its evidence object number (0 = none written)
    nums = sorted({p["pass_"] for p in ps if p["pass_"] > 0})
    m["evidence_objects"] = len(nums)
    best = 0
    stamps = sorted(p["ts"] for p in ps if p["pass_"] > 0)
    for i, t in enumerate(stamps):
        best = max(best, sum(1 for x in stamps[i:] if x < t + 60000))
    m["evidence_max_per_min"] = best
    # the load average sampled by the harness at the start of the repetition
    for r in results_rows:
        if r.get("cell", "").endswith(f"/{name}/rep{rep}"):
            m["load1"], m["load5"] = num(r.get("load1"), None), num(r.get("load5"), None)
    # gc pressure sampled in the series (daemon's own view)
    gp = [((s.get("status") or {}).get("throttle") or {}).get("gc_pressure") for s in ser]
    m["gc_pressure_samples"] = {k: gp.count(k) for k in set(gp) if k}
    rc = [(s.get("gc") or {}).get("reclaim") for s in ser]
    rc = [x for x in rc if x]
    m["reclaim_first"], m["reclaim_last"] = (rc[0], rc[-1]) if rc else (None, None)
    gs = [(s.get("gc") or {}).get("stats") for s in ser]
    gs = [x for x in gs if x]
    m["gcstats_first"], m["gcstats_last"] = (gs[0], gs[-1]) if gs else (None, None)
    return m


def cut_lines(log):
    out = []
    for ts, lvl, mod, msg in log:
        if msg.startswith("store factor of the cut"):
            k = kv(msg)
            try:
                out.append(dict(ts=ts, pred=int(k["a_pred_milli"]), real=int(k["a_real_milli"]), hat=int(k["a_hat_milli"]),
                                journal=int(k["journal_bytes"]), store=int(k["store_bytes"])))
            except (KeyError, ValueError):
                pass
    return out


def collect(root):
    runs = {}
    for run in sorted(glob.glob(os.path.join(root, "b*-*-*-*"))):
        base = os.path.basename(run)
        if not os.path.isdir(run):
            continue
        parts = base.split("-")
        if len(parts) != 4:
            continue
        blk, arm, ckb, setname = parts
        ck = int(ckb) // 1024
        log = load_log(os.path.join(run, "lunixd.1.log"))
        passes, onsets, warn = gc_events(log)
        cuts_all = cut_lines(log)
        rows = []
        try:
            with open(os.path.join(run, "results.jsonl")) as fh:
                for line in fh:
                    try:
                        rows.append(json.loads(line))
                    except ValueError:
                        pass
        except OSError:
            pass
        fsck = jl(os.path.join(run, "raw", f"fsck-parta-c{ck}k-q4.json"))
        rp = os.path.join(run, "result.txt")
        res = open(rp).read().strip() if os.path.exists(rp) else "?"
        try:
            md = open(os.path.join(run, "results.md")).read()
        except OSError:
            md = ""
        fsck_line = next((ln for ln in md.splitlines() if "fsck --deep clean" in ln), "")
        try:
            lb = open(os.path.join(run, "loadavg-block-start.txt")).read().split()
            load_start = [float(lb[0]), float(lb[1]), float(lb[2])]
        except (OSError, ValueError, IndexError):
            load_start = None
        m_ = re.search(r'load average at the start: (\S+) (\S+) ', md)
        harness_load = [float(m_.group(1)), float(m_.group(2))] if m_ else None
        cells = []
        for name in CELL_ORDER:
            for rep in range(1, 6):
                c = cell(run, ck, name, rep, passes, onsets, cuts_all, rows)
                if c:
                    cells.append(c)
        t0 = min((p["ts"] for p in passes), default=0)
        runs[base] = dict(
            blk=blk, arm=arm, ck=ck, set=setname, cells=cells, passes=passes, onsets=onsets, warn=warn, cuts=cuts_all,
            fsck_ok=fsck is not None and fsck.get("exit") == 0, fsck_exit=(fsck or {}).get("exit"),
            fsck_counts=(fsck or {}).get("counts"), fsck_line=fsck_line.strip("- ").strip(), result=res,
            eio_log=sum(1 for _, _, _, msg in log if "EIO" in msg or "ENOSPC" in msg),
            load_block_start=load_start, load_harness_start=harness_load, noisy_warning="WARNING: 5-minute load" in md,
            first_ts=t0, final_pass=max((p["pass_"] for p in passes), default=0),
            log_bytes=os.path.getsize(os.path.join(run, "lunixd.1.log")) if os.path.exists(os.path.join(run, "lunixd.1.log")) else 0,
            reclaim_final=next((c["reclaim_last"] for c in reversed(cells) if c["reclaim_last"]), None))
    return runs


def sel(runs, arm, ck, setname=None, cellname=None, reps=None):
    out = []
    for r in runs.values():
        if r["arm"] != arm or r["ck"] != ck or (setname and r["set"] != setname):
            continue
        for c in r["cells"]:
            if cellname and c["cell"] != cellname:
                continue
            if reps == "1" and c["rep"] != 1:
                continue
            if reps == "2-5" and c["rep"] == 1:
                continue
            out.append(c)
    return out


def cut_stats(cuts):
    """Per cut and byte-weighted (journal bytes) statistics of the A_hat error (real - predicted, milli)."""
    if not cuts:
        return None
    err = [c["real"] - c["pred"] for c in cuts]
    wts = [c["journal"] for c in cuts]
    tot = sum(wts)
    under = sum(1 for c in cuts if c["real"] > c["pred"])
    under20 = sum(1 for c in cuts if c["real"] > 1.2 * c["pred"])
    under_b = sum(c["journal"] for c in cuts if c["real"] > c["pred"])
    under20_b = sum(c["journal"] for c in cuts if c["real"] > 1.2 * c["pred"])
    store = sum(c["store"] for c in cuts)
    pred_store = sum(c["journal"] * c["pred"] / 1000 for c in cuts)
    return dict(
        n=len(cuts), journal_bytes=tot, store_bytes=store,
        measured_factor=store / tot if tot else None, predicted_factor=pred_store / tot if tot else None,
        err_p10=pct(err, 10), err_p50=pct(err, 50), err_p90=pct(err, 90),
        werr_p10=wpct(list(zip(err, wts)), 10), werr_p50=wpct(list(zip(err, wts)), 50), werr_p90=wpct(list(zip(err, wts)), 90),
        under_n=under, under20_n=under20, under_bytes_pct=100 * under_b / tot if tot else None,
        under20_bytes_pct=100 * under20_b / tot if tot else None,
        underpred_bytes_mib=sum(max(0, c["real"] - c["pred"]) * c["journal"] / 1000 for c in cuts) / 2**20 / len(cuts))


def cut_txt(cs):
    v = [x for c in cs for x in c["cut_samples"]]
    if not v:
        return "-"
    return f"{len(v)}; {st.median(v):.0f} / {st.mean(v):.0f} / {pct(v, 90):.0f}"


def lag_stats(passes, kinds, exclude_first=False):
    """Median over pass lines (urgent lines left out) of lag_mean_ms, with the median lag_max_ms and mark_ms of the same lines."""
    ps = [p for p in passes if p["kind"] in kinds and p["how"] != "urgent" and p["lag_count"] > 0
          and not (exclude_first and p["first_after_onset"])]
    if not ps:
        return None
    wsum = sum(p["lag_mean_ms"] * p["lag_count"] for p in ps)
    wn = sum(p["lag_count"] for p in ps)
    return dict(n=len(ps), objects=wn, mean_med=st.median([p["lag_mean_ms"] for p in ps]), mean_p90=pct([p["lag_mean_ms"] for p in ps], 90),
                max_med=st.median([p["lag_max_ms"] for p in ps]), max_worst=max(p["lag_max_ms"] for p in ps),
                mark_med=st.median([p["mark_ms"] for p in ps]), object_mean=wsum / wn if wn else None)


def cell_passes(runs, arm, ck, setname, cellnames, reps=None):
    """Pass lines inside the measured windows of the selected cells (fill and idle gaps left out)."""
    out = []
    for c in sel(runs, arm, ck, setname):
        if c["cell"] in cellnames and (reps is None or (reps == "1" and c["rep"] == 1) or (reps == "2-5" and c["rep"] != 1)):
            out += c["passes"]
    return out


def main():
    root = sys.argv[1]
    runs = collect(root)
    summary = {}
    if "--json" in sys.argv:
        pass
    out = []
    A = out.append
    A("# T6 fastrecheck A/B: tables\n")
    A("Generated by `bench/phase1/perf/t6recheck.py`. Both arms run with `throttle_pace_store_bytes = true`; the only difference is "
      "`gc_fast_recheck` (OFF = false, ON = true). IOPS tails are the lower decile (p10) and the minimum; latency tails are the "
      "upper decile (p90) and the maximum. `A_hat` is the engine's estimate of the store factor, not a measurement.\n")

    # ---- load -------------------------------------------------------------------------------------------------------
    A("## Host load at each block start\n")
    A("The driver waits for the 1-minute load to fall below 1 before a block (and, before the first block, the 5-minute load below 2). "
      "A block is flagged `NOT QUIET` when the 5-minute load at its start was above 2 (the harness rule). The 5-minute load after a "
      "block, and at every repetition start after the first, is high by itself: the daemon's blocked writer threads and the fio jobs of the "
      "measurement count in the load average (hundreds), so only the block-start figure says whether the host was quiet before the block.\n")
    A("| block | arm | 1 / 5 / 15 min at block start (driver) | 1 / 5 min at harness start | max 1-min / 5-min at a rep start (own load) | verdict |")
    A("|---|---|---|---|---|---|")
    for name, r in runs.items():
        ls = r["load_block_start"]
        l1s = [c.get("load1") for c in r["cells"] if c.get("load1") is not None]
        l5s = [c.get("load5") for c in r["cells"] if c.get("load5") is not None]
        quiet = ls is not None and ls[1] <= 2.0
        A(f"| {name} | {r['arm'].upper()} | {' / '.join(f1(x, 2) for x in ls) if ls else '-'} | "
          f"{' / '.join(f1(x, 2) for x in r['load_harness_start']) if r['load_harness_start'] else '-'} | "
          f"{f1(max(l1s), 1) if l1s else '-'} / {f1(max(l5s), 1) if l5s else '-'} | {'quiet at start' if quiet else 'NOT QUIET at start'} |")
    summary["load"] = {n: dict(block_start=r["load_block_start"], harness_start=r["load_harness_start"]) for n, r in runs.items()}

    # ---- cells ------------------------------------------------------------------------------------------------------
    cellsets = [("main", 64, "w4k", "64 KiB w4k"), ("main", 64, "seqw", "64 KiB seqw"), ("ctl", 16, "w4k", "16 KiB w4k (control)"),
                ("zipf", 64, "w4kz", "64 KiB w4k, zipf:1.2 (skewed)")]
    for setname, ck, cn, title in cellsets:
        A(f"\n## {title}\n")
        A("| arm | reps | stops (reps with a stop) | longest completion s (med / p90 / worst) | IOPS (med / p10 / worst) | p99.9 s (med / p90 / worst) | throttled_ms (med) | delayed_writes (med) | delay_capped (max) |")
        A("|---|---|---|---|---|---|---|---|---|")
        summary.setdefault(f"{title}", {})
        for grp, label in (("1", "rep 1"), ("2-5", "reps 2-5"), (None, "all")):
            for arm in ("off", "on"):
                cs = sel(runs, arm, ck, setname, cn, grp)
                if not cs:
                    continue
                nstop = sum(1 for c in cs if c["stops"] > 0)
                iops = [c["iops"] for c in cs]
                A(f"| {arm.upper()} {label} | {len(cs)} | {sum(c['stops'] for c in cs)} ({nstop} of {len(cs)}) | {triple([c['max_s'] for c in cs])} | "
                  f"{triple(iops, 0, lower=True)} | {triple([c['p999_s'] for c in cs], 2)} | "
                  f"{f1(med([c['throttled_ms'] for c in cs]), 0)} | {f1(med([c['delayed_writes'] for c in cs]), 0)} | {max(c['delay_capped'] for c in cs)} |")
                summary[title][f"{arm}_{label}"] = dict(
                    reps=len(cs), stops=sum(c["stops"] for c in cs), reps_with_stop=nstop,
                    max_s=dict(med=med([c["max_s"] for c in cs]), p90=pct([c["max_s"] for c in cs], 90), worst=max(c["max_s"] for c in cs)),
                    iops=dict(med=med(iops), p10=pct(iops, 10), worst=min(iops)),
                    p999_s=dict(med=med([c["p999_s"] for c in cs]), p90=pct([c["p999_s"] for c in cs], 90), worst=max(c["p999_s"] for c in cs)),
                    delay_capped_max=max(c["delay_capped"] for c in cs),
                    cut_ms_med=med([c["cut_ms_med"] for c in cs]), cut_ms_worst=max([c["cut_ms_max"] or 0 for c in cs]),
                    p99_s_med=med([c["p99_s"] for c in cs]))

    # ---- per repetition ---------------------------------------------------------------------------------------------
    A("\n## Per repetition (w4k, w4kz; seqw in the json)\n")
    A("onset = first `write throttle on` of the repetition (s after its start); first delete = the end of the first pass line that "
      "deleted anything after the onset (an upper bound of the delete time); recheck = the first Recheck pass line after the onset "
      "(wall of that pass); GC % = time inside collector units over the repetition's wall time; `A_hat max` is the engine's estimate.\n")
    A("| run | rep | max s | stops | IOPS | p99.9 s | capped | load 1/5 | onset s | onset to first delete s (kind, pass wall ms) | onset to first Recheck line s (wall ms, segs deleted) | recheck / regular passes | GC % | cuts | cut ms med / max | evidence objs (max in 60 s) | A_hat max milli |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for name, r in runs.items():
        for c in r["cells"]:
            if c["cell"] == "seqw":
                continue
            fd = "-" if c["onset_first_delete_s"] is None else f"{c['onset_first_delete_s']:.1f} ({c['onset_first_delete_kind']}, {c['onset_first_delete_wall_ms']})"
            fr = "-" if c["onset_first_recheck_s"] is None else f"{c['onset_first_recheck_s']:.1f} ({c['onset_first_recheck_wall_ms']}, {c['onset_first_recheck_deleted']})"
            A(f"| {name} | {c['rep']} | {c['max_s']:.1f} | {c['stops']} | {c['iops']:.0f} | {c['p999_s']:.2f} | {c['delay_capped']} | "
              f"{f1(c.get('load1'), 1)}/{f1(c.get('load5'), 1)} | {f1(c['onset_s'])} | {fd} | {fr} | {c['recheck_passes']} / {c['regular_passes']} | "
              f"{c['gc_share_pct']:.1f} | {len(c['cuts'])} | {f1(c['cut_ms_med'], 0)} / {f1(c['cut_ms_max'], 0)} | {c['evidence_objects']} ({c['evidence_max_per_min']}) | {c['a_hat_max_milli']} |")

    # ---- gate 1: lag --------------------------------------------------------------------------------------------------
    A("\n## Gate 1: reclaim lag (per pass line, `how=urgent` lines excluded)\n")
    A("Median over `kind=Recheck` pass lines of `lag_mean_ms`; the target is <= 1.5 s plus the mark time (median `mark_ms` of the same lines). "
      "`excl. onset` leaves out the first Recheck line after each pressure onset (it measures the grace shrink, not the steady lag). "
      "The OFF arm has no Recheck lines; its regular passes (Whole / Pipelined) are given for comparison.\n")
    A("| set | arm | pass kinds | lines (objects) | median lag_mean ms | p90 lag_mean ms | median lag_max ms | worst lag_max ms | median mark_ms | target (1500 + mark) | verdict |")
    A("|---|---|---|---|---|---|---|---|---|---|---|")
    for setname, ck, cns, title in (("main", 64, ("w4k", "seqw"), "64K main"), ("ctl", 16, ("w4k",), "16K control"), ("zipf", 64, ("w4kz",), "64K zipf")):
        for arm in ("off", "on"):
            for grp in (None, "1", "2-5"):
                ps = cell_passes(runs, arm, ck, setname, cns, grp)
                lab = {None: "all reps", "1": "rep 1", "2-5": "reps 2-5"}[grp]
                for kinds, excl, klabel in (({RECHECK_KIND}, False, "Recheck"), ({RECHECK_KIND}, True, "Recheck excl. onset"),
                                            ({"Whole", "Pipelined"}, False, "regular")):
                    s = lag_stats(ps, kinds, excl)
                    if not s:
                        continue
                    tgt = 1500 + s["mark_med"]
                    verdict = ("within target" if s["mean_med"] <= tgt else "ABOVE target") if klabel.startswith("Recheck") else "(reference)"
                    A(f"| {title} {lab} | {arm.upper()} | {klabel} | {s['n']} ({s['objects']}) | {s['mean_med']:.0f} | {f1(s['mean_p90'], 0)} | {s['max_med']:.0f} | {s['max_worst']} | {s['mark_med']:.0f} | {tgt:.0f} | {verdict} |")
                    summary.setdefault("lag", {})[f"{title}|{arm}|{lab}|{klabel}"] = s

    # ---- gate 3: mark_ms -----------------------------------------------------------------------------------------------
    A("\n## Gate 3: mark_ms per pass\n")
    A("A recheck pass may start `max(delay, 3 x mark)` after the last pass; at `mark_ms` >= 1,670 `fast_passes == 0` is the expected behaviour.\n")
    A("| set | arm | passes | mark_ms median / p90 / max | passes with mark >= 1,670 ms | fast_passes (snapshot at block end) | regular_passes (snapshot) |")
    A("|---|---|---|---|---|---|---|")
    for setname, ck, cns, title in (("main", 64, ("w4k", "seqw"), "64K main"), ("ctl", 16, ("w4k",), "16K control"), ("zipf", 64, ("w4kz",), "64K zipf")):
        for arm in ("off", "on"):
            ps = cell_passes(runs, arm, ck, setname, cns)
            rs = [r for r in runs.values() if r["arm"] == arm and r["ck"] == ck and r["set"] == setname]
            if not ps:
                continue
            marks = [p["mark_ms"] for p in ps]
            fp = [r["reclaim_final"]["fast_passes"] for r in rs if r["reclaim_final"]]
            rp = [r["reclaim_final"]["regular_passes"] for r in rs if r["reclaim_final"]]
            A(f"| {title} | {arm.upper()} | {len(ps)} | {f1(med(marks), 0)} / {f1(pct(marks, 90), 0)} / {max(marks)} | {sum(1 for x in marks if x >= 1670)} | {', '.join(map(str, fp)) or '-'} | {', '.join(map(str, rp)) or '-'} |")

    # ---- gate 2: onset ----------------------------------------------------------------------------------------------
    A("\n## Gate 2: pressure onset to first delete (see the per-repetition table for every rep)\n")
    A("| set | arm | group | reps with an onset | onset to first delete s (med / worst) | first Recheck line s (med / worst) | first Recheck pass wall ms (med / worst) |")
    A("|---|---|---|---|---|---|---|")
    for setname, ck, cn, title in (("main", 64, "w4k", "64K w4k"), ("ctl", 16, "w4k", "16K w4k"), ("zipf", 64, "w4kz", "64K zipf")):
        for arm in ("off", "on"):
            for grp, lab in (("1", "rep 1"), ("2-5", "reps 2-5")):
                cs = sel(runs, arm, ck, setname, cn, grp)
                on = [c for c in cs if c["onset_ts"]]
                if not cs:
                    continue
                fd = [c["onset_first_delete_s"] for c in on if c["onset_first_delete_s"] is not None]
                fr = [c["onset_first_recheck_s"] for c in on if c["onset_first_recheck_s"] is not None]
                fw = [c["onset_first_recheck_wall_ms"] for c in on if c["onset_first_recheck_wall_ms"] is not None]
                A(f"| {title} | {arm.upper()} | {lab} | {len(on)} of {len(cs)} | {f1(med(fd))} / {f1(max(fd) if fd else None)} (n={len(fd)}) | "
                  f"{f1(med(fr))} / {f1(max(fr) if fr else None)} (n={len(fr)}) | {f1(med(fw), 0)} / {f1(max(fw) if fw else None, 0)} |")

    # ---- gate 4 / 5: share and starvation ------------------------------------------------------------------------------
    A("\n## Gate 4: share of the reclaim done by Recheck passes (pass lines inside the measured windows of the cells)\n")
    A("| set | arm | passes Recheck / regular | segments deleted Recheck / all | bytes freed Recheck / all (share) | snapshot fast_segments_deleted / fast_bytes_freed at block end |")
    A("|---|---|---|---|---|---|")
    for setname, ck, cns, title in (("main", 64, ("w4k", "seqw"), "64K main"), ("ctl", 16, ("w4k",), "16K control"), ("zipf", 64, ("w4kz",), "64K zipf")):
        for arm in ("off", "on"):
            ps = cell_passes(runs, arm, ck, setname, cns)
            if not ps:
                continue
            rs = [r for r in runs.values() if r["arm"] == arm and r["ck"] == ck and r["set"] == setname]
            rch = [p for p in ps if p["kind"] == RECHECK_KIND]
            sd_a, bf_a = sum(p["segments_deleted"] for p in ps), sum(p["bytes_freed"] for p in ps)
            sd_r, bf_r = sum(p["segments_deleted"] for p in rch), sum(p["bytes_freed"] for p in rch)
            snap = [(r["reclaim_final"]["fast_segments_deleted"], r["reclaim_final"]["fast_bytes_freed"]) for r in rs if r["reclaim_final"]]
            snaptxt = "; ".join(f"{a} / {b / 2**30:.2f} GiB" for a, b in snap) or "-"
            if sd_a:
                A(f"| {title} | {arm.upper()} | {len(rch)} / {len(ps) - len(rch)} | {sd_r} / {sd_a} ({100 * sd_r / sd_a:.0f} %) | "
                  f"{bf_r / 2**30:.2f} / {bf_a / 2**30:.2f} GiB ({100 * bf_r / bf_a if bf_a else 0:.0f} %) | {snaptxt} |")
            else:
                A(f"| {title} | {arm.upper()} | {len(rch)} / {len(ps) - len(rch)} | 0 / 0 | - | {snaptxt} |")
            summary.setdefault("share", {})[f"{title}|{arm}"] = dict(recheck_passes=len(rch), regular_passes=len(ps) - len(rch), seg_recheck=sd_r, seg_all=sd_a, bytes_recheck=bf_r, bytes_all=bf_a)

    A("\n## Gate 5: regular passes and compaction volume on the skewed cell (ON / OFF >= 80 %)\n")
    A("Regular = every pass line that is not `kind=Recheck` (urgent included), inside the five measured windows of the zipf block.\n")
    A("| arm | regular passes | segments compacted | bytes moved GiB | Recheck passes | ON / OFF regular passes | ON / OFF segments compacted | ON / OFF bytes moved | verdict |")
    A("|---|---|---|---|---|---|---|---|---|")
    zs = {}
    for arm in ("off", "on"):
        ps = cell_passes(runs, arm, 64, "zipf", ("w4kz",))
        reg = [p for p in ps if p["kind"] != RECHECK_KIND]
        zs[arm] = dict(passes=len(reg), compacted=sum(p["compacted"] for p in reg), moved=sum(p["bytes_moved"] for p in reg),
                       recheck=len(ps) - len(reg), recheck_compacted=sum(p["compacted"] for p in ps if p["kind"] == RECHECK_KIND))
    for arm in ("off", "on"):
        z = zs[arm]
        if arm == "on" and zs["off"]["passes"]:
            o = zs["off"]
            ratios = (100 * z["passes"] / o["passes"], 100 * z["compacted"] / o["compacted"] if o["compacted"] else None,
                      100 * z["moved"] / o["moved"] if o["moved"] else None)
            ok = all(x is None or x >= 80 for x in ratios)
            A(f"| ON | {z['passes']} | {z['compacted']} | {z['moved'] / 2**30:.2f} | {z['recheck']} | {ratios[0]:.0f} % | {f1(ratios[1], 0)} % | {f1(ratios[2], 0)} % | {'PASS' if ok else 'FAIL'} |")
            summary["skew_ratios"] = dict(passes=ratios[0], compacted=ratios[1], moved=ratios[2], ok=ok)
        else:
            A(f"| {arm.upper()} | {z['passes']} | {z['compacted']} | {z['moved'] / 2**30:.2f} | {z['recheck']} | - | - | - | reference |")
    A("\nCompaction counters from the daemon's `gc --status` (cumulative since the daemon started, last sample of the block minus the first sample of its first cell, so the fill is excluded; sampled every 5 s):\n")
    A("| run | segments_compacted | compaction_read GiB | compaction_written GiB | passes (collector) |")
    A("|---|---|---|---|---|")
    for name, r in runs.items():
        first = next((c["gcstats_first"] for c in r["cells"] if c["gcstats_first"]), None)
        last = next((c["gcstats_last"] for c in reversed(r["cells"]) if c["gcstats_last"]), None)
        if first and last:
            A(f"| {name} | {last['segments_compacted'] - first['segments_compacted']} | {(last['compaction_read'] - first['compaction_read']) / 2**30:.2f} | {(last['compaction_written'] - first['compaction_written']) / 2**30:.2f} | {last['passes'] - first['passes']} |")

    # ---- gate 6: GC share, cut time, commit latency ---------------------------------------------------------------------
    A("\n## Gate 6: GC share of the coordinator, cut time, write latency\n")
    A("GC share = time inside collector units (pass profile, real clock, from the `phases` field of the pass lines) over the repetition's wall time, "
      "so it is the share of the coordinator thread's wall time; target <= 30 %. Pass lines that straddle the window edges are counted whole. "
      "Cut time = `last_cut_ms` of the status, sampled every 5 s and pooled over the repetitions of the group (n samples; the mean and the p90 are steadier than the median at this n). Commit latency is not measured separately: p99 and p99.9 of the fio completion are given.\n")
    A("| set | arm | group | GC % (med / p90 / worst) | of which Recheck % (med) | cut ms (n; median / mean / p90 of the samples) | p99 s (med) | p99.9 s (med) | commits per rep (med) |")
    A("|---|---|---|---|---|---|---|---|---|")
    for setname, ck, cn, title in (("main", 64, "w4k", "64K w4k"), ("main", 64, "seqw", "64K seqw"), ("ctl", 16, "w4k", "16K w4k"), ("zipf", 64, "w4kz", "64K zipf")):
        for arm in ("off", "on"):
            for grp, lab in (("1", "rep 1"), ("2-5", "reps 2-5")):
                cs = sel(runs, arm, ck, setname, cn, grp)
                if not cs:
                    continue
                gs = [c["gc_share_pct"] for c in cs]
                rr = [100.0 * c["gc_active_recheck_ms"] / (c["window"][1] - c["window"][0]) for c in cs]
                A(f"| {title} | {arm.upper()} | {lab} | {f1(med(gs))} / {f1(pct(gs, 90))} / {f1(max(gs))} | {f1(med(rr))} | {cut_txt(cs)} | {f1(med([c['p99_s'] for c in cs]), 2)} | {f1(med([c['p999_s'] for c in cs]), 2)} | {f1(med([c['commits'] for c in cs]), 0)} |")
                summary.setdefault("gc_share", {})[f"{title}|{arm}|{lab}"] = dict(med=med(gs), p90=pct(gs, 90), worst=max(gs))

    # ---- gate 7: aborts, urgent --------------------------------------------------------------------------------------
    A("\n## Gate 7: aborted passes and urgent passes that stopped early (whole block, fill included)\n")
    A("| run | `gc pass abandoned` | urgent pass stopped early | urgent compaction stopped | ladder urgent passes (failed) | urgent pass lines (`how=urgent`) |")
    A("|---|---|---|---|---|---|")
    for name, r in runs.items():
        w = r["warn"]
        A(f"| {name} | {len(w['abandoned'])} | {len(w['urgent_early'])} | {len(w['urgent_compaction'])} | {len(w['ladder_urgent'])} ({len(w['ladder_failed'])}) | {sum(1 for p in r['passes'] if p['how'] == 'urgent')} |")

    # ---- gate 8: evidence ----------------------------------------------------------------------------------------------
    A("\n## Gate 8: evidence objects per minute against EVIDENCE_KEEP = 1,024\n")
    A("Evidence objects are numbered; the highest `pass=` number of a block is the number written (aborted passes included). "
      "Per minute: over the measured windows of the block; the maximum is the largest count in any 60 s window of a repetition. "
      "History = 1,024 divided by the rate (minutes of evidence the daemon keeps).\n")
    A("| run | evidence objects written (final pass number) | block minutes (first to last pass line) | objects / min (block) | max in a 60 s window | history at the max rate (min) |")
    A("|---|---|---|---|---|---|")
    for name, r in runs.items():
        ps = r["passes"]
        if not ps:
            continue
        span = (max(p["ts"] for p in ps) - min(p["ts"] for p in ps)) / 60000
        mx = max([c["evidence_max_per_min"] for c in r["cells"]] or [0])
        A(f"| {name} | {r['final_pass']} | {span:.1f} | {r['final_pass'] / span if span else 0:.1f} | {mx} | {f1(1024 / mx if mx else None, 0)} |")

    # ---- A_hat error --------------------------------------------------------------------------------------------------
    A("\n## A_hat error per cut and weighted by journal bytes (cuts inside the repetitions' own windows only)\n")
    A("Error = measured factor of the cut (`a_real_milli`) minus the factor it was projected with (`a_pred_milli`), milli. "
      "`byte-weighted` weights each cut by its journal bytes. measured = store bytes / journal bytes of all cuts of the group "
      "(a measurement); predicted = the A_hat-projected figure over the same bytes.\n")
    A("| arm | chunk | cell | group | cuts | journal GiB | measured factor | predicted factor | error p10 / p50 / p90 (per cut) | error p10 / p50 / p90 (byte-weighted) | under-predicted: cuts / bytes | under by > 20 %: cuts / bytes | mean under-prediction per cut MiB |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for arm in ("off", "on"):
        for ck, cn in ((64, "w4k"), (16, "w4k"), (64, "w4kz")):
            for grp, lab in (("1", "rep 1"), ("2-5", "reps 2-5")):
                cuts = [x for c in sel(runs, arm, ck, None, cn, grp) for x in c["cuts"]]
                s = cut_stats(cuts)
                if not s:
                    continue
                A(f"| {arm.upper()} | {ck} | {cn} | {lab} | {s['n']} | {s['journal_bytes'] / 2**30:.2f} | {s['measured_factor']:.2f} | {s['predicted_factor']:.2f} | "
                  f"{f1(s['err_p10'], 0)} / {f1(s['err_p50'], 0)} / {f1(s['err_p90'], 0)} | {f1(s['werr_p10'], 0)} / {f1(s['werr_p50'], 0)} / {f1(s['werr_p90'], 0)} | "
                  f"{s['under_n']} ({100 * s['under_n'] / s['n']:.0f} %) / {s['under_bytes_pct']:.0f} % | {s['under20_n']} ({100 * s['under20_n'] / s['n']:.0f} %) / {s['under20_bytes_pct']:.0f} % | {s['underpred_bytes_mib']:.1f} |")

    # ---- blocks and integrity -------------------------------------------------------------------------------------------
    A("\n## Per block of five repetitions (one daemon, one fresh volume)\n")
    A("| run | cell | reps with a stop | stops | longest completion median s | worst s | IOPS median | IOPS p10 |")
    A("|---|---|---|---|---|---|---|---|")
    for name, r in runs.items():
        for cn in CELL_ORDER:
            cs = [c for c in r["cells"] if c["cell"] == cn]
            if not cs:
                continue
            A(f"| {name} | {cn} | {sum(1 for c in cs if c['stops'] > 0)} of {len(cs)} | {sum(c['stops'] for c in cs)} | {f1(med([c['max_s'] for c in cs]))} | {f1(max(c['max_s'] for c in cs))} | {f1(med([c['iops'] for c in cs]), 0)} | {f1(pct([c['iops'] for c in cs], 10), 0)} |")

    A("\n## Integrity (`fsck --deep` of every final volume)\n")
    A("| run | result.txt (0 = pass) | fsck exit | fsck line | EIO/ENOSPC lines in daemon log | fio errors (all cells) | enospc + fulls deltas | daemon log MiB |")
    A("|---|---|---|---|---|---|---|---|")
    for name, r in runs.items():
        A(f"| {name} | {r['result']} | {r['fsck_exit']} | {r['fsck_line'][:60]} | {r['eio_log']} | {sum(c['err'] for c in r['cells'])} | {sum(c['enospc'] + c['fulls'] for c in r['cells'])} | {r['log_bytes'] / 2**20:.1f} |")

    print("\n".join(out))
    if "--json" in sys.argv:
        for name, r in runs.items():
            summary.setdefault("runs", {})[name] = dict(
                arm=r["arm"], chunk_k=r["ck"], set=r["set"], result=r["result"], fsck_exit=r["fsck_exit"], eio_enospc_log_lines=r["eio_log"],
                abandoned=len(r["warn"]["abandoned"]), urgent_early=len(r["warn"]["urgent_early"]), final_pass=r["final_pass"],
                reclaim_final=r["reclaim_final"],
                cells=[{k: v for k, v in c.items() if k not in ("cuts", "passes", "window", "reclaim_first", "gcstats_first", "gcstats_last", "reclaim_last")} | dict(n_cuts=len(c["cuts"]))
                       for c in r["cells"]])
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as fh:
            json.dump(summary, fh, indent=1, default=str)


if __name__ == "__main__":
    main()
