# M15 crash loop MODE=kill FS=ext4 chunk=65536 integrity (2026-11-17T14:33:44Z, kernel 7.0.0, 16 cpus)
- phases: immediate:50:0 delay1s:25:1 delay10s:10:10 double:15:0; kills every 5-20 s; script cap 8000 s
- PASS create volume crashext4 (32 GiB, chunk 65536, integrity)
- PASS attach crashext4 (4 queues)
- device /dev/ublkb0

## phase immediate: 50 cycles, restart delay 0 s
- PASS immediate: detach
- PASS immediate: lunix fsck --deep clean after detach (exit 0)
- PASS immediate: attach again
- PASS immediate: 50 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase delay1s: 25 cycles, restart delay 1 s
- PASS delay1s: detach
- PASS delay1s: lunix fsck --deep clean after detach (exit 0)
- PASS delay1s: attach again
- PASS delay1s: 25 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase delay10s: 10 cycles, restart delay 10 s
- PASS delay10s: detach
- PASS delay10s: lunix fsck --deep clean after detach (exit 0)
- PASS delay10s: attach again
- PASS delay10s: 10 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase double: 15 cycles, restart delay 0 s
- PASS double: detach
- PASS double: lunix fsck --deep clean after detach (exit 0)
- PASS double: attach again
- PASS double: 15 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## stall times
| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|
| immediate | 50 | 478 | 3009 | 3009 | 536 | 3098 | 0 | 3466 | 3466 |
| delay1s | 25 | 896 | 3739 | 3739 | 1952 | 4798 | 0 | 2452 | 2452 |
| delay10s | 10 | 1255 | 1455 | 1455 | 11315 | 11515 | 0 | 11501 | 11501 |
| double | 15 | 1571 | 5359 | 5359 | 1890 | 5652 | 1759 | 5749 | 5749 |
- daemon-logged recovery stall_ms (sorted) in daemon-stall-ms.txt: 100 values
- PASS detach crashext4
- PASS lunix fsck --deep clean after detach (exit 0) [final]
overall FAIL=0
