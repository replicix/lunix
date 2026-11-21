# M15 tamper cell (2026-11-20T06:41:59Z): variants segment commit map, chunks 65536 16384

## tamper/c64k/segment
- PASS create volume tamsegment64 (8 GiB, chunk 65536, integrity)
- PASS attach tamsegment64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/segment: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/segment: reference hashes of 2 GiB taken
- PASS tamper/c64k/segment: detach
- PASS tamper/c64k/segment: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 8401956 of segments/000000000000008b.seg (16803913 bytes)
- PASS tamper/c64k/segment: fsck --deep reports an error after the flip (exit 2: extent_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 1, "mismatches": 0, "first_error": 1631, "first_mismatch": null}
- PASS tamper/c64k/segment: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/segment: the damage was refused (attach refused: no, read errors: 1)

## tamper/c64k/commit
- PASS create volume tamcommit64 (8 GiB, chunk 65536, integrity)
- PASS attach tamcommit64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/commit: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/commit: reference hashes of 2 GiB taken
- PASS tamper/c64k/commit: detach
- PASS tamper/c64k/commit: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 154 of commits/0000000000000016 (308 bytes)
- PASS tamper/c64k/commit: fsck --deep reports an error after the flip (exit 2: tip_auth)
- attach refused (rc 1): lunix: io: tip commit 0000000000000016 does not authenticate: authentication failed: metadata object 
- PASS tamper/c64k/commit: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/commit: the damage was refused (attach refused: yes, read errors: 0)

## tamper/c64k/map
- PASS create volume tammap64 (8 GiB, chunk 65536, integrity)
- PASS attach tammap64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/map: reference hashes of 2 GiB taken
- PASS tamper/c64k/map: detach
- PASS tamper/c64k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 26727 of map/00000000000000b0 (53455 bytes)
- PASS tamper/c64k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 32, "mismatches": 0, "first_error": 2016, "first_mismatch": null}
- PASS tamper/c64k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/map: the damage was refused (attach refused: no, read errors: 32)

## tamper/c16k/segment
- PASS create volume tamsegment16 (8 GiB, chunk 16384, integrity)
- PASS attach tamsegment16 (4 queues)
- device /dev/ublkb0
- PASS tamper/c16k/segment: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/segment: reference hashes of 2 GiB taken
- PASS tamper/c16k/segment: detach
- PASS tamper/c16k/segment: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 8408611 of segments/0000000000000061.seg (16817222 bytes)
- PASS tamper/c16k/segment: fsck --deep reports an error after the flip (exit 2: extent_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 1, "mismatches": 0, "first_error": 1078, "first_mismatch": null}
- PASS tamper/c16k/segment: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/segment: the damage was refused (attach refused: no, read errors: 1)

## tamper/c16k/commit
- PASS create volume tamcommit16 (8 GiB, chunk 16384, integrity)
- PASS attach tamcommit16 (4 queues)
- device /dev/ublkb0
- PASS tamper/c16k/commit: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/commit: reference hashes of 2 GiB taken
- PASS tamper/c16k/commit: detach
- PASS tamper/c16k/commit: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 142 of commits/0000000000000016 (284 bytes)
- PASS tamper/c16k/commit: fsck --deep reports an error after the flip (exit 2: tip_auth)
- attach refused (rc 1): lunix: io: tip commit 0000000000000016 does not authenticate: authentication failed: metadata object 
- PASS tamper/c16k/commit: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/commit: the damage was refused (attach refused: yes, read errors: 0)

## tamper/c16k/map
- PASS create volume tammap16 (8 GiB, chunk 16384, integrity)
- PASS attach tammap16 (4 queues)
- device /dev/ublkb0
- PASS tamper/c16k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/map: reference hashes of 2 GiB taken
- PASS tamper/c16k/map: detach
- PASS tamper/c16k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 43541 of map/00000000000000b7 (87082 bytes)
- PASS tamper/c16k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 2048, "mismatches": 0, "first_error": 0, "first_mismatch": null}
- PASS tamper/c16k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/map: the damage was refused (attach refused: no, read errors: 2048)

overall FAIL=0
