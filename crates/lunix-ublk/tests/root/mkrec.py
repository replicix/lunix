#!/usr/bin/env python3
"""Rewrites fields of a `state/device` record (96 bytes) and fixes its crc32c.
  mkrec.py <in> <out> [boot=flip] [queues=N] [depth=N] [sectors=+N] [flags=xorN] [dev_id=N]"""
import struct, sys

def crc32c(data):
    crc = 0xFFFFFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 & -(crc & 1))
    return crc ^ 0xFFFFFFFF

src, dst = sys.argv[1:3]
rec = bytearray(open(src, "rb").read())
assert len(rec) == 96 and rec[:8] == b"LUNIXDEV", "not a device record"
for a in sys.argv[3:]:
    k, v = a.split("=", 1)
    if k == "boot":
        rec[24:40] = bytes(b ^ 0xFF for b in rec[24:40])
    elif k == "queues":
        struct.pack_into("<H", rec, 10, int(v))
    elif k == "depth":
        struct.pack_into("<H", rec, 12, int(v))
    elif k == "dev_id":
        struct.pack_into("<I", rec, 16, int(v))
    elif k == "sectors":
        (cur,) = struct.unpack_from("<Q", rec, 56)
        struct.pack_into("<Q", rec, 56, cur + int(v))
    elif k == "flags":
        (cur,) = struct.unpack_from("<Q", rec, 64)
        struct.pack_into("<Q", rec, 64, cur ^ int(v, 0))
    else:
        raise SystemExit(f"unknown field {k}")
struct.pack_into("<I", rec, 92, crc32c(bytes(rec[:92])))
open(dst, "wb").write(rec)
