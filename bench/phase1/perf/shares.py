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
# Stacks that perf could not unwind past the leaf are classified by the leaf alone (the CPU rules at the end).
RULES = [
    ("zero_rewrite", ["zero_range", "reinit_slot"]),
    ("gc_urgent_mark", ["urgent"]),
    ("gc_mark", ["lunix_gc::mark", "Mark::unit"]),
    ("gc_other", ["lunix_gc::", "gc_step", "gc_share", "with_collector"]),
    ("packer_seal_fsync", ["ContainerWriter>::seal", "ContainerWriter::seal"]),
    ("packer_other", ["lunix_destage::pack", "Packer", "ContainerWriter"]),
    ("c8_index", ["true_up_index", "lunix_index::", "retry_persist"]),
    ("c5_map_seal", ["seal_map", "lunix_map::"]),
    ("c6_full_c7_checkpoint", ["write_full", "lunix_engine::full", "commit_full", "checkpoint", "rollback_floor"]),
    ("c9_reclaim", ["Journal>::reclaim", "journal::reclaim", "reclaim_covered", "lunix_journal::ring"]),
    ("destage_worker_cpu", ["_blake3", "ZSTD_", "crc32c", "aes", "sha2", "lz4"]),
    ("destage_worker_other", ["lunix_destage::", "lunix_engine::destage"]),
    ("journal_flush", ["journal_flush", "lunix_journal::"]),
    ("queue_io", ["lunix_engine::queue", "lunix_ublk::", "lunix_overlay", "io_cqring_wait"]),
]


def classify(frames, role=""):
    for cat, keys in RULES:
        for f in frames:
            for k in keys:
                if k in f:
                    return cat
    # an O_DIRECT write issued by the coordinator thread is the reclaim zero rewrite (its only direct write)
    if role == "coordinator" and any("direct_write" in f for f in frames):
        return "zero_rewrite"
    # kernel-only stacks (perf lost the user frames): waits on writeback or fsync of the coordinator thread, that is the
    # segment seals of the packer (fdatasync), the checkpoint and the index runs
    if role == "coordinator" and any(k in f for f in frames for k in ("filemap_fdatawait_range", "do_fsync", "btrfs_sync_file", "wbt_wait", "write_all_supers", "btrfs_btree_wait", "extent_buffer")):
        return "fsync_writeback_wait"
    if any("futex" in f for f in frames):
        return "futex_wait"
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


def role_of(names):
    """tid -> role: the lowest tid named lunix-coord is the coordinator thread; the other lunix-coord tids are destage workers
    (scoped threads that inherit the name); other threads keep their name."""
    coord = sorted(t for t, n in names.items() if n == "lunix-coord")
    r = {t: n for t, n in names.items()}
    for t in coord:
        r[t] = "coordinator" if t == coord[0] else "destage_workers"
    return r


def oncpu(path):
    blocks = list(read_blocks(path))
    roles = role_of({b["tid"]: b["comm"] for b in blocks})
    sec = collections.defaultdict(lambda: collections.defaultdict(float))
    for b in blocks:
        role = roles[b["tid"]]
        sec[role][classify(b["frames"], role)] += 1.0 / HZ
    return sec


def offcpu(path):
    """Intervals a thread is off CPU: from its switch-out (state not R) to the next event that switches it in."""
    evs = sorted(read_blocks(path), key=lambda b: b["t"])
    sw = re.compile(r"^(.*):(\d+) \[\d+\] (\S+) ==> (.*):(\d+) \[\d+\]")
    names = {}
    for b in evs:
        m = sw.match(b["trace"])
        if m:
            names[int(m.group(2))] = m.group(1)
    roles = role_of(names)
    out_ev = {}
    sec = collections.defaultdict(lambda: collections.defaultdict(float))
    span = [None, None]
    for b in evs:
        t = b["t"]
        m = sw.match(b["trace"])
        if not m:
            continue
        prev_name, prev_pid, state, next_name, next_pid = m.group(1), int(m.group(2)), m.group(3), m.group(4), int(m.group(5))
        if next_pid in out_ev:
            t0, frames, _st = out_ev.pop(next_pid)
            if next_name.startswith("lunix"):
                role = roles.get(next_pid, next_name)
                sec[role][classify(frames, role)] += t - t0
        if prev_name.startswith("lunix") and not state.startswith("R"):
            span[0] = t if span[0] is None else span[0]
            span[1] = t
            out_ev[prev_pid] = (t, b["frames"], state)
    return sec, (span[1] - span[0]) if span[0] is not None else 0.0


