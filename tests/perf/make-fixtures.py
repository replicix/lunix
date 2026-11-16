#!/usr/bin/env python3
"""Synthetic result directories for the self-test of the two analysers (invented numbers, shaped like the real ones).

  make-fixtures.py OUT_PARTA OUT_CHUNK
"""
import json
import os
import random
import sys

random.seed(3)
pa, ck = sys.argv[1], sys.argv[2]
os.makedirs(os.path.join(pa, "raw"), exist_ok=True)
rows = []
for chunk_k, scale in ((64, 1.0), (16, 0.9)):
    for q in (4, 8):
        rows.append({"cell": f"partA/c{chunk_k}k/q{q}/idle", "status": "info", "chunk_k": str(chunk_k), "queues": str(q), "rss_kib": str(600000 + q * 40000)})
        for name, base in (("r4k", 620000), ("w4k", 280000), ("seqw", 3.4e9), ("seqr", 5.6e9)):
            for rep in (1, 2, 3):
                v = base * scale * (0.9 + 0.1 * q / 8) * random.uniform(0.97, 1.03)
                iops = v if name in ("r4k", "w4k") else v / 1048576
                bw = v if name in ("seqw", "seqr") else v * 4096
                rows.append({"cell": f"partA/c{chunk_k}k/q{q}/{name}/rep{rep}", "status": "info", "chunk_k": str(chunk_k), "queues": str(q), "cell_name": name, "rep": str(rep),
                             "iops": f"{iops:.0f}", "bw_bytes": f"{bw:.0f}", "p99_us": f"{random.uniform(90, 140):.1f}", "cpu_us_per_io": f"{random.uniform(5, 9):.2f}",
                             "rss_kib_after": "900000", "hwm_kib": "950000", "journal_used_end": "123456", "journal_used_idle10": "1000", "window_s": "29",
                             "thread_split": "q-0=20.1(4) jrnl=5.0(1)", "noisy_host": "0"})
        rows.append({"cell": f"partA/c{chunk_k}k/q{q}/r4k-verifyreads/rep1", "status": "info", "chunk_k": str(chunk_k), "queues": str(q), "cell_name": "r4k-verifyreads", "rep": "1",
                     "iops": f"{400000 * scale:.0f}", "bw_bytes": "1", "p99_us": "180"})
open(os.path.join(pa, "results.jsonl"), "w").write("\n".join(json.dumps(r) for r in rows) + "\n")
os.makedirs(ck + "/raw", exist_ok=True)
rows = []
for chunk, f_store in ((65536, 1.0), (16384, 0.8)):
    for w, fs in (("pgbench", "ext4"), ("pgbench", "xfs"), ("vm", "ext4"), ("randmix", "ext4")):
        d = f"{ck}/raw/{w}-{chunk // 1024}-{fs}"
        os.makedirs(d, exist_ok=True)
        bw = 30 * 2 ** 30
        json.dump({"stats": {"bytes_written": 0, "ops_write": 0, "cold_rmw": 0}}, open(d + "/status-0.json", "w"))
        json.dump({"stats": {"bytes_written": bw, "ops_write": 2_000_000, "cold_rmw": int(110000 * f_store)}, "space": {"map": int(2e8 / f_store)}}, open(d + "/status-1.json", "w"))
        json.dump({"ratios": {"chunk_size": chunk, "mapped_chunks": 1_000_000, "dedup_ratio": 1.4, "compression_ratio": 2.1, "unique_clen": int(12e9 * f_store),
                              "bytes": {"segments": int(14e9 * f_store), "map_packs": int(1.5e8 / f_store), "commits": 1e6, "index": 5e7}}}, open(d + "/fsck.json", "w"))
        if w == "pgbench":
            open(d + "/pgbench-run.txt", "w").write("tps = 4321.5 (without initial connection time)\n")
            json.dump({"p50_ms": 3.1, "p99_ms": 18.2}, open(d + "/pglat.json", "w"))
            open(d + "/pg-version.txt", "w").write("postgres (PostgreSQL) 18.6")
        rows.append({"cell": f"chunk/{w}/c{chunk // 1024}k/{fs}/rep1", "status": "info", "workload": w, "chunk": str(chunk), "fs": fs, "rep": "1", "dir": "/x/" + os.path.basename(d),
                     "daemon_cpu_s": f"{40 * (1.1 if chunk == 16384 else 1.0):.1f}", "workload_secs": "1800", "fsck_rc": "0"})
open(ck + "/results.jsonl", "w").write("\n".join(json.dumps(r) for r in rows) + "\n")
