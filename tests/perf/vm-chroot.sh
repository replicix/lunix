#!/bin/bash
# M16 Part B workload 3, the part that needs mounts: run by chunksize.sh inside `unshare --mount --propagation private`, so
# nothing mounted here is visible to (or leaks into) the mount namespaces of the host's services and the whole tree
# disappears when this process exits.
#   vm-chroot.sh PART ROOT CACHE PACKAGES STEP_CAP LOG
# PART: the partition (or loop device) holding the image's root file system, mounted nosuid,nodev on ROOT. The host's /dev is
# bound read-only (apt and dpkg scripts need /dev/null); this is the one exception to CONVENTIONS rule 3 (every mount nosuid,nodev
# under the scratch directory), accepted by the lead on 2026-11-18. Prints "pkgs_before", "pkgs_after" and "apt_rc" lines.
set -u
part=$1 root=$2 cache=$3 pkgs=$4 cap=$5 log=$6
mount -o nosuid,nodev "$part" "$root" || exit 20
mount -t proc proc "$root/proc" || exit 21
mount --rbind -o ro /dev "$root/dev" || exit 22
mount --make-rslave "$root/dev" 2>/dev/null
mkdir -p "$cache" "$root/var/cache/apt/archives"
mount --bind "$cache" "$root/var/cache/apt/archives" || exit 23
# the image's resolv.conf is usually a relative symlink into /run (dangling in the chroot): replace it with the host's resolver file
rm -f "$root/etc/resolv.conf"
if [ -s /run/systemd/resolve/resolv.conf ]; then cp -L /run/systemd/resolve/resolv.conf "$root/etc/resolv.conf"; else cp -L /etc/resolv.conf "$root/etc/resolv.conf"; fi
printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"; chmod +x "$root/usr/sbin/policy-rc.d"
cp "$pkgs" "$root/tmp/packages.txt"
echo "pkgs_before=$(chroot "$root" dpkg-query -f '.\n' -W | wc -l)"
timeout -k 60 "$cap" chroot "$root" /bin/bash -c '
  set -e; export DEBIAN_FRONTEND=noninteractive
  apt-get update -q; apt-get -y -q upgrade
  avail=""; for p in $(grep -v "^#" /tmp/packages.txt); do apt-cache policy "$p" 2>/dev/null | grep -q "Candidate: [^(]" && avail="$avail $p"; done
  echo "available: $(echo $avail | wc -w)"
  # one transaction first; names that conflict with each other (two mail transport agents, say) fall back to one by one
  apt-get -y -q --no-install-recommends install $avail || { echo "bulk install failed, one by one"; for p in $avail; do apt-get -y -q --no-install-recommends install "$p" > /dev/null 2>&1 || echo "skipped: $p"; done; }
' > "$log" 2>&1
echo "apt_rc=$?"
echo "pkgs_after=$(chroot "$root" dpkg-query -f '.\n' -W | wc -l)"
sync
