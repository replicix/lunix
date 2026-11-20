#!/usr/bin/env python3
"""ADR-33 T6: shares of wall time per daemon thread and category, from the SHARES=1 profiles of tests/perf/partA.sh.

  shares.py CELLDIR [--rules] > shares.md

CELLDIR holds oncpu-script.txt.gz (perf record -e cpu-clock -F 499 --call-graph dwarf, 8 s) and offcpu-script.txt.gz
(sched:sched_switch with the raw trace fields, dwarf, 6 s) of one cell, and thread-states.txt.

Per thread name: seconds on CPU (samples / 499) and seconds blocked (interval from a switch-out to the thread's next
switch-in; switch-outs in state R are preemptions and not counted), each split into categories by the frames of the stack
(first matching rule wins, rules below). Shares are against the window of the profile (8 s on CPU, 6 s off CPU), per thread
(one thread = 100 % of the window) and, for the coordinator, against the time between its first and last event.
"""
import collections
import gzip
import re
import sys

HZ = 499.0

# (category, frame substrings): first rule with any frame matching wins. Frames are as perf prints them (demangled, inlined).
RULES = [
    ("zero_rewrite", ["zero_range"]),
    ("gc_urgent_mark", ["urgent"]),
    ("gc_mark_other", ["lunix_gc::mark", "Mark::unit"]),
    ("gc_other", ["lunix_gc::", "gc_step", "gc_share", "with_collector"]),
    ("packer", ["lunix_destage::pack", "Packer", "ContainerWriter", "lunix_store::container"]),
    ("destage_workers", ["lunix_destage::", "lunix_engine::destage"]),
    ("c9_reclaim", ["Journal>::reclaim", "journal::reclaim", "reclaim_covered", "lunix_journal::ring"]),
    ("c8_index", ["true_up_index", "lunix_index::", "retry_persist"]),
    ("c7_checkpoint", ["checkpoint", "rollback_floor", "persist"]),
    ("c6_full", ["write_full", "lunix_engine::full", "commit_full"]),
    ("c5_map_seal", ["seal_map", "lunix_map::"]),
    ("journal_flush", ["journal_flush", "lunix_journal::"]),
    ("queue_io", ["lunix_engine::queue", "lunix_ublk::", "lunix_overlay"]),
]


def classify(frames):
    for cat, keys in RULES:
        for f in frames:
            for k in keys:
                if k in f:
                    return cat
    return "other"


def read_blocks(path):
    """perf script output: a header line (comm tid time: [trace]) then indented frame lines, blocks split by blank lines."""
    op = gzip.open if path.endswith(".gz") else open
    cur = None
    with op(path, "rt", errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip():
                if cur:
                    yield cur
                cur = None
            elif not line.startswith((" ", "\t")):
                if cur:
                    yield cur
                m = re.match(r"\s*(\S.*?)\s+(\d+)\s+([\d.]+):\s*(.*)$", line)
                cur = {"comm": m.group(1), "tid": int(m.group(2)), "t": float(m.group(3)), "trace": m.group(4), "frames": []} if m else None
            elif cur is not None:
                parts = line.strip().split(None, 1)
                cur["frames"].append(parts[1] if len(parts) > 1 else parts[0])
    if cur:
        yield cur


def oncpu(path):
    sec = collections.defaultdict(lambda: collections.defaultdict(float))
    for b in read_blocks(path):
        sec[b["comm"]][classify(b["frames"])] += 1.0 / HZ
    return sec


def offcpu(path):
    evs = sorted(read_blocks(path), key=lambda b: b["t"])
    out_ev = {}
    sec = collections.defaultdict(lambda: collections.defaultdict(float))
    names = {}
    span = [None, None]
    for b in evs:
        t = b["t"]
        span[0] = t if span[0] is None else span[0]
        span[1] = t
        m = re.search(r"prev_state=(\S+)", b["trace"])
        nxt = re.search(r"next_pid=(\d+)", b["trace"])
        names[b["tid"]] = b["comm"]
        if nxt and int(nxt.group(1)) in out_ev:
            tid = int(nxt.group(1))
            t0, frames, _st = out_ev.pop(tid)
            sec[names.get(tid, str(tid))][classify(frames)] += t - t0
        state = m.group(1) if m else "?"
        if not state.startswith("R"):
            out_ev[b["tid"]] = (t, b["frames"], state)
    return sec, (span[1] - span[0]) if span[0] is not None else 0.0


def report(title, sec, window):
    print(f"\n### {title}\n")
    cats = sorted({c for d in sec.values() for c in d})
    print("| thread | total s | " + " | ".join(cats) + " |")
    print("|---|---|" + "---|" * len(cats))
    for th, d in sorted(sec.items(), key=lambda kv: -sum(kv[1].values())):
        tot = sum(d.values())
        if tot < 0.05:
            continue
        print(f"| {th} | {tot:.2f} | " + " | ".join(f"{d.get(c, 0):.2f}" if d.get(c) else "" for c in cats) + " |")
    print(f"\n(window {window:.1f} s; seconds per thread name, summed over its threads)")


def main():
    d = sys.argv[1]
    import os
    p = os.path.join(d, "oncpu-script.txt.gz")
    if os.path.exists(p):
        report("on CPU (cpu-clock samples at 499 Hz)", oncpu(p), 8.0)
    p = os.path.join(d, "offcpu-script.txt.gz")
    if os.path.exists(p):
        sec, w = offcpu(p)
        report("blocked (sched_switch intervals)", sec, w)


if __name__ == "__main__":
    main()
