#!/usr/bin/env bash
# jstall (review S1): the ring-file re-initialisation by ZERO_RANGE on ext4,
# xfs and btrfs, on loop-device images under the milestone's own directory.
# For each file system: the microbenchmark, the nearly-full cases (a filler
# leaves 8 MiB free; once with the real file system, once with ZERO_RANGE
# failing with ENOSPC) and the reuse and recovery tests of lunix-journal.
#
# Root only, one at a time:
#   tools/remote.sh root bash tools/jstall-zero-fs.sh <target-dir> <work-dir>
# Both arguments are relative to the home of the user who ran sudo; the target
# directory holds `release/journal-zero-bench` and the debug test binary
# `reclaim-*` (built beforehand as that user), the work directory is created
# and removed here. Prints one block per file system.
set -euo pipefail

target="${1:?target dir}"
work="${2:?work dir}"
home="$(getent passwd "${SUDO_USER:?run under sudo}" | cut -d: -f6)"
target="$home/$target"
work="$home/$work"
bench="$target/release/journal-zero-bench"
reuse="$(ls -t "$target"/debug/deps/reclaim-* 2>/dev/null | grep -v '\.d$' | head -1)"
[ -x "$bench" ] && [ -x "$reuse" ] || { echo "jstall-zero-fs: binaries missing" >&2; exit 2; }

dev=""
mnt=""
cleanup() {
  set +e
  [ -n "$mnt" ] && mountpoint -q "$mnt" && umount "$mnt"
  [ -n "$dev" ] && losetup -d "$dev"
  rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work"
for fs in ext4 xfs btrfs; do
  img="$work/$fs.img"
  mnt="$work/mnt-$fs"
  mkdir -p "$mnt"
  truncate -s 3G "$img"
  dev="$(losetup --find --show "$img")"
  case "$fs" in
    ext4) mkfs.ext4 -F -q "$dev" ;;
    xfs) mkfs.xfs -q -f "$dev" ;;
    btrfs) mkfs.btrfs -q -f "$dev" ;;
  esac
  mount "$dev" "$mnt"
  echo "== $fs ($(stat -f -c %T "$mnt"), $(df -h --output=avail "$mnt" | tail -1 | tr -d ' ') free)"
  echo "-- microbenchmark"
  "$bench" --dir "$mnt/zb" --rounds 8 --probe 128 2>/dev/null
  echo "-- nearly full, the real file system"
  "$bench" --dir "$mnt/nf" --nearly-full 8 2>/dev/null
  echo "-- nearly full, ZERO_RANGE answering ENOSPC"
  "$bench" --dir "$mnt/nf2" --nearly-full 8 --inject-errno 28 2>/dev/null
  echo "-- reuse and recovery tests"
  mkdir -p "$mnt/t"
  LUNIX_JOURNAL_TEST_DIR="$mnt/t" "$reuse" --test-threads=1 reinitialised freed_files a_zero_range_that_fails 2>&1 \
    | grep -E "^test |^test result"
  umount "$mnt"
  losetup -d "$dev"
  dev=""
  rm -f "$img"
done
echo "== done"
