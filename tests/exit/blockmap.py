#!/usr/bin/env python3
"""blockmap.py snap DIR OUT.pkl.gz [GLOB] | cmp A.pkl.gz B.pkl.gz OUT.json

snap: for every file DIR/GLOB (default main.*): the FIEMAP state of every 4 KiB block (0 hole, 1 mapped, 2 unwritten) and the first 8 bytes of
the SHA-256 of every 4 KiB block, read with O_DIRECT. cmp: per file, blocks whose content changed, blocks whose extent state changed
(written->unwritten, unwritten->written, hole), and the number of extents of each snapshot. Exit 0 always; the summary line says what changed."""
import fcntl, glob, gzip, hashlib, json, mmap, os, pickle, struct, sys

FS_IOC_FIEMAP = 0xC020660B
UNWRITTEN, LAST = 0x800, 0x1
BS = 4096


def extents(fd, size):
    out, start = [], 0
    while start < size:
        buf = bytearray(32 + 56 * 512)
        struct.pack_into("<QQIII", buf, 0, start, size - start, 0, 0, 512)
        fcntl.ioctl(fd, FS_IOC_FIEMAP, buf, True)
        n = struct.unpack_from("<I", buf, 20)[0]
        if n == 0:
            break
        last = False
        for k in range(n):
            lo, ph, ln = struct.unpack_from("<QQQ", buf, 32 + 56 * k)
            fl = struct.unpack_from("<I", buf, 32 + 56 * k + 40)[0]
            out.append((lo, ln, 2 if fl & UNWRITTEN else 1))
            last = bool(fl & LAST)
            start = lo + ln
        if last:
            break
    return out


def snap(d, out, pat):
    res = {}
    for p in sorted(glob.glob(os.path.join(d, pat))):
        size = os.path.getsize(p)
        nb = size // BS
        fd = os.open(p, os.O_RDONLY | os.O_DIRECT)
        ex = extents(fd, size)
        st = bytearray(nb)
        for lo, ln, kind in ex:
            for b in range(lo // BS, min(nb, (lo + ln + BS - 1) // BS)):
                st[b] = kind
        hs = bytearray()
        m = mmap.mmap(-1, 1 << 20)
        off = 0
        while off < nb * BS:
            got = os.preadv(fd, [m], off)
            if got <= 0:
                break
            for i in range(0, got - got % BS, BS):
                hs += hashlib.sha256(m[i:i + BS]).digest()[:8]
            off += got
        os.close(fd)
        res[os.path.basename(p)] = {"size": size, "state": bytes(st), "hash": bytes(hs), "extents": len(ex)}
    with gzip.open(out, "wb") as f:
        pickle.dump(res, f)
    print("snap %d files, %d blocks, %d extents" % (len(res), sum(v["size"] // BS for v in res.values()), sum(v["extents"] for v in res.values())))


def cmp(a, b, out):
    A, B = (pickle.load(gzip.open(x, "rb")) for x in (a, b))
    rep, tot = {}, {"content_changed": 0, "written_to_unwritten": 0, "unwritten_to_written": 0, "hole_changes": 0, "extents_pre": 0, "extents_post": 0}
    for f in sorted(A):
        if f not in B:
            rep[f] = "missing after"; continue
        x, y = A[f], B[f]
        nb = min(len(x["state"]), len(y["state"]))
        ch = [i * BS for i in range(nb) if x["hash"][i * 8:i * 8 + 8] != y["hash"][i * 8:i * 8 + 8]]
        w2u = [i * BS for i in range(nb) if x["state"][i] == 1 and y["state"][i] == 2]
        u2w = [i * BS for i in range(nb) if x["state"][i] == 2 and y["state"][i] == 1]
        hl = [i * BS for i in range(nb) if (x["state"][i] == 0) != (y["state"][i] == 0)]
        rep[f] = {"changed_offsets": ch[:50], "w2u": w2u[:50], "u2w": u2w[:50], "hole": hl[:50], "n": [len(ch), len(w2u), len(u2w), len(hl)],
                  "extents": [x["extents"], y["extents"]], "unwritten_blocks_pre": x["state"].count(2), "unwritten_blocks_post": y["state"].count(2)}
        tot["content_changed"] += len(ch); tot["written_to_unwritten"] += len(w2u); tot["unwritten_to_written"] += len(u2w); tot["hole_changes"] += len(hl)
        tot["extents_pre"] += x["extents"]; tot["extents_post"] += y["extents"]
    json.dump({"total": tot, "files": rep}, open(out, "w"))
    print(" ".join("%s=%d" % kv for kv in tot.items()), "unwritten_blocks_pre=%d post=%d" % (
        sum(v["unwritten_blocks_pre"] for v in rep.values() if isinstance(v, dict)), sum(v["unwritten_blocks_post"] for v in rep.values() if isinstance(v, dict))))


if sys.argv[1] == "snap":
    snap(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) > 4 else "main.*")
else:
    cmp(sys.argv[2], sys.argv[3], sys.argv[4])
