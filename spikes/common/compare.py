#!/usr/bin/env python3
"""Pair two summary.csv files (kernel reference vs ublk) and print a Markdown table with ratios.
Usage: compare.py <ref-summary.csv> <ublk-summary.csv> [cell-prefix-filter]"""
import csv, sys

def load(p):
    return {r["cell"]: r for r in csv.DictReader(open(p))}

ref, ublk = load(sys.argv[1]), load(sys.argv[2])
flt = sys.argv[3] if len(sys.argv) > 3 else ""
f = lambda r, k: float(r[k]) if r.get(k) not in (None, "") else float("nan")
print("| cell | ref IOPS (min-max) | ublk IOPS (min-max) | ublk/ref | ref MB/s | ublk MB/s | ref lat p50/p99/p99.9 us | ublk lat p50/p99/p99.9 us | ublk daemon CPU us/IO | ublk fio usr+sys % | noisy |")
print("|---|---|---|---|---|---|---|---|---|---|---|")
for c, u in ublk.items():
    if flt and not c.startswith(flt):
        continue
    r = ref.get(c)
    if not r:
        continue
    print(f'| {c} | {f(r,"iops_med"):.0f} ({f(r,"iops_min"):.0f}-{f(r,"iops_max"):.0f}) | {f(u,"iops_med"):.0f} ({f(u,"iops_min"):.0f}-{f(u,"iops_max"):.0f}) | '
          f'{f(u,"iops_med") / f(r,"iops_med"):.2f} | {f(r,"mbps_med"):.0f} | {f(u,"mbps_med"):.0f} | '
          f'{f(r,"lat_p50_us"):.1f}/{f(r,"lat_p99_us"):.1f}/{f(r,"lat_p999_us"):.1f} | {f(u,"lat_p50_us"):.1f}/{f(u,"lat_p99_us"):.1f}/{f(u,"lat_p999_us"):.1f} | '
          f'{f(u,"daemon_cpu_us_per_io"):.2f} | {f(u,"fio_usr_cpu") + f(u,"fio_sys_cpu"):.0f} | {r["noisy"]}{u["noisy"]} |')
