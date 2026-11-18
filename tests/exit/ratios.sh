#!/bin/bash
# shellcheck disable=SC2319  # `[ test ]; expect LABEL $?` is the idiom of the M12a scripts: $? is the test result, on purpose
# M15 item 4: dedup and compression ratios, both chunk sizes. For each chunk size, on a fresh volume per cell:
#   fio on the raw device with --buffer_compress_percentage 0/50/90 x --dedupe_percentage 0/50/90 at bs = chunk size
#   (SIZE_GIB each, default 4; refill_buffers on, or fio repeats buffers), a fully random cell (0/0 again, named `random`), the extraction of a Linux source tarball on
#   ext4 (TARBALL=<path to .tar.xz/.tar.gz>; skipped when unset), and a copy of a Debian/Ubuntu root file system
#   (ROOTFS_SRC, default /usr, read-only copy with rsync -aHx; ROOTFS_SRC=none skips; capped at ROOTFS_MAX_GIB).
# After each cell: `lunix status --json` (attached, then detached) and the exact recount of `lunix fsck --deep --json`
# are kept under raw/ratio-*/; bench/phase1/exit/summarize.py turns them into the tables and applies the gate:
# measured dedup ratio within 10 % of 1/(1-p), compression ratio within 15 % of the knob's expectation, random data
# stored raw (stored bytes <= 1.01 x logical). The table is printed whichever side the data falls on.
#
# Runtime: 2 chunk sizes x (10 fio cells x ~3 min + tarball + rootfs ~15 min + fsck) about 2.5 h. Disk: one 32 GiB
# volume at a time, store at most ~10 GiB; df-guard asks for MKFS_GIB (default 40) before the mkfs cells.
here="$(cd "$(dirname "$0")" && pwd)"
: "${CHUNKS:=65536 16384}" "${VOLMODE:=integrity}" "${SIZE_GIB:=4}" "${VOL_GIB:=32}" "${COMPRESS_PCTS:=0 50 90}" "${DEDUPE_PCTS:=0 50 90}"
: "${ROOTFS_SRC:=/usr}" "${ROOTFS_MAX_GIB:=12}" "${TARBALL:=}" "${WRITE_RUNTIME_CAP:=1800}"
read -ra _c <<< "$CHUNKS"
export SCRIPT_CAP="${SCRIPT_CAP:-$((${#_c[@]} * 12000 + 3600))}"
. "$here/lib.sh"
harness_init m15-ratios
say "# M15 ratios ($(date -u +%FT%TZ), kernel $(uname -r | cut -d- -f1), $(nproc) cpus; chunks $CHUNKS, $SIZE_GIB GiB per fio cell)"

