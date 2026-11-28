#!/usr/bin/env python3
"""Discard and write-zeroes checker (M15 item 1: "trim and write_zeroes mixed in").

  trimcheck.py raw  DEV OFFSET LENGTH SECONDS OUT.json [SEED]
  trimcheck.py file DIR LENGTH       SECONDS OUT.json [SEED]

Each round writes random data over a window (4 KiB .. 4 MiB, 4 KiB aligned, inside the region), then discards or
zeroes a random 4 KiB aligned sub-range of it: BLKDISCARD / BLKZEROOUT on a block device (what `blkdiscard` and
`blkdiscard -z` do), fallocate(PUNCH_HOLE) / fallocate(ZERO_RANGE) on a file. The window is read back with
O_DIRECT: the sub-range must read as zeros and every other byte must still hold what was written. Sub-ranges start
and end inside chunks, so the partial-chunk paths of discard and write-zeroes are exercised at both chunk sizes.
`--discard-zeroes 0` (environment DISCARD_ZEROES=0) skips the zero check for plain discards (a device that does not
promise zeros); write-zeroes is always checked.

Stops at the deadline or on SIGTERM; exit 0 when no mismatch was found, 1 on a mismatch (the first one is
recorded), 2 on an I/O error. Output: JSON with counters. Only O_DIRECT I/O is used, so the page cache cannot
hide a stale read.
"""
import ctypes
import fcntl
import json
import mmap
import os
import random
import signal
import struct
import sys
import time

BLKDISCARD = 0x1277
BLKZEROOUT = 0x127F
FALLOC_FL_KEEP_SIZE = 0x01
FALLOC_FL_PUNCH_HOLE = 0x02
FALLOC_FL_ZERO_RANGE = 0x10
BLK = 4096
MAX_WINDOW = 4 << 20

stop = False


def _term(*_):
    global stop
    stop = True


def buf(n):
    """An anonymous (page aligned) buffer, as O_DIRECT needs."""
    return mmap.mmap(-1, n)


def pwrite_all(fd, data, off):
    m = buf(len(data))
    m.write(data)
    done = 0
    while done < len(data):
        n = os.pwrite(fd, memoryview(m)[done:], off + done)
        if n <= 0:
            raise OSError("short write")
        done += n


def pread_all(fd, n, off):
    m = buf(n)
    done = 0
    while done < n:
        r = os.preadv(fd, [memoryview(m)[done:]], off + done)
        if r <= 0:
            raise OSError("short read")
        done += r
    return bytes(m)


class Target:
    def __init__(self, kind, path, offset, length):
        self.kind, self.offset, self.length = kind, offset, length
        if kind == "raw":
            self.fd = os.open(path, os.O_RDWR | os.O_DIRECT)
        else:
            self.path = os.path.join(path, "trimcheck.dat")
            self.fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_DIRECT, 0o600)
            os.posix_fallocate(self.fd, 0, length)
            os.fsync(self.fd)
        self.libc = ctypes.CDLL(None, use_errno=True)
        self.libc.fallocate.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_long, ctypes.c_long]

    def _falloc(self, mode, off, n):
        if self.libc.fallocate(self.fd, mode, off, n) != 0:
            e = ctypes.get_errno()
            raise OSError(e, os.strerror(e))

    def discard(self, off, n):
        if self.kind == "raw":
            fcntl.ioctl(self.fd, BLKDISCARD, struct.pack("QQ", self.offset + off, n))
        else:
            self._falloc(FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, off, n)

    def zeroout(self, off, n):
        if self.kind == "raw":
            fcntl.ioctl(self.fd, BLKZEROOUT, struct.pack("QQ", self.offset + off, n))
        else:
            self._falloc(FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE, off, n)

    def write(self, off, data):
        pwrite_all(self.fd, data, self.offset + off)

    def read(self, off, n):
        return pread_all(self.fd, n, self.offset + off)

    def flush(self):
        os.fsync(self.fd)


def main():
    if len(sys.argv) < 6:
        print(__doc__)
        return 64
    kind = sys.argv[1]
    if kind == "raw":
        path, offset, length, secs, out = sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), float(sys.argv[5]), sys.argv[6]
        seed = int(sys.argv[7]) if len(sys.argv) > 7 else 1
    elif kind == "file":
        path, offset, length, secs, out = sys.argv[2], 0, int(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
        seed = int(sys.argv[6]) if len(sys.argv) > 6 else 1
    else:
        print(__doc__)
        return 64
    assert offset % BLK == 0 and length % BLK == 0 and length >= 2 * MAX_WINDOW
    discard_zeroes = os.environ.get("DISCARD_ZEROES", "1") != "0"
    signal.signal(signal.SIGTERM, _term)
    signal.signal(signal.SIGINT, _term)
    rng = random.Random(seed)
    t = Target(kind, path, offset, length)
    stats = {"rounds": 0, "discards": 0, "zeroouts": 0, "bytes_checked": 0, "mismatches": 0, "first_mismatch": None,
             "error": None}
    deadline = time.time() + secs
    rc = 0
    try:
        while not stop and time.time() < deadline:
            wn = BLK * rng.choice([1, 2, 3, 4, 8, 16, 17, 32, 33, 64, 128, 256, 1024])
            wn = min(wn, MAX_WINDOW)
            wo = BLK * rng.randrange(0, (length - wn) // BLK + 1)
            data = bytearray(os.urandom(wn))
            t.write(wo, bytes(data))
            t.flush()
            # a sub-range of whole 4 KiB blocks inside the window (it starts and ends inside chunks)
            nblk = wn // BLK
            s = rng.randrange(0, nblk)
            e = rng.randrange(s + 1, nblk + 1)
            so, sn = s * BLK, (e - s) * BLK
            zero_op = rng.random() < 0.5
            if zero_op:
                t.zeroout(wo + so, sn)
                stats["zeroouts"] += 1
            else:
                t.discard(wo + so, sn)
                stats["discards"] += 1
            check_zero = zero_op or discard_zeroes
            if check_zero:
                data[so:so + sn] = bytes(sn)
            got = t.read(wo, wn)
            stats["rounds"] += 1
            stats["bytes_checked"] += wn
            if check_zero:
                bad = got != bytes(data)
            else:
                bad = got[:so] != bytes(data[:so]) or got[so + sn:] != bytes(data[so + sn:])
            if bad:
                stats["mismatches"] += 1
                if stats["first_mismatch"] is None:
                    first = next((i for i in range(wn) if got[i] != data[i]), -1) if check_zero else -1
                    stats["first_mismatch"] = {"op": "zeroout" if zero_op else "discard", "window_off": wo,
                                               "window_len": wn, "sub_off": so, "sub_len": sn, "first_bad_byte": first}
                rc = 1
                break
    except OSError as ex:
        stats["error"] = f"{type(ex).__name__}: {ex}"
        rc = 2
    stats["kind"] = kind
    stats["exit"] = rc
    with open(out, "w") as f:
        json.dump(stats, f)
        f.write("\n")
    return rc


if __name__ == "__main__":
    sys.exit(main())
