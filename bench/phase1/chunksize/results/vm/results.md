# M16 Part B (2026-11-18T21:07:54Z, kernel 7.3, 16 cpus): workloads vm, chunks 65536 16384, 48 GiB volumes, queues 4
- daemon CPUs 0-7 (queue threads 0-3), workload CPUs 8-15; script cap 31200 s
- load average at the start: 0.04 1.42 (1 and 5 min), 16 cpus

## chunk/vm/c64k/ext4/rep1
- PASS create csvm64ext41
- PASS attach (4 queues)
- PASS chunk/vm/c64k/ext4/rep1: guest fsck clean
- PASS chunk/vm/c64k/ext4/rep1: detach
- PASS chunk/vm/c64k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/vm/c64k/ext4/rep1: workload rc 0 in 3676s, daemon CPU 156.1 s

## chunk/vm/c16k/ext4/rep1
- PASS create csvm16ext41
- PASS attach (4 queues)
- PASS chunk/vm/c16k/ext4/rep1: guest fsck clean
- PASS chunk/vm/c16k/ext4/rep1: detach
- PASS chunk/vm/c16k/ext4/rep1: lunix fsck --deep clean (exit 0)
- chunk/vm/c16k/ext4/rep1: workload rc 0 in 3158s, daemon CPU 396.6 s

overall FAIL=0
