#!/usr/bin/env python3
"""SQLite WAL, synchronous=FULL, one INSERT per transaction, paced. usage: sqlite_wal.py db secs rate"""
import os, sqlite3, sys, time
db, secs, rate = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
c = sqlite3.connect(db, isolation_level=None)
c.execute("PRAGMA journal_mode=WAL"); c.execute("PRAGMA synchronous=FULL")
c.execute("CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, v BLOB)")
t0 = time.monotonic(); end = t0 + secs; n = 0; nxt = t0
while nxt < end:
    d = nxt - time.monotonic()
    if d > 0: time.sleep(d)
    c.execute("INSERT INTO t(v) VALUES (zeroblob(200))"); n += 1; nxt += 1.0 / rate
print("sqlite ops=%d ops_per_s=%.1f" % (n, n / secs), flush=True)
os._exit(0)  # no close: keeps the -wal file so it can be mapped
