#!/bin/sh
# prints only generic host categories
echo "AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root"
echo "kernel $(uname -r | sed 's/^\([0-9]*\.[0-9]*\.[0-9]*\).*/\1/') (Ubuntu); $(. /etc/os-release; echo $PRETTY_NAME)"
echo "$(mkfs.ext4 -V 2>&1 | head -1); $(mkfs.xfs -V); $(mkfs.btrfs --version); $(trace-cmd -h 2>&1 | head -0)python3 $(python3 -V | cut -d' ' -f2), sqlite $(python3 -c 'import sqlite3;print(sqlite3.sqlite_version)'), $(cc --version | head -1)"
