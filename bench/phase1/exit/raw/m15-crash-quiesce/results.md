# M15 crash loop MODE=quiesce FS=ext4 chunk=65536 integrity (2026-11-17T16:54:38Z, kernel 7.0.0, 16 cpus)
- phases: handover:20:0; kills every 5-20 s; script cap 3000 s
- PASS create volume crashext4 (32 GiB, chunk 65536, integrity)
- PASS attach crashext4 (4 queues)
- device /dev/ublkb0

## phase handover: 20 cycles, restart delay 0 s
- PASS handover: detach
- PASS handover: lunix fsck --deep clean after detach (exit 0)
- PASS handover: attach again
- PASS handover: 20 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## stall times
| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|
| handover | 20 | 410 | 1333 | 1333 | 4496 | 7949 | 3197 | 5426 | 5426 |
- daemon-logged recovery stall_ms (sorted) in daemon-stall-ms.txt: 20 values
- PASS detach crashext4
- PASS lunix fsck --deep clean after detach (exit 0) [final]
overall FAIL=0
