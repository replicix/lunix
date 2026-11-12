## Host

- CPU: AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node(s)
- Memory: ~31 GiB
- Kernel: 7.0.0 (Ubuntu); distro: Ubuntu 26.04
- Storage: local NVMe SSD; scratch dir fs: ext4 (root-backed); write_cache of the device: write through
- CPU governor: not exposed; turbo/boost: not exposed
- kernel.io_uring_disabled: 0
- Kernel config: CONFIG_BLK_DEV_UBLK=m CONFIG_BLKDEV_UBLK_LEGACY_OPCODES=not set CONFIG_BLK_DEV_NULL_BLK=m
- Tools: fio-3.41; mke2fs 1.47.2 (1-Jan-2025); mkfs.xfs version 6.18.0; mkfs.btrfs, part of btrfs-progs v6.17.1; losetup from util-linux 2.41.3; jq-1.8.1; Python 3.14.4; rustc 1.96.0 (ac68faa20 2026-05-25)
- Date: 2026-11-11
