#!/usr/bin/env python3
"""Summarise fio JSON files in a directory into CSV (median/min/max per cell)."""
import csv, glob, json, os, re, statistics, sys

d = sys.argv[1]
cpu = {}
p = os.path.join(d, "daemon_cpu.csv")
if os.path.exists(p):
    for r in csv.DictReader(open(p)):
        cpu[r["cell"]] = float(r["daemon_cpu_s"])
cells = {}
for f in sorted(glob.glob(os.path.join(d, "*.json"))):
    m = re.match(r"(.+)_r(\d+)\.json$", os.path.basename(f))
    if not m:
        continue
    try:
        j = json.load(open(f))
    except Exception:
        continue
    job = j["jobs"][0]
    ios = job["read"]["total_ios"] + job["write"]["total_ios"]
    side = job["read"] if job["read"]["total_ios"] >= job["write"]["total_ios"] else job["write"]
    rt = (job["read"]["runtime"] + job["write"]["runtime"]) / 1000.0
    pc = (side.get("lat_ns", {}).get("percentile") or side["clat_ns"]["percentile"])
    cells.setdefault(m.group(1), []).append({
        "iops": side["iops"], "bw": side["bw_bytes"] / 1e6,
        "p50": pc["50.000000"] / 1000, "p99": pc["99.000000"] / 1000,
        "p999": pc["99.900000"] / 1000, "usr": job["usr_cpu"], "sys": job["sys_cpu"],
        "ios": ios, "cpu": cpu.get(os.path.basename(f)[:-5], 0.0),
        "err": job.get("total_err", 0)})
w = csv.writer(sys.stdout)
w.writerow(["cell", "reps", "iops_med", "iops_min", "iops_max", "mbps_med", "lat_p50_us", "lat_p99_us",
            "lat_p999_us", "fio_usr_cpu", "fio_sys_cpu", "daemon_cpu_us_per_io", "noisy"])
for c, rs in cells.items():
    med = lambda k: statistics.median(r[k] for r in rs)
    iops = [r["iops"] for r in rs]
    tot_ios = sum(r["ios"] for r in rs)  # includes ramp-free runtime ios only
    cpu_s = sum(r["cpu"] for r in rs)
    w.writerow([c, len(rs), "%.0f" % med("iops"), "%.0f" % min(iops), "%.0f" % max(iops), "%.1f" % med("bw"),
                "%.1f" % med("p50"), "%.1f" % med("p99"), "%.1f" % med("p999"),
                "%.1f" % med("usr"), "%.1f" % med("sys"),
                ("%.2f" % (cpu_s / (tot_ios / 1e6))) if cpu_s and tot_ios else "",
                "Y" if min(iops) > 0 and max(iops) / min(iops) > 1.2 else ""])
