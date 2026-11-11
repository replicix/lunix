#!/usr/bin/env bash
# Prints only the host categories allowed in results (no names, addresses, ids).
set -u
fam=$(awk -F: '/^cpu family/{print $2+0; exit}' /proc/cpuinfo)
ven=$(awk -F: '/^vendor_id/{gsub(/ /,"",$2); print $2; exit}' /proc/cpuinfo)
cpu="unknown CPU"
if [ "$ven" = AuthenticAMD ] && [ "$fam" = 26 ]; then cpu="AMD EPYC (Zen 5)"; fi
nodes=$(ls -d /sys/devices/system/node/node[0-9]* 2>/dev/null | wc -l)
smt=$(cat /sys/devices/system/cpu/smt/active 2>/dev/null || echo "?")
kmem=$(awk '/MemTotal/{printf "%.0f", $2/1048576}' /proc/meminfo)
kr=$(uname -r | sed -E 's/^([0-9]+\.[0-9]+\.[0-9]+).*/\1/')
rel=$(. /etc/os-release; echo "$NAME $VERSION_ID")
gov=$(cat /sys/devices/system/cpu/cpu8/cpufreq/scaling_governor 2>/dev/null || echo "n/a (virtualised, no cpufreq)")
rootfs=$(findmnt -no FSTYPE / 2>/dev/null)
echo "- CPU: $cpu, $(nproc --all) vCPUs, SMT active=$smt, $nodes NUMA node(s), ~${kmem} GiB RAM, local NVMe SSD, $rootfs root"
echo "- Kernel $kr (Ubuntu), $rel; governor: $gov"
echo "- rustc: $($HOME/.cargo/bin/rustc --version 2>/dev/null)"
echo "- io_uring_disabled: $(cat /proc/sys/kernel/io_uring_disabled 2>/dev/null || echo n/a) (unused by this spike)"