def report(title, sec, window):
    print(f"\n### {title}\n")
    cats = sorted({c for d in sec.values() for c in d})
    print("| role | total s | " + " | ".join(cats) + " |")
    print("|---|---|" + "---|" * len(cats))
    for th, d in sorted(sec.items(), key=lambda kv: -sum(kv[1].values())):
        tot = sum(d.values())
        if tot < 0.05:
            continue
        print(f"| {th} | {tot:.2f} | " + " | ".join(f"{d.get(c, 0):.2f}" if d.get(c) else "" for c in cats) + " |")
    co = sec.get("coordinator")
    if co:
        tot = sum(co.values())
        print(f"\nCoordinator thread, share of its {title.split()[0]} time ({tot:.2f} s of a {window:.1f} s window): " +
              ", ".join(f"{c} {100 * v / tot:.0f} %" for c, v in sorted(co.items(), key=lambda kv: -kv[1]) if v / tot >= 0.01))
    print(f"\n(window {window:.1f} s; destage_workers = all worker threads summed, so up to 8 s per second of window)")


def uprobes(path, window=None):
    """Durations of the probed functions (tests/perf/uprobes.sh) from an ftrace dump; prints the wall-clock shares."""
    ev = re.compile(r"^\s*(\S.*?)-(\d+)\s+\[\d+\]\s+\S+\s+([\d.]+):\s+(\w+):")
    stack = collections.defaultdict(list)   # (tid, fn) -> entry times
    dur = collections.defaultdict(list)
    cuts = []   # (tid, t_entry, t_finish_return, t_return)
    cur = {}
    first = last = None
    for line in open(path, errors="replace"):
        m = ev.match(line)
        if not m:
            continue
        tid, t, name = int(m.group(2)), float(m.group(3)), m.group(4)
        first = t if first is None else first
        last = t
        ret = name.endswith("_r")
        fn = name[:-2] if ret else name
        if not ret:
            stack[(tid, fn)].append(t)
            if fn == "cut":
                cur[tid] = [t, None]
        elif stack[(tid, fn)]:
            t0 = stack[(tid, fn)].pop()
            dur[fn].append((t0, t - t0, tid))
            if fn == "finish" and tid in cur:
                cur[tid][1] = t
            if fn == "cut" and tid in cur:
                e, f = cur.pop(tid)
                cuts.append((tid, e, f if f is not None else e, t))
    span = (last - first) if first is not None else 0.0
    print(f"\nuprobes: {path} ({span:.1f} s of events)\n")
    print("| function | calls | total s | share of the window | mean ms | max ms |")
    print("|---|---|---|---|---|---|")
    for fn in ("cut", "finish", "segseal", "cwseal", "reclaim", "zero", "gcurgent", "runurgent", "trueup"):
        v = dur.get(fn, [])
        if not v:
            print(f"| {fn} | 0 | 0 | 0 % | | |")
            continue
        tot = sum(d for _, d, _ in v)
        print(f"| {fn} | {len(v)} | {tot:.2f} | {100 * tot / span:.1f} % | {1000 * tot / len(v):.0f} | {1000 * max(d for _, d, _ in v):.0f} |")
    if cuts:
        tot = sum(r - e for _, e, _, r in cuts)
        c14 = sum(f - e for _, e, f, _ in cuts)
        c59 = sum(r - f for _, _, f, r in cuts)
        print(f"\nCuts: {len(cuts)}; in cut_and_commit {tot:.2f} s = {100 * tot / span:.0f} % of the window (the rest is the coordinator waiting for the next trigger or in the GC step);")
        print(f"C1-C4 (to the return of Packer::finish) {c14:.2f} s = {100 * c14 / tot:.0f} % of the cuts, C5-C9 {c59:.2f} s = {100 * c59 / tot:.0f} % of the cuts ({100 * c59 / span:.1f} % of the window).")
        rec = sum(d for _, d, _ in dur.get("reclaim", []))
        zr = sum(d for _, d, _ in dur.get("zero", []))
        print(f"Reclaim (C9) {rec:.2f} s, of which the zero rewrite {zr:.2f} s = {100 * zr / tot:.0f} % of the cuts ({100 * zr / span:.1f} % of the window).")


def main():
    d = sys.argv[1]
    import os
    if d == "--uprobes":
        uprobes(sys.argv[2])
        return
    p = os.path.join(d, "oncpu-script.txt.gz")
    if os.path.exists(p):
        report("on-CPU time (cpu-clock samples at 499 Hz)", oncpu(p), 8.0)
    p = os.path.join(d, "offcpu-script.txt.gz")
    if os.path.exists(p):
        sec, w = offcpu(p)
        report("blocked time (sched_switch intervals)", sec, w)


if __name__ == "__main__":
    main()
