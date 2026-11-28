#!/usr/bin/env python3
"""shipcheck: every `write throttle off` line of every daemon log, against the headroom the status samples report around it.

  throttle_off.py RESULTS_DIR > throttle-off.md

The off line carries no headroom; the sample before and the sample after it (series.jsonl, every 2 s: throttle.headroom, soft, resume, state)
and the headroom of the `write throttle on` line that opened the throttled stretch are printed. `low` = the nearest sample is below 1.2 GiB.
"""
import glob
import json
import os
import re
import sys

GiB = 2 ** 30
ON = re.compile(r"^(\d+) INFO\s+lunix_engine::pressure: write throttle on headroom=(\d+) soft=(\d+)")
OFF = re.compile(r"^(\d+) INFO\s+lunix_engine::pressure: write throttle off throttled_ms=(\d+)")
STOP = re.compile(r"^(\d+) \S+\s+lunix_engine::pressure: write throttle stop")


def main():
    root = sys.argv[1]
    print("# write throttle off lines\n")
    print("| block | on lines | off lines | stop lines | off with the sample before / after below 1.2 GiB | min headroom at off GiB (before / after) | max | on-line headroom GiB (min / max) |")
    print("|---|---|---|---|---|---|---|---|")
    detail = []
    for d in sorted(glob.glob(os.path.join(root, "b*-*-*-*"))):
        name = os.path.basename(d)
        ser = []
        try:
            for l in open(os.path.join(d, "series.jsonl")):
                x = json.loads(l)
                th = (x.get("status") or {}).get("throttle") or {}
                if th.get("headroom") is not None:
                    ser.append((x["t"], th["headroom"], th.get("state"), th.get("soft"), th.get("resume")))
        except OSError:
            pass
        ons, offs, stops, last_on = [], [], 0, None
        for lp in sorted(glob.glob(os.path.join(d, "lunixd.*.log"))):
            for l in open(lp, errors="replace"):
                m = ON.match(l)
                if m:
                    last_on = (int(m[1]), int(m[2]), int(m[3]))
                    ons.append(last_on)
                    continue
                if STOP.match(l):
                    stops += 1
                    continue
                m = OFF.match(l)
                if m:
                    t = int(m[1])
                    bef = [s for s in ser if s[0] <= t]
                    aft = [s for s in ser if s[0] > t]
                    b, a = (bef[-1] if bef else None), (aft[0] if aft else None)
                    offs.append((t, int(m[2]), b, a, last_on))
        if not ons and not offs:
            print(f"| {name} | 0 | 0 | {stops} | - | - | - | - |")
            continue
        hs = [(x[2][1] if x[2] else None, x[3][1] if x[3] else None) for x in offs]
        low = sum(1 for b, a in hs if (b is not None and b < 1.2 * GiB) or (a is not None and a < 1.2 * GiB))
        allh = [h for pair in hs for h in pair if h is not None]
        onh = [o[1] for o in ons]
        print(f"| {name} | {len(ons)} | {len(offs)} | {stops} | {low} | {min(allh) / GiB:.2f} | {max(allh) / GiB:.2f} | "
              f"{(min(onh) / GiB if onh else float('nan')):.2f} / {(max(onh) / GiB if onh else float('nan')):.2f} |" if allh else f"| {name} | {len(ons)} | {len(offs)} | {stops} | - | - | - | - |")
        for t, thr, b, a, o in offs:
            detail.append((name, t, thr, b, a, o))
    print("\n## Off lines with a sample below 1.2 GiB next to them\n")
    print("| block | log ts ms | throttled ms | headroom before GiB (state) | headroom after GiB (state) | soft GiB | resume GiB | headroom of the opening on-line GiB |")
    print("|---|---|---|---|---|---|---|---|")
    n = 0
    for name, t, thr, b, a, o in detail:
        if not ((b and b[1] < 1.2 * GiB) or (a and a[1] < 1.2 * GiB)):
            continue
        n += 1
        ref = b or a
        f = lambda x: f"{x[1] / GiB:.2f} ({x[2]})" if x else "-"
        print(f"| {name} | {t} | {thr} | {f(b)} | {f(a)} | {(ref[3] or 0) / GiB:.2f} | {(ref[4] or 0) / GiB:.2f} | {(o[1] / GiB if o else float('nan')):.2f} |")
    if n == 0:
        print("| none | | | | | | | |")


main()
