# M16 Part B (2026-11-18T14:29:23Z, kernel 7.3, 16 cpus): workloads randmix seqrand pgbench, chunks 65536 16384, 48 GiB volumes, queues 4
- daemon CPUs 0-7 (queue threads 0-3), workload CPUs 8-15; script cap 79200 s
- load average at the start: 0.00 0.59 (1 and 5 min), 16 cpus

## chunk/randmix/c64k/ext4/rep1
- PASS create csrandmi64ext41
- PASS attach (4 queues)
- PASS chunk/randmix/c64k/ext4/rep1: guest fsck clean
- PASS chunk/randmix/c64k/ext4/rep1: detach
- PASS chunk/randmix/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/randmix/c64k/ext4/rep1: workload rc 0 in 1817s, daemon CPU 312.9 s

## chunk/randmix/c16k/ext4/rep1
- PASS create csrandmi16ext41
- PASS attach (4 queues)
- PASS chunk/randmix/c16k/ext4/rep1: guest fsck clean
- PASS chunk/randmix/c16k/ext4/rep1: detach
- PASS chunk/randmix/c16k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/randmix/c16k/ext4/rep1: workload rc 0 in 1805s, daemon CPU 269.0 s

## chunk/seqrand/c64k/ext4/rep1
- PASS create csseqran64ext41
- PASS attach (4 queues)
- PASS chunk/seqrand/c64k/ext4/rep1: guest fsck clean
- PASS chunk/seqrand/c64k/ext4/rep1: detach
- PASS chunk/seqrand/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/seqrand/c64k/ext4/rep1: workload rc 0 in 1358s, daemon CPU 186.3 s

## chunk/seqrand/c16k/ext4/rep1
- PASS create csseqran16ext41
- PASS attach (4 queues)
- PASS chunk/seqrand/c16k/ext4/rep1: guest fsck clean
- PASS chunk/seqrand/c16k/ext4/rep1: detach
- PASS chunk/seqrand/c16k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/seqrand/c16k/ext4/rep1: workload rc 0 in 1383s, daemon CPU 177.7 s

## chunk/pgbench/c64k/ext4/rep1
- PASS create cspgbenc64ext41
- PASS attach (4 queues)
- PASS chunk/pgbench/c64k/ext4/rep1: guest fsck clean
- PASS chunk/pgbench/c64k/ext4/rep1: detach
- PASS chunk/pgbench/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/pgbench/c64k/ext4/rep1: workload rc 0 in 2640s, daemon CPU 341.0 s

## chunk/pgbench/c64k/xfs/rep1
- PASS create cspgbenc64xfs1
- PASS attach (4 queues)
- PASS chunk/pgbench/c64k/xfs/rep1: guest fsck clean
- PASS chunk/pgbench/c64k/xfs/rep1: detach
- PASS chunk/pgbench/c64k/xfs/rep1: lunix fsck --deep clean (exit 0)
- chunk/pgbench/c64k/xfs/rep1: workload rc 0 in 2619s, daemon CPU 331.5 s

## chunk/pgbench/c16k/ext4/rep1
- PASS create cspgbenc16ext41
- PASS attach (4 queues)
- PASS chunk/pgbench/c16k/ext4/rep1: guest fsck clean
- PASS chunk/pgbench/c16k/ext4/rep1: detach
- PASS chunk/pgbench/c16k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/pgbench/c16k/ext4/rep1: workload rc 0 in 2673s, daemon CPU 320.0 s

## chunk/pgbench/c16k/xfs/rep1
- PASS create cspgbenc16xfs1
- PASS attach (4 queues)
- PASS chunk/pgbench/c16k/xfs/rep1: guest fsck clean
- PASS chunk/pgbench/c16k/xfs/rep1: detach
- PASS chunk/pgbench/c16k/xfs/rep1: lunix fsck --deep clean (exit 0)
- chunk/pgbench/c16k/xfs/rep1: workload rc 0 in 2720s, daemon CPU 308.8 s

overall FAIL=0
