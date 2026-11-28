#!/usr/bin/env python3
"""c5def: the gate of making gc_condemn_on_cut = "on" the default, on the real daemon (tests/perf/c5def.sh), built on c5t6.py.

  c5def.py RESULTS_DIR [--json OUT.json] > tables.md

Blocks b<NN>-<arm>-<chunk bytes>-<kind>: arm ON | OFF (throttle_pace_store_bytes = true) and ONS | OFFS (that key left at its default);
kind w4k | w4kz | pin | restart. Clauses A-F are those of the design (c5-default-on-design.md section 3.4); F as corrected by the maintainer
(zero_waiting against the segment files that exist at each sample, series.jsonl `nseg`, and no climb across the pinned reps).
"""
import glob
import json
import os
import statistics as st
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import c5t6  # noqa: E402
from c5t6 import f1, jl, med  # noqa: E402

GiB = 2 ** 30
ON_ARMS, OFF_ARMS = ("ON", "ONS"), ("OFF", "OFFS")


def verdict(ok):
    return "PASS" if ok else "FAIL"


def rep1_metrics(r):
    on_ms = r["lo"] + r["onset_s"] * 1000 if r["onset_s"] is not None else None
    cands = [x for x in (r["onset_first_delete_s"], r["onset_first_proposal_delete_s"]) if x is not None]
    gib5 = None
    if on_ms is not None:
        gib5 = sum(p["bytes_freed"] for p in r["passes"] if on_ms <= p["ts"] <= on_ms + 5000) / GiB
    return dict(onset_s=r["onset_s"], first_delete_s=min(cands) if cands else None, stops=r["stops"], wait_s=r["max_s"],
                throttled_s=r["throttled_ms"] / 1000, gib5=gib5, iops=r["iops"], p999_s=r["p999_s"])


def rep1_clauses(m):
    return dict(first_delete=m["first_delete_s"] is not None and m["first_delete_s"] <= 3, stops=m["stops"] == 0, wait=m["wait_s"] <= 5,
                throttled=m["throttled_s"] <= 22, gib5=(m["gib5"] or 0) >= 6)


def final_of(b):
    return jl(os.path.join(b["dir"], "raw", "status-final.json")) or {}


def safety(b):
    T, rc, stt = b["T"], (b["final"] or {}).get("reclaim", {}), final_of(b).get("stats", {})
    on = b["arm"] in ON_ARMS
    reps = b["reps"]
    fio_err = sum(r["err"] for r in reps) + (0 if reps and all(r["ok"] for r in reps) else 1)
    ver = ""
    p = os.path.join(b["dir"], "verify.txt")
    if os.path.exists(p):
        ver = open(p).read().strip()
    cond, unc = T.get("proposal_condemned", 0), T.get("proposal_uncondemned", 0)
    c = dict(
        fio=fio_err == 0, verify=ver == "rc=0 job_errors=0", fsck=b["fsck_exit"] == 0, reftable=b["ref_table_lines"] == 0,
        drift=T["drift_segments"] == 0, mismatch=T["report_mismatches"] == 0, rebases=(T["rebases"] > 0) if on else True,
        early=T.get("proposal_rejected_early", 0) == 0, named=T.get("proposal_named", 0) == 0, perr=T.get("propose_errors", 0) == 0,
        uncond=(unc * 20 <= cond) and len(b["uncond"]) == unc, hold=T.get("hold_footer_errors", 0) == 0,
        exhausted=stt.get("read_revalidate_exhausted", 0) == 0, auth=stt.get("extent_auth_failures", 0) == 0,
        pgrace=(rc.get("pgrace_min_wait_ms", 0) >= 2000) if rc.get("pgrace_deletes", 0) else True,
        eio=b["eio_log"] == 0, space=sum((r["fulls"] or 0) + (r["enospc"] or 0) for r in reps) == 0, result=b["result"] == "0")
    return c, dict(fio_err=fio_err, verify=ver, rebases=T["rebases"], cond=cond, unc=unc, min_wait=rc.get("pgrace_min_wait_ms"),
                   exhausted=stt.get("read_revalidate_exhausted"), auth=stt.get("extent_auth_failures"))


