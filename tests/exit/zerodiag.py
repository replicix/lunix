#!/usr/bin/env python3
"""zerodiag.py BADLIST OUT.jsonl: look at the blocks a fio verify pass reported (BADLIST: one `file offset length` per line).

For each block: the FIEMAP extent state of the file (mapped, unwritten, hole) and a direct re-read of the block (all zero, or not).
Prints one summary line; writes one JSON line per block to OUT. A device has no FIEMAP: that field is null.
Used by fio-verify.sh to tell a transient read (the second read is not zero), a persistent zero in a mapped extent (data or file
system state) and a zero in an unwritten extent or a hole (the file system never had the data)."""
import fcntl, json, mmap, os, struct, sys

FS_IOC_FIEMAP = 0xC020660B
UNWRITTEN = 0x800


def fiemap(fd, off):
    # one extent at or after `off`: the extent that holds the block is the one whose logical range contains it
    buf = bytearray(32 + 56)
    struct.pack_into("<QQIII", buf, 0, off, 4096, 0, 0, 1)
    try:
        fcntl.ioctl(fd, FS_IOC_FIEMAP, buf, True)
    except OSError:
        return None
    mapped = struct.unpack_from("<I", buf, 20)[0]
    if mapped == 0:
        return "hole"
    logical, physical, length = struct.unpack_from("<QQQ", buf, 32)
    flags = struct.unpack_from("<I", buf, 32 + 40)[0]
    if not logical <= off < logical + length:
        return "hole"
    return "unwritten" if flags & UNWRITTEN else "mapped"


def reread(fd, off, length):
    n = max(4096, min((length + 4095) // 4096 * 4096, 65536))
    m = mmap.mmap(-1, n)
    try:
        got = os.preadv(fd, [m], off)
    except OSError as e:
        return "error:%d" % e.errno
    return "zero" if not any(m[:got]) else "nonzero"


def main():
    lst, out = sys.argv[1], sys.argv[2]
    counts = {}
    fds = {}
    with open(lst) as f, open(out, "w") as o:
        for line in f:
            p = line.split()
            if len(p) < 3:
                continue
            path, off, length = p[0], int(p[1]), int(p[2])
            if path not in fds:
                try:
                    fds[path] = os.open(path, os.O_RDONLY | os.O_DIRECT)
                except OSError:
                    fds[path] = None
            fd = fds[path]
            if fd is None:
                r, fm = "open-failed", None
            else:
                r, fm = reread(fd, off, length), fiemap(fd, off)
            o.write(json.dumps({"file": path, "off": off, "len": length, "reread": r, "extent": fm}) + "\n")
            for k in (r, "extent:" + str(fm)):
                counts[k] = counts.get(k, 0) + 1
    print("blocks %d: %s" % (sum(v for k, v in counts.items() if not k.startswith("extent:")), " ".join("%s=%d" % kv for kv in sorted(counts.items()))))


main()
