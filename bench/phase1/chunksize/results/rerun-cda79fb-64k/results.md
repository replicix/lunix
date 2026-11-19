# M16 Part B (2026-11-19T02:31:21Z, kernel 7.3, 16 cpus): workloads randmix seqrand, chunks 65536, 48 GiB volumes, queues 4
- daemon CPUs 0-7 (queue threads 0-3), workload CPUs 8-15; script cap 31200 s
- load average at the start: 1.34 51.94 (1 and 5 min), 16 cpus
- WARNING: 5-minute load 51.94 is above 2: the host is not quiet

## chunk/randmix/c64k/ext4/rep1
- PASS create csrandmi64ext41
- PASS attach (4 queues)
- PASS chunk/randmix/c64k/ext4/rep1: guest fsck clean
- PASS chunk/randmix/c64k/ext4/rep1: detach
- PASS chunk/randmix/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/randmix/c64k/ext4/rep1: workload rc 0 in 1801s, daemon CPU 324.0 s

## chunk/seqrand/c64k/ext4/rep1
- PASS create csseqran64ext41
- PASS attach (4 queues)
- PASS chunk/seqrand/c64k/ext4/rep1: guest fsck clean
- PASS chunk/seqrand/c64k/ext4/rep1: detach
- PASS chunk/seqrand/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/seqrand/c64k/ext4/rep1: workload rc 0 in 1378s, daemon CPU 207.9 s

overall FAIL=0
