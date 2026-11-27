# pgracemix tables

## Per block (reps 2-5 medians unless noted)

| block | arm | start load 1/5/15 | fio err | verify | fsck | reads (all) | revalidated | per M | exhausted | auth fail | stale age min/max ms | pgrace deletes | pgrace min wait ms | raised % rep1 / 2-5 | stops (5 reps) | WARN/ERROR lines |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-C-65536-w4kmix | C | 17.73/140.44/130.94 | 0 | rc=0 job_errors=0 | 0 | 6,489,670 | 0 | 0.000 | 0 | 0 | 0/0 | 11,381 | 5009 | 57 / 71 | 13 | 11 |
| b02-C2-65536-w4kmix | C2 | 111.63/284.14/220.65 | 0 | rc=0 job_errors=0 | 0 | 8,100,816 | 0 | 0.000 | 0 | 0 | 0/0 | 13,619 | 2000 | 47 / 54 | 2 | 3 |
| b03-C2-65536-w4kmix | C2 | 115.83/321.64/278.13 | 0 | rc=0 job_errors=0 | 0 | 7,795,432 | 0 | 0.000 | 0 | 0 | 0/0 | 13,315 | 2017 | 42 / 53 | 18 | 7 |
| b04-C-65536-w4kmix | C | 114.92/330.68/314.27 | 0 | rc=0 job_errors=0 | 0 | 7,164,233 | 0 | 0.000 | 0 | 0 | 0/0 | 11,948 | 5002 | 57 / 74 | 8 | 2 |

## Per block, throughput and latency

| block | arm | group | write IOPS | read IOPS | read p99 ms | read p99.9 ms | read max ms | longest write wait s (per-second max) | longest read wait s | throttled s | stops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| b01-C-65536-w4kmix | C | rep 1 | 42,284 | 18,142 | 167 | 742 | 4,296 | 4.34 | 4.30 | 11.3 | 3 |
| b01-C-65536-w4kmix | C | reps 2-5 median (worst) | 38,241 | 16,407 | 237 | 738 | 4,502 | 4.58 | 4.50 | 13.2 | 10 |
| b02-C2-65536-w4kmix | C2 | rep 1 | 45,774 | 19,644 | 158 | 700 | 4,672 | 4.79 | 4.67 | 6.2 | 0 |
| b02-C2-65536-w4kmix | C2 | reps 2-5 median (worst) | 50,944 | 21,838 | 121 | 633 | 3,872 | 4.11 | 3.87 | 7.3 | 2 |
| b03-C2-65536-w4kmix | C2 | rep 1 | 47,593 | 20,421 | 137 | 667 | 4,605 | 4.69 | 4.61 | 7.3 | 6 |
| b03-C2-65536-w4kmix | C2 | reps 2-5 median (worst) | 51,125 | 21,919 | 113 | 642 | 4,921 | 4.60 | 4.92 | 6.4 | 12 |
| b04-C-65536-w4kmix | C | rep 1 | 45,699 | 19,608 | 160 | 625 | 2,252 | 2.21 | 2.25 | 8.2 | 2 |
| b04-C-65536-w4kmix | C | reps 2-5 median (worst) | 44,974 | 19,280 | 159 | 671 | 3,794 | 4.49 | 3.79 | 11.6 | 6 |

## Per arm: median over blocks (worst block in brackets)

| metric | C (n=2) | C2 (n=2) |
|---|---|---|
| read_revalidated per million guest reads (whole block) | 0.000 (0.000) | 0.000 (0.000) |
| read_revalidated (count, whole block) | 0 (0) | 0 (0) |
| read_revalidate_exhausted | 0 (0) | 0 (0) |
| extent_auth_failures | 0 (0) | 0 (0) |
| read_stale_age_min_ms (0 = no sample) | 0 (0) | 0 (0) |
| read_stale_age_max_ms | 0 (0) | 0 (0) |
| guest write IOPS, reps 2-5 | 41,608 (38,241) | 51,035 (50,944) |
| guest read IOPS, reps 2-5 | 17,844 (16,407) | 21,878 (21,838) |
| guest write IOPS, rep 1 | 43,992 (42,284) | 46,683 (45,774) |
| guest read IOPS, rep 1 | 18,875 (18,142) | 20,032 (19,644) |
| read p99 ms, reps 2-5 | 198 (237) | 117 (121) |
| read p99.9 ms, reps 2-5 | 705 (738) | 638 (642) |
| read max ms, reps 1-5 (worst rep) | 4,148 (4,502) | 4,796 (4,921) |
| longest write wait s, reps 1-5 (per-second max of fio write latency) | 4.53 (4.58) | 4.74 (4.79) |
| longest write wait s, reps 2-5 | 4.53 (4.58) | 4.36 (4.60) |
| stops (5 reps) | 10 (13) | 10 (18) |
| pgrace_deletes | 11,664 (11,381) | 13,467 (13,315) |
| pgrace_min_wait_ms | 5,006 (5,002) | 2,008 (2,000) |
| pressure raised, % of reps 2-5 | 73 (71) | 54 (53) |
| pressure raised, % of rep 1 | 57 (57) | 44 (42) |
| `read part retried` debug lines | 0 (0) | 0 (0) |

Worst block: the largest value, except IOPS, pgrace and stale-age minimum and raised share (the smallest).

