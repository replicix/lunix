#!/usr/bin/env python3
"""Durability checker for acknowledged fsync()s.
  fsyncer.py write  <file> <ackfile>   loop: pwrite 4 KiB block i (content derived from i), fsync,
                                       then record i in <ackfile> (kept on a different file system).
                                       Runs until SIGTERM; exits 2 on any I/O error.
  fsyncer.py verify <file> <ackfile>   every block from max(0, A-RING+1) to the acknowledged index A
                                       must hold its pattern. Exit 0 = ok, 1 = mismatch.
The file is a ring of RING blocks so it stays small."""
import hashlib, os, signal, sys

RING = 16384
mode, path, ack = sys.argv[1:4]

def block(i):
    h = hashlib.sha256(i.to_bytes(8, "little")).digest()
    return i.to_bytes(8, "little") + (h * 128)[:4088]

if mode == "write":
    stop = False
    signal.signal(signal.SIGTERM, lambda *_: globals().__setitem__("stop", True))
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    afd = os.open(ack, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o600)
    i = 0
    try:
        while not stop:
            os.pwrite(fd, block(i), (i % RING) * 4096)
            os.fsync(fd)
            os.pwrite(afd, b"%020d\n" % i, 0)
            i += 1
    except OSError as e:
        print(f"fsyncer: I/O error at block {i}: {e}", file=sys.stderr)
        sys.exit(2)
    print(f"fsyncer: wrote {i} blocks")
else:
    last = int(open(ack).read().strip() or -1)
    fd = os.open(path, os.O_RDONLY)
    bad = 0
    for j in range(max(0, last - RING + 1), last + 1):
        if os.pread(fd, 4096, (j % RING) * 4096) != block(j):
            bad += 1
    print(f"fsyncer verify: acked={last} bad_blocks={bad}")
    sys.exit(1 if bad else 0)
