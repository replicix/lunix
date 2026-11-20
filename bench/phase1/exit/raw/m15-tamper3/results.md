# M15 tamper cell (2026-11-20T01:17:48Z): variants segment commit map, chunks 65536 16384

## tamper/c64k/segment
- PASS create volume tamsegment64 (8 GiB, chunk 65536, integrity)
- PASS attach tamsegment64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/segment: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/segment: reference hashes of 2 GiB taken
- PASS tamper/c64k/segment: detach
- PASS tamper/c64k/segment: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 8401941 of segments/000000000000009f.seg (16803882 bytes)
- PASS tamper/c64k/segment: fsck --deep reports an error after the flip (exit 2: extent_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 1, "mismatches": 0, "first_error": 1678, "first_mismatch": null}
- PASS tamper/c64k/segment: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/segment: the damage was refused (attach refused: no, read errors: 1)

## tamper/c64k/commit
- PASS create volume tamcommit64 (8 GiB, chunk 65536, integrity)
- PASS attach tamcommit64 (4 queues)
- device /dev/ublkb0
