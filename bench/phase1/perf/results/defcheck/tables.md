# defcheck tables

## Per block

| block | arm | start load 1/5/15 | rep 1: onset s | first delete s | stops | longest wait s | throttled s | GiB <= 5 s | IOPS | p99.9 s |
|---|---|---|---|---|---|---|---|---|---|---|
| b01-D-65536-w4k | D | 1.34/0.63/0.24 | 13.5 | 8.78 | 0 | 5.4 | 9.8 | 0.00 | 60,120 | 0.67 |
| b02-C2-65536-w4k | C2 | 116.95/254.67/138.97 | 13.2 | 0.09 | 0 | 1.0 | 3.4 | 12.23 | 69,381 | 0.60 |
| b03-O-65536-w4k | O | 117.77/315.45/226.57 | 13.0 | 35.79 | 5 | 34.2 | 45.1 | 0.00 | 19,996 | 6.81 |
| b04-O-65536-w4k | O | 114.36/328.12/281.06 | 17.7 | 31.37 | 4 | 38.5 | 40.2 | 0.00 | 19,209 | 6.88 |
| b05-C2-65536-w4k | C2 | 105.47/325.71/313.83 | 15.7 | 0.08 | 0 | 1.2 | 2.6 | 12.34 | 66,372 | 0.62 |
| b06-D-65536-w4k | D | 115.81/330.73/336.84 | 16.2 | 5.87 | 1 | 3.6 | 7.9 | 0.00 | 60,876 | 0.62 |

| block | arm | reps 2-5 IOPS (med) | p99.9 s (med) | longest wait s (med / worst rep) | stops (reps with a stop) | throttled s (med) | pgrace deletes | pgrace min wait ms | mark share % (all / 2-5) |
|---|---|---|---|---|---|---|---|---|---|
| b01-D-65536-w4k | D | 63,690 | 0.62 | 1.3 / 3.5 | 0 (0) | 3.4 | 16379 | 2034 | 3.03 / 3.17 |
| b02-C2-65536-w4k | C2 | 62,343 | 0.63 | 2.7 / 3.8 | 0 (0) | 4.9 | 16273 | 2000 | 3.10 / 3.23 |
| b03-O-65536-w4k | O | 18,118 | 3.25 | 10.9 / 19.7 | 4 (2) | 43.6 | 0 | 0 | 1.30 / 1.46 |
| b04-O-65536-w4k | O | 17,634 | 2.63 | 6.4 / 10.4 | 0 (0) | 43.4 | 0 | 0 | 1.28 / 1.44 |
| b05-C2-65536-w4k | C2 | 63,432 | 0.62 | 1.5 / 2.4 | 0 (0) | 4.9 | 16128 | 2010 | 2.84 / 2.99 |
| b06-D-65536-w4k | D | 64,982 | 0.62 | 1.3 / 2.1 | 0 (0) | 5.2 | 16340 | 2000 | 2.92 / 3.11 |

## Safety per block

| block | arm | fio errors | verify (crc32c read-back) | fsck exit | exhausted | auth failures | revalidated | EIO/ENOSPC lines | fulls+enospc | driver result | WARN/ERROR lines |
|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-D-65536-w4k | D | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| b02-C2-65536-w4k | C2 | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| b03-O-65536-w4k | O | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 6 |
| b04-O-65536-w4k | O | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 6 |
| b05-C2-65536-w4k | C2 | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| b06-D-65536-w4k | D | 0 | rc=0 job_errors=0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

- b03-O-65536-w4k: `1795795206298 WARN  lunix_engine::pressure: writes wait for space waiters=2 limit=17966332409 used=18057370535 full_timeout_ms=60000`

- b03-O-65536-w4k: `1795795212191 WARN  lunix_gc::collector: gc urgent compaction stopped error=no space`

- b03-O-65536-w4k: `1795795217268 WARN  lunix_gc::collector: gc compaction stopped for this pass error=no space`

- b03-O-65536-w4k: `1795795271929 WARN  lunix_engine::pressure: writes wait for space waiters=2 limit=17966038644 used=18014363454 full_timeout_ms=60000`

- b04-O-65536-w4k: `1795795632772 WARN  lunix_engine::pressure: writes wait for space waiters=2 limit=17965666277 used=18079314843 full_timeout_ms=60000`

- b04-O-65536-w4k: `1795795632772 WARN  lunix_engine::coord: commit headroom is short; charging actual sizes after the fact`

- b04-O-65536-w4k: `1795795632898 WARN  lunix_gc::collector: gc compaction stopped for this pass error=no space`

- b04-O-65536-w4k: `1795795633737 WARN  lunix_gc::collector: gc urgent compaction stopped error=no space`

## Per arm: median over blocks (worst block in brackets)

| metric | C2 (n=2) | D (n=2) | O (n=2) |
|---|---|---|---|
| rep 1: first delete after onset s | 0.08 (0.09) | 7.32 (8.78) | 33.58 (35.79) |
| rep 1: stops | 0 (0) | 0 (1) | 4 (5) |
| rep 1: longest write wait s | 1.1 (1.2) | 4.5 (5.4) | 36.4 (38.5) |
| rep 1: throttled s | 3.0 (3.4) | 8.8 (9.8) | 42.7 (45.1) |
| rep 1: GiB deleted within 5 s of onset | 12.29 (12.23) | 0.00 (0.00) | 0.00 (0.00) |
| rep 1: IOPS | 67,877 (66,372) | 60,498 (60,120) | 19,602 (19,209) |
| rep 1: p99.9 s | 0.61 (0.62) | 0.65 (0.67) | 6.85 (6.88) |
| reps 2-5: IOPS (median of reps) | 62,887 (62,343) | 64,336 (63,690) | 17,876 (17,634) |
| reps 2-5: p99.9 s (median of reps) | 0.62 (0.63) | 0.62 (0.62) | 2.94 (3.25) |
| reps 2-5: longest write wait s (median of reps) | 2.1 (2.7) | 1.3 (1.3) | 8.7 (10.9) |
| reps 2-5: longest write wait s (worst rep) | 3.1 (3.8) | 2.8 (3.5) | 15.0 (19.7) |
| reps 2-5: stops (4 reps) | 0 (0) | 0 (0) | 2 (4) |
| reps 2-5: throttled s (median of reps) | 4.9 (4.9) | 4.3 (5.2) | 43.5 (43.6) |
| pgrace_deletes | 16,200 (16,128) | 16,360 (16,340) | 0 (0) |
| pgrace_min_wait_ms (least) | 2,005 (2,000) | 2,017 (2,000) | 0 (0) |
| mark share of the coordinator, all reps % | 2.97 (3.10) | 2.98 (3.03) | 1.29 (1.30) |
| mark share of the coordinator, reps 2-5 % | 3.11 (3.23) | 3.14 (3.17) | 1.45 (1.46) |

## pgracet6 rep-1 gate for the short grace, per block

| block | arm | first delete <= 3 s | 0 stops | longest wait <= 5 s | throttled <= 22 s | >= 6 GiB in 5 s | all |
|---|---|---|---|---|---|---|---|
| b01-D-65536-w4k | D | FAIL | pass | FAIL | pass | FAIL | FAIL |
| b02-C2-65536-w4k | C2 | pass | pass | pass | pass | pass | PASS |
| b03-O-65536-w4k | O | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |
| b04-O-65536-w4k | O | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL |
| b05-C2-65536-w4k | C2 | pass | pass | pass | pass | pass | PASS |
| b06-D-65536-w4k | D | FAIL | FAIL | pass | pass | FAIL | FAIL |
