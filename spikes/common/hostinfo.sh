#!/bin/bash
# Prints the CONVENTIONS "Benchmark hygiene" host header as Markdown.
# Deliberately prints only coarse categories (no hostnames, addresses, serials,
# provider details, uname -a, device models). Usage: hostinfo.sh [scratch-dir]
D="${1:-${LUNIX_SPIKE_DIR:-/var/tmp/lunix-spike}}"
mname=$(lscpu | sed -n 's/^Model name:[[:space:]]*//p' | head -1)
fam=$(lscpu | sed -n 's/^CPU family:[[:space:]]*//p' | head -1)
case "$mname" in
  AMD\ EPYC*) cpu="AMD EPYC"; [ "$fam" = 26 ] && cpu="AMD EPYC (Zen 5)" ;;
  Intel*)     cpu="Intel Xeon (family $fam)" ;;
  *)          cpu="other (family $fam)" ;;
esac
thr=$(lscpu | sed -n 's/^Thread(s) per core:[[:space:]]*//p')
smt=off; [ "${thr:-1}" -gt 1 ] && smt=on
numa=$(lscpu | sed -n 's/^NUMA node(s):[[:space:]]*//p')
kr=$(uname -r | sed 's/^\([0-9]*\.[0-9]*\.[0-9]*\).*/\1/')
distro=$(. /etc/os-release; echo "$NAME $VERSION_ID")
memg=$(awk '/MemTotal/{printf "%.0f", $2/1048576}' /proc/meminfo)
fs=$(findmnt -no FSTYPE -T "$D" 2>/dev/null || echo unknown)
src=$(findmnt -no SOURCE -T "$D" 2>/dev/null | sed 's#/dev/##')
class=other
for b in /sys/block/nvme*n1; do [ -e "$b" ] && class="local NVMe SSD"; done
wc=$(cat /sys/block/nvme0n1/queue/write_cache 2>/dev/null || echo "not exposed")
gov=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "not exposed")
turbo=$(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null || cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo "not exposed")
iou=$(cat /proc/sys/kernel/io_uring_disabled 2>/dev/null || echo n/a)
cfg() { local f=/boot/config-$(uname -r); [ -r "$f" ] && { grep -E "^$1=" "$f" | head -1 | cut -d= -f2; grep -q "^# $1 is not set" "$f" && echo "not set"; } | head -1; }
echo "## Host"
echo
echo "- CPU: $cpu, $(nproc) vCPUs, SMT $smt, $numa NUMA node(s)"
echo "- Memory: ~${memg} GiB"
echo "- Kernel: $kr (Ubuntu); distro: $distro"
echo "- Storage: $class; scratch dir fs: $fs (root-backed); write_cache of the device: $wc"
echo "- CPU governor: $gov; turbo/boost: $turbo"
echo "- kernel.io_uring_disabled: $iou"
echo "- Kernel config: CONFIG_BLK_DEV_UBLK=$(cfg CONFIG_BLK_DEV_UBLK) CONFIG_BLKDEV_UBLK_LEGACY_OPCODES=$(cfg CONFIG_BLKDEV_UBLK_LEGACY_OPCODES) CONFIG_BLK_DEV_NULL_BLK=$(cfg CONFIG_BLK_DEV_NULL_BLK)"
v() { "$@" 2>&1 | head -1; }
echo "- Tools: $(v fio --version); $(v mkfs.ext4 -V 2>&1 | head -1); $(v mkfs.xfs -V); $(v mkfs.btrfs --version); $(v losetup --version); $(v jq --version); $(v python3 --version); $(v $HOME/.cargo/bin/rustc --version)"
echo "- Date: $(date -u +%Y-%m-%d)"
