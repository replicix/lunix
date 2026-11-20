# M15 tamper cell (2026-11-20T00:34:54Z): variants map, chunks 65536 16384

## tamper/c64k/map
- PASS create volume tammap64 (8 GiB, chunk 65536, integrity)
- PASS attach tammap64 (4 queues)
- device /dev/ublkb0
- PASS tamper/c64k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c64k/map: reference hashes of 2 GiB taken
- PASS tamper/c64k/map: detach
- PASS tamper/c64k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 17191 of map/00000000000000c4 (34382 bytes)
- PASS tamper/c64k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 2048, "mismatches": 0, "first_error": 0, "first_mismatch": null}
- PASS tamper/c64k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c64k/map: the damage was refused (attach refused: no, read errors: 2048)

## tamper/c16k/map
- PASS create volume tammap16 (8 GiB, chunk 16384, integrity)
- PASS attach tammap16 (4 queues)
- device /dev/ublkb0
- PASS tamper/c16k/map: the write finished across the kill and the recovery (fio rc 0)
- PASS tamper/c16k/map: reference hashes of 2 GiB taken
- PASS tamper/c16k/map: detach
- PASS tamper/c16k/map: fsck --deep clean before the tampering (exit 0)
- flipping one byte at offset 43543 of map/00000000000000c5 (87086 bytes)
- PASS tamper/c16k/map: fsck --deep reports an error after the flip (exit 2: extent_auth,map_node_auth,seg_hash)
- attached; read check: {"blocks": 2048, "errors": 2048, "mismatches": 0, "first_error": 0, "first_mismatch": null}
- PASS tamper/c16k/map: no block returned other bytes (mismatches 0)
- PASS tamper/c16k/map: the damage was refused (attach refused: no, read errors: 2048)

overall FAIL=0
