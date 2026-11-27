#!/bin/bash
# A stand-in for `lunix` and `lunixd` (install it under both names) to exercise the M15 scripts' control flow
# without Lunix: volumes are sparse files behind loop devices, "attach" creates a loop device and the /dev symlink,
# the daemon is a sleeping process that honours SIGTERM/SIGUSR2 and LUNIX_CRASH_AT / LUNIX_CRASH_COUNT. Root only
# (loop devices). It checks nothing about Lunix; it only proves that the scripts drive a daemon and a CLI correctly.
me=$(basename "$0")
sdir() { dirname "$SOCKET"; }
if [ "$me" = lunixd ]; then
  case "$1" in
    --list-crashpoints) printf 'fake.alpha\nfake.beta\nengine.commit_x\n'; exit 0 ;;
    --version) echo "fake-lunixd 0"; exit 0 ;;
    ctl) shift; [ "$1" = --socket ] && S=$2; [ -S "$S" ] || [ -e "$S" ] || exit 1; echo '{"ok":"pong"}'; exit 0 ;;
    --config)
      SOCK=$(sed -n 's/^socket = "\(.*\)"/\1/p' "$2"); ST=$(sed -n 's/^state_dir = "\(.*\)"/\1/p' "$2")
      mkdir -p "$ST"; : > "$SOCK"
      if [ -d "$(dirname "$SOCK")/dev" ] && [ -n "$(ls "$(dirname "$SOCK")/dev" 2>/dev/null)" ]; then
        echo "INFO volume recovered at start name=x recovered=true stall_ms=$((100 + RANDOM % 50))" >&2
      fi
      count() { [ -n "$LUNIX_CRASH_COUNT" ] && printf 'fake.alpha 10\nfake.beta 3\nengine.commit_x 0\n' > "$LUNIX_CRASH_COUNT"; }
      trap 'count; rm -f "$SOCK"; exit 0' TERM USR2
      if [ -n "$LUNIX_CRASH_AT" ]; then ( sleep "${FAKE_CRASH_AFTER:-4}"; kill -KILL $$ ) & fi
      while :; do sleep 0.2 & wait $!; done ;;
  esac
  exit 2
fi
# ---- the CLI
SOCKET=/nonexistent
while [ $# -gt 0 ]; do case "$1" in --socket) SOCKET=$2; shift 2 ;; --timeout) shift 2 ;; *) break ;; esac; done
[ -e "$SOCKET" ] || { echo "lunix: cannot reach the daemon" >&2; exit 1; }
ST=$(sdir)
cmd=$1; shift
case "$cmd" in
  volume)
    sub=$1; shift
    if [ "$sub" = create ]; then
      name=$1; shift; size=0; dir=""
      while [ $# -gt 0 ]; do case "$1" in --size) size=$2; shift 2 ;; --dir) dir=$2; shift 2 ;; *) shift ;; esac; done
      u=$(cat /proc/sys/kernel/random/uuid); mkdir -p "$dir/$u"
      jq -n --arg n "$name" --arg d "$dir/$u" '{name:$n,dir:$d,verify_reads:false}' > "$dir/$u/volume.json"
      truncate -s "$size" "$dir/$u/disk.img"; echo "created $name"
    else # list --json
      D="${ST%-state}-vols"
      jq -s '.' < <(for f in "$D"/*/volume.json; do [ -f "$f" ] || continue; dev=null; [ -L "$ST/dev/$(jq -r .name "$f")" ] && dev=\"$ST/dev/$(jq -r .name "$f")\"; jq --argjson dev "$dev" '. + {device:$dev}' "$f"; done)
    fi ;;
  attach)
    name=$1; D="${ST%-state}-vols"; vd=$(for f in "$D"/*/volume.json; do [ "$(jq -r .name "$f")" = "$name" ] && dirname "$f"; done)
    l=$(losetup -f --show --direct-io=on "$vd/disk.img") || exit 1
    echo "$l" >> "${RUN_DIR:-$LUNIX_SPIKE_DIR/run}/created-devices"; mkdir -p "$ST/dev"; ln -sf "$l" "$ST/dev/$name"; echo "attached $name $l" ;;
  detach)
    name=$1; l=$(readlink "$ST/dev/$name") || exit 4
    mountpoint -q "$l" 2>/dev/null; losetup -d "$l"; rm -f "$ST/dev/$name"; echo "detached $name" ;;
  status)
    name=$1; dev=null; [ -L "$ST/dev/$name" ] && dev=\"$ST/dev/$name\"
    jq -n --argjson dev "$dev" '{name:"x",device:$dev,stats:{bytes_written:4294967296,cold_rmw:10,ops_write:1000},space:{map:1000000,segments:5000000},journal_used:12345,dedup_ratio:2.0,compression_ratio:1.9}' ;;
  gc) echo '{}' ;;
  fsck)
    echo '{"exit":0,"findings":[],"counts":{},"ratios":{"chunk_size":65536,"mapped_chunks":100,"unique_chunks":50,"dedup_ratio":2.0,"compression_ratio":1.9,"bytes":{"segments":1000,"map_packs":10}}}'; exit "${FAKE_FSCK_RC:-0}" ;;
  *) exit 2 ;;
esac
