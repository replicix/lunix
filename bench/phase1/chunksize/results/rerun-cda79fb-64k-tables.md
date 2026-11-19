# M16 Part B: chunk size 16 KiB against 64 KiB


## randmix

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 64 | 1 (once: noise caveat) | 29.4 | 1.000 | 1.993 | 10.17 | 0.080 | 4.04 | 2920188.0563 | 0.0429 | 17.88 | 0.3490 | 11.03 | 869.1 | 1742 | 16 | 38.54 | n/a | n/a |

## seqrand

| fs | chunk KiB | runs | logical GiB written | dedup | compression | live data GiB | map GiB | map GiB per TiB mapped | map bytes per GiB written | map seal WA (map bytes written / bytes written) | store after GC GiB | stored (data+map) per logical GiB | daemon CPU s per GiB | cold RMW per 1000 writes | IOPS | MB/s | p99 ms | TPS | pg p99 ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ext4 | 64 | 1 (once: noise caveat) | 28.9 | 1.000 | 1.978 | 8.09 | 0.062 | 4.00 | 2320793.0680 | 0.0173 | 15.46 | 0.2822 | 7.19 | 994.7 | 1594 | 15 | 49.55 | n/a | n/a |

## Advisory rule (ADR-5, fixed in advance)

Not evaluable: the VM-image and pgbench cells of both chunk sizes are not all present.

