# M15 tamper cell (2026-11-20T00:49:48Z): variants segment commit map, chunks 65536 16384

## tamper/c64k/segment
- PASS create volume tamsegment64 (8 GiB, chunk 65536, integrity)
- PASS attach tamsegment64 (4 queues)
- device /dev/ublkb0
- FAIL tamper/c64k/segment: the write finished across the kill and the recovery (fio rc 1)

## tamper/c64k/commit
- PASS create volume tamcommit64 (8 GiB, chunk 65536, integrity)
- PASS attach tamcommit64 (4 queues)
- device /dev/ublkb1
- PASS tamper/c64k/commit: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/commit: reference hashes of 2 GiB taken
- PASS tamper/c64k/commit: detach
- PASS tamper/c64k/commit: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 146 of commits/0000000000000020 (292 bytes)
- PASS tamper/c64k/commit: fsck --deep reports an error after the flip (exit 2: tip_auth)
- attach refused (rc 1): lunix: io: tip commit 0000000000000020 does not authenticate: authentication failed: metadata object 
- PASS tamper/c64k/commit: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/commit: the damage was refused (attach refused: yes, read errors: 0)

## tamper/c64k/map
- PASS create volume tammap64 (8 GiB, chunk 65536, integrity)
- PASS attach tammap64 (4 queues)
- device /dev/ublkb1
- PASS tamper/c64k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/map: reference hashes of 2 GiB taken
- PASS tamper/c64k/map: detach
- PASS tamper/c64k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 17190 of map/00000000000000c4 (34381 bytes)
- PASS tamper/c64k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 2048, "mismatches": 0, "first_error": 0, "first_mismatch": null}
- PASS tamper/c64k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/map: the damage was refused (attach refused: no, read errors: 2048)

## tamper/c16k/segment
- PASS create volume tamsegment16 (8 GiB, chunk 16384, integrity)
- PASS attach tamsegment16 (4 queues)
- device /dev/ublkb1
- PASS tamper/c16k/segment: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/segment: reference hashes of 2 GiB taken
- PASS tamper/c16k/segment: detach
- PASS tamper/c16k/segment: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 8408544 of segments/0000000000000091.seg (16817089 bytes)
- PASS tamper/c16k/segment: fsck --deep reports an error after the flip (exit 2: extent_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 1, "mismatches": 0, "first_error": 1488, "first_mismatch": null}
- PASS tamper/c16k/segment: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/segment: the damage was refused (attach refused: no, read errors: 1)

## tamper/c16k/commit
- PASS create volume tamcommit16 (8 GiB, chunk 16384, integrity)
- PASS attach tamcommit16 (4 queues)
- device /dev/ublkb1
- FAIL tamper/c16k/commit: the write finished across the kill and the recovery (fio rc 1)

## tamper/c16k/map
- PASS create volume tammap16 (8 GiB, chunk 16384, integrity)
- PASS attach tammap16 (4 queues)
- device /dev/ublkb2
- PASS tamper/c16k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/map: reference hashes of 2 GiB taken
- PASS tamper/c16k/map: detach
- PASS tamper/c16k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 43543 of map/00000000000000c7 (87086 bytes)
- PASS tamper/c16k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 2048, "mismatches": 0, "first_error": 0, "first_mismatch": null}
- PASS tamper/c16k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/map: the damage was refused (attach refused: no, read errors: 2048)

overall FAIL=1
