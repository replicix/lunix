#!/usr/bin/env python3
"""1 ms heartbeat writer: O_DIRECT 4 KiB overwrites of one preallocated file on the
file system under test. Records every gap between consecutive completions that
exceeds GAP_MS (default 3) as "start_ns,gap_ms" (CLOCK_REALTIME) and writes the
CSV on SIGTERM. Usage: heartbeat.py <file> <out.csv>"""
import mmap, os, signal, sys, time

path, out = sys.argv[1], sys.argv[2]
GAP_MS = float(os.environ.get("GAP_MS", "3"))
fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_DIRECT, 0o600)
os.posix_fallocate(fd, 0, 1 << 20)
buf = mmap.mmap(-1, 4096)
buf.write(b"hb" * 2048)
gaps, errors, stop = [], 0, False
signal.signal(signal.SIGTERM, lambda *_: globals().__setitem__("stop", True))
last = time.time_ns()
n = 0
while not stop:
    try:
        os.pwrite(fd, buf, (n % 256) * 4096)
    except OSError:
        errors += 1
    now = time.time_ns()
    if (now - last) / 1e6 > GAP_MS:
        gaps.append((last, (now - last) / 1e6))
    last = now
    n += 1
    time.sleep(0.001)
with open(out, "w") as f:
    f.write("start_ns,gap_ms\n")
    for s, g in gaps:
        f.write(f"{s},{g:.2f}\n")
    f.write(f"# writes={n} errors={errors}\n")
