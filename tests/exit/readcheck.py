#!/usr/bin/env python3
"""Per-block read check of a device against a list of hashes (the tamper cell).

  readcheck.py make  DEV LENGTH REF.sha        write the SHA-256 of every 1 MiB block of the first LENGTH bytes
  readcheck.py check DEV LENGTH REF.sha        read every block again (O_DIRECT) and compare: prints JSON
                                               {"blocks", "errors", "mismatches", "first_error", "first_mismatch"}

A block that cannot be read (EIO) is an `error`; a block that reads but differs from the reference is a `mismatch`, which must
never happen on a volume whose stored data was tampered with: the daemon has to refuse the damaged data, not return other bytes.
Exit status 0 for `make`; for `check` 0 when there is no mismatch (errors are expected after tampering), 1 otherwise.
"""
import hashlib, json, mmap, os, sys

mode, dev, length, ref = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
BLK = 1 << 20
fd = os.open(dev, os.O_RDONLY | os.O_DIRECT)
buf = mmap.mmap(-1, BLK)
hashes = open(ref).read().split() if mode == "check" else []
out = []
errors = mism = 0
first_err = first_mis = None
for i in range(length // BLK):
    try:
        n = os.preadv(fd, [buf], i * BLK)
        h = hashlib.sha256(buf[:n]).hexdigest() if n == BLK else "short"
    except OSError:
        h = None
    if mode == "make":
        out.append(h or "unreadable")
    elif h is None:
        errors += 1
        first_err = i if first_err is None else first_err
    elif h != hashes[i]:
        mism += 1
        first_mis = i if first_mis is None else first_mis
if mode == "make":
    open(ref, "w").write("\n".join(out) + "\n")
else:
    print(json.dumps({"blocks": length // BLK, "errors": errors, "mismatches": mism, "first_error": first_err, "first_mismatch": first_mis}))
    sys.exit(0 if mism == 0 else 1)