def cell_reps(blocks, arms, ck, kind, grp):
    out = []
    for b in blocks:
        if b["arm"] in arms and b["ck"] == ck and b["kind"] == kind:
            for r in b["reps"]:
                if grp == "1" and r["rep"] != 1:
                    continue
                if grp == "2-5" and r["rep"] == 1:
                    continue
                out.append(r)
    return out


def main():
    root = sys.argv[1]
    blocks = []
    for d in sorted(glob.glob(os.path.join(root, "b*-*-*-*"))):
        if os.path.isdir(d):
            b = c5t6.load_block(d)
            if b:
                b["dir"] = d
                blocks.append(b)
    out, S = [], {"clauses": {}}
    A = out.append
    A("# c5def tables\n")
    A("Arms: ON / OFF = `gc_condemn_on_cut` on / off with `throttle_pace_store_bytes = true`; ONS / OFFS = the same with that key at its default. "
      "Every arm: grace 2000 ms and fast recheck on (the defaults), `log_level = debug`. Rep 1 is the cold run after the fill, reported apart.\n")
    A("## Blocks\n")
    A("| block | arm | chunk | kind | load 1/5/15 at start | reps | result | fsck |")
    A("|---|---|---|---|---|---|---|---|")
    for b in blocks:
        A(f"| {b['name']} | {b['arm']} | {b['ck']}K | {b['kind']} | {'/'.join(f1(x, 2) for x in (b['load_start'] or []))} | {len(b['reps'])} | {b['result']} | {b['fsck_exit']} |")

    A("\n## A. Safety, every block\n")
    A("| block | fio errors | verify | fsck | ref-table lines | drift | mismatches | rebases | rejected early | named | propose errors | uncond / condemned (lines) | hold footer errors | exhausted | auth failures | min pgrace wait ms | EIO/ENOSPC | verdict |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    allok = True
    for b in blocks:
        c, x = safety(b)
        T = b["T"]
        ok = all(c.values())
        allok &= ok
        A(f"| {b['name']} | {x['fio_err']} | {x['verify'] or '-'} | {b['fsck_exit']} | {b['ref_table_lines']} | {T['drift_segments']} | {T['report_mismatches']} | {x['rebases']} | "
          f"{T.get('proposal_rejected_early', 0)} | {T.get('proposal_named', 0)} | {T.get('propose_errors', 0)} | {x['unc']} / {x['cond']} ({len(b['uncond'])}) | "
          f"{T.get('hold_footer_errors', 0)} | {x['exhausted']} | {x['auth']} | {x['min_wait']} | {b['eio_log']} | {verdict(ok)}{'' if ok else ' ' + ','.join(k for k, v in c.items() if not v)} |")
    S["clauses"]["A"] = verdict(allok)

    A("\n## B. Rep 1 (cold run after the fill)\n")
    A("| block | arm | chunk | first delete after onset s | stops | longest wait s | throttled s | GiB deleted <= 5 s | first delete <= 3 s | 0 stops | wait <= 5 s | throttled <= 22 s | >= 6 GiB | all |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    bok, off_pass, nb = True, [], 0
    for b in blocks:
        if b["kind"] != "w4k":
            continue
        r1 = next((r for r in b["reps"] if r["rep"] == 1), None)
        if not r1:
            continue
        nb += 1
        m = rep1_metrics(r1)
        c = rep1_clauses(m)
        gated = list(c.values()) if b["ck"] == 64 else [c["stops"], c["first_delete"]]
        if b["arm"] == "ON":
            bok &= all(gated)
        if b["arm"] in OFF_ARMS and b["ck"] == 64 and (c["first_delete"] or c["gib5"]):
            off_pass.append(b["name"])
        A(f"| {b['name']} | {b['arm']} | {b['ck']}K | {f1(m['first_delete_s'], 2)} | {m['stops']} | {f1(m['wait_s'])} | {f1(m['throttled_s'])} | {f1(m['gib5'], 2)} | "
          + " | ".join('pass' if v else 'FAIL' for v in c.values()) + f" | {verdict(all(c.values()))} |")
    S["clauses"]["B"] = verdict(bok and not off_pass) if nb else "NOT RUN"

    A("\n## C. Cut cost per journal byte, ON against OFF\n")
    A("| cell | arm | windows | cuts | mean cut MiB | mean cut ms | us / MiB (plain, +- se) | us / MiB at pooled size | vs OFF at pooled size | OFF se % | map ns / chunk |")
    A("|---|---|---|---|---|---|---|---|---|---|---|")
    cok, cdec = True, False
    for ck, kind in ((64, "w4k"), (16, "w4k"), (64, "w4kz")):
        allr = [r for r in cell_reps(blocks, ("ON", "OFF"), ck, kind, "all") if r["cuts"]]
        if not allr:
            continue
        x0 = sum(r["cut_bytes_sum"] for r in allr) / 2 ** 20 / sum(r["cuts"] for r in allr)
        res = {arm: c5t6.cost(cell_reps(blocks, (arm,), ck, kind, "all"), x0) for arm in ("OFF", "ON")}
        base, on = res["OFF"], res["ON"]
        rel = None
        if base and on and base["us_per_mib_at_x0"] and on["us_per_mib_at_x0"]:
            rel = (on["us_per_mib_at_x0"] / base["us_per_mib_at_x0"] - 1) * 100
        se = 100 * base["us_per_mib_rep_se"] / base["us_per_mib"] if base and base["us_per_mib_rep_se"] else None
        for arm in ("OFF", "ON"):
            c = res[arm]
            if c:
                A(f"| {ck}K {kind} | {arm} | {c['n']} | {c['cuts']} | {f1(c['mean_cut_mib'], 1)} | {f1(c['mean_cut_ms'], 1)} | {f1(c['us_per_mib'], 0)} +- {f1(c['us_per_mib_rep_se'], 0)} | "
                  f"{f1(c['us_per_mib_at_x0'], 0)} ({f1(x0, 1)} MiB) | {'-' if arm == 'OFF' or rel is None else f'{rel:+.1f} %'} | {f1(se, 2) if arm == 'OFF' else '-'} | {f1(c['map_ns_per_chunk'], 0)} |")
        if kind == "w4k":
            enough = base and on and base["n"] >= 9 and on["n"] >= 9 and se is not None and se <= 1.5 and rel is not None
            if not enough:
                cdec = True
            elif rel > 3.0:
                cok = False
            S["clauses"][f"C_{ck}K"] = "PASS" if enough and rel <= 3.0 else "UNDECIDED" if not enough else "FAIL"
    missing = [k for k in ("C_64K", "C_16K") if k not in S["clauses"]]
    S["clauses"]["C"] = "NOT RUN" if missing else "FAIL" if not cok else ("UNDECIDED" if cdec else "PASS")

    A("\n## D. Steady state, reps 2-5, ON against OFF, pooled over blocks\n")
    A("| cell | metric | OFF | ON | limit | verdict |")
    A("|---|---|---|---|---|---|")
    dok, dcells = True, 0
    for ck, kind in ((64, "w4k"), (16, "w4k"), (64, "w4kz")):
        off, on = cell_reps(blocks, ("OFF",), ck, kind, "2-5"), cell_reps(blocks, ("ON",), ck, kind, "2-5")
        if not off or not on:
            continue
        dcells += 1
        tag = f"{ck}K {kind}"

        def row(name, a, b_, ok, lim, d=1):
            nonlocal dok
            dok &= ok
            A(f"| {tag} | {name} | {f1(a, d)} | {f1(b_, d)} | {lim} | {verdict(ok)} |")
        ia, ib = med([r["iops"] for r in off]), med([r["iops"] for r in on])
        row("IOPS (median)", ia, ib, ib >= 0.97 * ia, "ON >= 0.97 OFF", 0)
        pa, pb = med([r["p999_s"] for r in off]), med([r["p999_s"] for r in on])
        row("p99.9 s (median)", pa, pb, pb <= 1.10 * pa, "ON <= 1.10 OFF", 3)
        sa, sb = sum(r["stops"] for r in off), sum(r["stops"] for r in on)
        row("stops (total)", sa, sb, sb <= sa, "ON <= OFF", 0)
        wa, wb = med([r["max_s"] for r in off]), med([r["max_s"] for r in on])
        row("longest wait s (median)", wa, wb, wb <= wa + 1.0, "ON <= OFF + 1 s", 2)
        ma, mb = st.mean(r["share_mark_pct"] for r in off), st.mean(r["share_mark_pct"] for r in on)
        lim = 1.0 if ck == 64 else 2.0
        row("mark share of the coordinator % (mean)", ma, mb, mb - ma <= lim, f"ON - OFF <= {lim} point", 2)
        if kind == "w4kz":
            ca, cb = sum(r["compaction_written"] for r in off) / len(off), sum(r["compaction_written"] for r in on) / len(on)
            row("compaction written per rep MiB", ca / 2 ** 20, cb / 2 ** 20, cb >= 0.9 * ca, "ON >= 0.9 OFF", 0)
        ev = max([r["evidence_max_per_min"] for r in on] or [0])
        row("evidence objects per minute (max rep)", None, ev, ev <= 60, "<= 60", 0)
    S["clauses"]["D"] = verdict(dok) if dcells == 3 else "NOT RUN"

    A("\n## E. Proposal cost, ON blocks\n")
    A("| block | arm | reps | max propose_us_sum / wall % | mean us per call | propose_us_max us | orphan deferrals | cuts requested | verdict |")
    A("|---|---|---|---|---|---|---|---|---|")
    eok, en = True, 0
    for b in blocks:
        if b["arm"] not in ON_ARMS or b["kind"] == "pin":
            continue
        en += 1
        shares = [100 * (r["R"].get("propose_us_sum") or 0) / (r["window_s"] * 1e6) for r in b["reps"] if r["R"].get("propose_us_sum") is not None]
        T = b["T"]
        mean_us = T.get("propose_us_sum", 0) / max(1, T.get("propose_calls", 0))
        mxus = T.get("propose_us_max", 0)
        ok = bool(shares) and max(shares) <= 2.0 and mean_us <= 5000 and mxus <= 20000
        eok &= ok
        A(f"| {b['name']} | {b['arm']} | {len(shares)} | {f1(max(shares) if shares else None, 3)} | {f1(mean_us, 0)} | {mxus} | {T.get('orphan_deferrals', 0)} | {T.get('cuts_requested', 0)} | {verdict(ok)} |")
    S["clauses"]["E"] = verdict(eok) if en else "NOT RUN"

    A("\n## F. Pin cell (pin held through reps 1-3, removed before rep 4; plain volume), corrected check\n")
    A("While pinned: pin_blocked > 0, nothing offered or condemned, `zero_waiting` <= the segment files in the store at that sample "
      "(`nseg` of series.jsonl: a listing of the volume's segment directory every 2 s), and the per-rep peak of `zero_waiting` levels off over reps 1-3 "
      "(fails if every pinned rep's peak is more than 10 % above the one before). After the release: condemned > 0 in rep 5, `zero_waiting` below 64 within 30 s.\n")
    fok = None
    for b in blocks:
        if b["kind"] != "pin":
            continue
        isOn = b["arm"] in ON_ARMS
        ok_b = True
        rel_ms = None
        try:
            rel_ms = int(open(os.path.join(b["dir"], "raw", "pin-released-ms.txt")).read().strip())
        except (OSError, ValueError):
            ok_b = False
        A(f"Block {b['name']} ({b['arm']}): pin released at {rel_ms}.\n")
        A("| rep | pin_blocked delta | proposals_offered | proposal_condemned | zero_waiting peak | segment files (max) | samples with zero_waiting above the file count | refs.segments (end) | IOPS | stops |")
        A("|---|---|---|---|---|---|---|---|---|---|")
        peaks = []
        for r in b["reps"]:
            rd = os.path.join(b["dir"], "raw", r["label"])
            gb, ge = jl(os.path.join(rd, "gc-before.json")) or {}, jl(os.path.join(rd, "gc-end.json")) or {}
            pb = (ge.get("refs") or {}).get("pin_blocked", 0) - (gb.get("refs") or {}).get("pin_blocked", 0)
            smp = [(s["gc"]["refs"]["zero_waiting"], s.get("nseg")) for s in b["series"]
                   if s.get("gc") and s["gc"].get("refs") and r["lo"] <= s["t"] <= r["hi"]]
            zmax = max([z for z, _ in smp] or [0])
            over = [1 for z, n in smp if n is None or z > n]
            nsegs = max([n for _, n in smp if n is not None] or [None]) if smp else None
            segs = (ge.get("refs") or {}).get("segments", 0)
            pinned = r["rep"] <= 3
            offered, cond = r["R"].get("proposals_offered"), r["R"].get("proposal_condemned")
            if isOn:
                if pinned:
                    peaks.append(zmax)
                    ok_b &= pb > 0 and offered == 0 and cond == 0 and not over and bool(smp)
                if r["rep"] == 5:
                    ok_b &= (cond or 0) > 0
            A(f"| {r['rep']}{' (pinned)' if pinned else ''} | {pb} | {offered} | {cond} | {zmax} | {nsegs} | {len(over)} | {segs} | {f1(r['iops'], 0)} | {r['stops']} |")
        if isOn:
            climbing = len(peaks) == 3 and peaks[1] > 1.1 * peaks[0] and peaks[2] > 1.1 * peaks[1]
            A(f"\nzero_waiting peaks over pinned reps 1-3: {peaks}; {'climbs every rep: FAIL' if climbing else 'levels off'}")
            ok_b &= not climbing and len(peaks) == 3
            after = [s for s in b["series"] if rel_ms and s["t"] >= rel_ms and s.get("gc") and s["gc"].get("refs")]
            back = next((s["t"] - rel_ms for s in after if s["gc"]["refs"]["zero_waiting"] < 64), None)
            A(f"zero_waiting below 64 again {f1(back / 1000 if back is not None else None, 1)} s after the release (limit 30 s); orphan_deferrals {b['T'].get('orphan_deferrals', 0)}, hold_marks {b['T'].get('hold_marks', 0)}")
            ok_b &= back is not None and back <= 30000
            fok = ok_b if fok is None else (fok and ok_b)
        A("")
    S["clauses"]["F"] = verdict(fok) if fok is not None else "NOT RUN"

    A("\n## Shipping defaults and restart cells, reported\n")
    A("| block | arm | kind | rep 1: first delete s | stops | wait s | throttled s | GiB <= 5 s | rep 1 IOPS | reps 2-5 IOPS (med) | p99.9 s (med) | stops 2-5 | all rep-1 clauses |")
    A("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for b in blocks:
        if b["arm"] in ("ONS", "OFFS") or b["kind"] == "restart":
            r1 = next((r for r in b["reps"] if r["rep"] == 1), None)
            r25 = [r for r in b["reps"] if r["rep"] > 1]
            if r1:
                m = rep1_metrics(r1)
                A(f"| {b['name']} | {b['arm']} | {b['kind']} | {f1(m['first_delete_s'], 2)} | {m['stops']} | {f1(m['wait_s'])} | {f1(m['throttled_s'])} | {f1(m['gib5'], 2)} | {f1(m['iops'], 0)} | "
                  f"{f1(med([r['iops'] for r in r25]), 0)} | {f1(med([r['p999_s'] for r in r25]), 3)} | {sum(r['stops'] for r in r25)} | {verdict(all(rep1_clauses(m).values()))} |")
    A("\n## Clause summary\n")
    A("| clause | verdict |")
    A("|---|---|")
    for k in sorted(S["clauses"]):
        A(f"| {k} | {S['clauses'][k]} |")
    if "--json" in sys.argv:
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as f:
            json.dump(S, f, indent=1, default=str)
    print("\n".join(out))


main()
