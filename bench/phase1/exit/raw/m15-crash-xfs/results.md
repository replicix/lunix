# M15 crash loop MODE=kill FS=xfs chunk=65536 integrity (2026-11-17T15:58:42Z, kernel 7.0.0, 16 cpus)
- phases: immediate:25:0 delay1s:12:1 delay10s:5:10 double:8:0; kills every 5-20 s; script cap 6000 s
- PASS create volume crashxfs (32 GiB, chunk 65536, integrity)
- PASS attach crashxfs (4 queues)
- device /dev/ublkb0

## phase immediate: 25 cycles, restart delay 0 s
- PASS immediate: detach
- PASS immediate: lunix fsck --deep clean after detach (exit 0)
- PASS immediate: attach again
- PASS immediate: 25 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase delay1s: 12 cycles, restart delay 1 s
- PASS delay1s: detach
- PASS delay1s: lunix fsck --deep clean after detach (exit 0)
- PASS delay1s: attach again
- PASS delay1s: 12 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase delay10s: 5 cycles, restart delay 10 s
- PASS delay10s: detach
- PASS delay10s: lunix fsck --deep clean after detach (exit 0)
- PASS delay10s: attach again
- PASS delay10s: 5 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## phase double: 8 cycles, restart delay 0 s
- PASS double: detach
- PASS double: lunix fsck --deep clean after detach (exit 0)
- PASS double: attach again
- PASS double: 8 cycles, fio rc 0 errors 0, guest fsck 0, acknowledged fsyncs verify 0, lunix fsck 0

## stall times
| phase | n | daemon exec->recovered ms p50 | p99 | max | kill->ready ms p50 | max | heartbeat stall ms p50 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|
| immediate | 25 | 1946 | 2757 | 2757 | 2052 | 4018 | 0 | 6985 | 6985 |
| delay1s | 12 | 454 | 918 | 918 | 1509 | 2021 | 802 | 1565 | 1565 |
| delay10s | 5 | 509 | 1675 | 1675 | 10559 | 11740 | 1861 | 11736 | 11736 |
| double | 8 | 857 | 1903 | 1903 | 1196 | 2377 | 1713 | 2364 | 2364 |
- daemon-logged recovery stall_ms (sorted) in daemon-stall-ms.txt: 50 values
- PASS detach crashxfs
- PASS lunix fsck --deep clean after detach (exit 0) [final]
overall FAIL=0
