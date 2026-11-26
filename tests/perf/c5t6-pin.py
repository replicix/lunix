#!/usr/bin/env python3
"""Writes a snapshot pin (`snapshots/<name>` of a volume directory) on a commit of a PLAIN-mode volume: a form 0 metadata envelope
(kind 10) whose MAC is BLAKE3-256 over `meta_aad || envelope[0 .. 12 + body_len]` (lunix-crypto meta.rs, lunix-format envelope.rs,
pins.rs; little-endian). Plain mode needs no key, so no product code is involved.
  c5t6-pin.py VOLDIR VOLUME_UUID COMMIT_SEQ NAME
"""
import os
import struct
import sys
import uuid

import blake3

vd, vol, seq, name = sys.argv[1], uuid.UUID(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
label = b"c5t6"
body = struct.pack("<BBH4xQQ", 1, 1, len(label), seq, 1) + label       # BODY_VER 1, Consistency::Crash 1, label_len, pad, seq, created
kind = 10                                                               # ObjectKind::Snapshots
obj = b"LXM1" + struct.pack("<BBBBI", 1, kind, 0, 0, len(body)) + body  # header (12 bytes) + body
oname = ("snapshots/" + name).encode()
aad = b"lunix meta v1\0" + vol.bytes + bytes([kind]) + struct.pack("<I", len(oname)) + oname
obj += blake3.blake3(aad + obj).digest()
os.makedirs(os.path.join(vd, "snapshots"), exist_ok=True)
with open(os.path.join(vd, "snapshots", name), "wb") as f:
    f.write(obj)
print("pin", name, "on commit", seq, len(obj), "bytes")
