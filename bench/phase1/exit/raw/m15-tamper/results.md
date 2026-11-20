# M15 tamper cell (2026-11-19T23:25:51Z): variants segment commit map, chunks 65536 16384

## tamper/c64k/segment
- PASS create volume tamsegment64 (8 GiB, chunk 65536, integrity)
- PASS attach tamsegment64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/segment: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/segment: reference hashes of 2 GiB taken
- PASS tamper/c64k/segment: detach
- PASS tamper/c64k/segment: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 8401933 of segments/0000000000000013.seg (16803867 bytes)
- PASS tamper/c64k/segment: fsck --deep reports an error after the flip (exit 2: extent_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 1, "mismatches": 0, "first_error": 229, "first_mismatch": null}
- PASS tamper/c64k/segment: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/segment: the damage was refused (attach refused: no, read errors: 1)

## tamper/c64k/commit
- PASS create volume tamcommit64 (8 GiB, chunk 65536, integrity)
- PASS attach tamcommit64 (4 queues)
- device /dev/ublkb0
- FAIL tamper/c64k/commit: the write finished across the kill and the recovery (fio rc 1)

## tamper/c64k/map
- PASS create volume tammap64 (8 GiB, chunk 65536, integrity)
- PASS attach tammap64 (4 queues)
- device /dev/ublkb1
- FAIL tamper/c64k/map: the write finished across the kill and the recovery (fio rc 1)

## tamper/c16k/segment
- PASS create volume tamsegment16 (8 GiB, chunk 16384, integrity)
- PASS attach tamsegment16 (4 queues)
- device /dev/ublkb2
- FAIL tamper/c16k/segment: the write finished across the kill and the recovery (fio rc 1)

## tamper/c16k/commit
- PASS create volume tamcommit16 (8 GiB, chunk 16384, integrity)
- PASS attach tamcommit16 (4 queues)
- device /dev/ublkb3
- PASS tamper/c16k/commit: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/commit: reference hashes of 2 GiB taken
- PASS tamper/c16k/commit: detach
- PASS tamper/c16k/commit: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 150 of commits/0000000000000020 (300 bytes)
- PASS tamper/c16k/commit: fsck --deep reports an error after the flip (exit 2: tip_auth)
- attach refused (rc 1): lunix: io: tip commit 0000000000000020 does not authenticate: authentication failed: metadata object 
- PASS tamper/c16k/commit: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/commit: the damage was refused (attach refused: yes, read errors: 0)

## tamper/c16k/map
- PASS create volume tammap16 (8 GiB, chunk 16384, integrity)
- PASS attach tammap16 (4 queues)
- device /dev/ublkb3
- FAIL tamper/c16k/map: the write finished across the kill and the recovery (fio rc 1)

overall FAIL=1
