# P0.12 RAM-sink latency and replay

## Acknowledgement latency (4k random write on the raw device, no FLUSH issued)
- sink=file QD1 (io_size 8m): 243 IOPS, lat p50 3358.72 us, p99 13697.024 us, p99.9 16908.288 us, mean 4106 us
- sink=file QD32 (io_size 64m): 4294 IOPS, lat p50 6651.904 us, p99 16580.608 us, p99.9 33816.576 us, mean 7450 us
- sink=mem QD1 (io_size 64m): 210051 IOPS, lat p50 3.696 us, p99 11.712 us, p99.9 154.624 us, mean 4 us
- sink=mem QD32 (io_size 64m): 712347 IOPS, lat p50 42.24 us, p99 51.968 us, p99.9 514.048 us, mean 43 us

## Replay of a 256 MiB buffer after kill -9 (no FLUSH before the kill)
- bs=64k: REPLAY pending_bytes=262272000 applied_bytes=262144000 ms=3755.9 generation=1 ; exec->recovered 3894 ms