# snapshot CELL TAG: status (attached) now
ratio_cell() { # name chunk kind(raw|ext4) writer-function
  local name=$1 chunk=$2 kind=$3; shift 3
  local cell="ratios/c$((chunk / 1024))k/$name" vol="r$((chunk / 1024))k${name//[^a-z0-9]/}"; vol=${vol:0:60}
  local d="$OUT/raw/ratio-c$((chunk / 1024))k-$name"; mkdir -p "$d"
  say ""; say "## $cell"
  cell_begin "$vol" "$VOL_GIB" "$chunk" "$VOLMODE"
  local ok=0
  if [ "$kind" != raw ]; then
    mkfs_fs "$kind" "$CUR_DEV" && mount_fs "$kind" "$CUR_DEV" "$MNT" || { fail "$cell: mkfs/mount"; rec "$cell" fail why=mkfs; return; }
  fi
  "$@" "$d"; ok=$?
  [ "$kind" = raw ] || { sync; umount_fs "$MNT"; guest_fsck "$kind" "$CUR_DEV"; expect "$cell: guest fsck clean" $?; }
  # GC runs on its own: give it GC_SETTLE seconds (default 120, past the 60 s grace) and record its state and the store size
  # the cell is left with (the store bound of the gate)
  sleep "${GC_SETTLE:-120}"
  lx gc "$vol" --status --json > "$d/gc-status.json" 2>&1
  du -s --block-size=1 "$(vol_dir "$vol")" 2>/dev/null | cut -f1 > "$d/store-bytes-settled.txt"
  status_json "$vol" > "$d/status-attached.json" 2>/dev/null
  detach_vol "$vol"; expect "$cell: detach" $?
  status_json "$vol" > "$d/status-detached.json" 2>/dev/null
  lunix_fsck "$vol" "ratio-c$((chunk / 1024))k-$name"; local frc=$?
  cp "$OUT/raw/fsck-ratio-c$((chunk / 1024))k-$name.json" "$d/fsck.json"
  expect "$cell: lunix fsck --deep clean (exit $FSCK_RC)" $frc
  store_bytes "$vol" > "$d/store-bytes.txt" 2>/dev/null
  [ "$ok" = 0 ] && [ "$frc" = 0 ]; local st=$?
  rec "$cell" "$([ "$st" = 0 ] && echo pass || echo fail)" chunk="$chunk" kind="$kind" cell_name="$name" dir="$d" writer_rc="$ok" fsck_rc="$FSCK_RC"
}
# writers ----------------------------------------------------------------------------------------------------------
fio_ratio() { # compress dedupe dir
  local c=$1 dd=$2 d=$3
  DEV="$CUR_DEV" BS=$CUR_CHUNK SIZE="${SIZE_GIB}g" COMP=$c DEDUP=$dd timeout -k 30 "$WRITE_RUNTIME_CAP" \
    taskset -c "$FIO_CPUS" fio "$REPO/bench/phase1/exit/fio/ratio.fio" --output-format=json --output="$d/fio.json" > "$d/fio.stdout" 2>&1
  local rc=$?
  # the ratios of the data fio really wrote, measured by reading it back (the yardstick next to 1/(1-p))
  taskset -c "$FIO_CPUS" python3 -I "$REPO/bench/phase1/exit/chunkstat.py" "$CUR_DEV" "$CUR_CHUNK" $((SIZE_GIB << 30)) > "$d/chunkstat.json" 2> "$d/chunkstat.err"
  [ "$rc" = 0 ] && [ "$(fio_errors "$d/fio.json")" = 0 ]
}
tar_ratio() { # dir
  case "$TARBALL" in *.xz) tar -xJf "$TARBALL" -C "$MNT" ;; *.gz|*.tgz) tar -xzf "$TARBALL" -C "$MNT" ;; *.zst) tar --zstd -xf "$TARBALL" -C "$MNT" ;; *) tar -xf "$TARBALL" -C "$MNT" ;; esac
}
rootfs_ratio() { # dir
  local s; s=$(du -sxB1G $ROOTFS_SRC 2>/dev/null | awk '{t+=$1} END{print t+0}')
  [ "$s" -le "$ROOTFS_MAX_GIB" ] || { echo "rootfs source is $s GiB, above ROOTFS_MAX_GIB=$ROOTFS_MAX_GIB" > "$1/skipped.txt"; return 3; }
  mkdir -p "$MNT/rootfs"; local p
  for p in $ROOTFS_SRC; do rsync -aHx "$p" "$MNT/rootfs/" > "$1/rsync.txt" 2>&1 || return 1; done
}
for chunk in $CHUNKS; do
  CUR_CHUNK=$chunk
  for c in $COMPRESS_PCTS; do for dd in $DEDUPE_PCTS; do
    ratio_cell "fio-c${c}-d${dd}" "$chunk" raw fio_ratio "$c" "$dd"
    [ -e "$OUT/WEDGED" ] && break 4
  done; done
  ratio_cell "random" "$chunk" raw fio_ratio 0 0
  if [ -n "$TARBALL" ] && [ -f "$TARBALL" ]; then ratio_cell "linux-tarball" "$chunk" ext4 tar_ratio; else rec "ratios/c$((chunk / 1024))k/linux-tarball" skip why="TARBALL not set"; fi
  if [ "$ROOTFS_SRC" != none ]; then ratio_cell "rootfs-copy" "$chunk" ext4 rootfs_ratio; else rec "ratios/c$((chunk / 1024))k/rootfs-copy" skip why="ROOTFS_SRC=none"; fi
done
say ""; say "overall FAIL=$FAIL"; echo "$FAIL" > "$OUT/result.txt"
exit "$FAIL"
