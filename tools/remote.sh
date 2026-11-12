#!/usr/bin/env bash
# Run builds, tests and root scripts on the remote test host; never locally.
#
#   tools/remote.sh [-m <milestone>] <subcommand> [args]
#
#   sync                  rsync the worktree (minus .git and target) to the host
#   cargo <args>          sync, then run cargo there (-j and CPU pinning applied)
#   run <cmd...>          sync, then run a command in the synced tree
#   fmt                   run `cargo fmt --all` there and pull back the *.rs
#                         files the milestone owns
#   pull <path>...        copy files or directories back from the synced tree
#   root <cmd...>         run under the host-wide root lock with the leftovers
#                         check before and after (root milestones only)
#   leftovers             run spikes/common/ublk-leftovers.sh on the host
#   df                    show free space of the host's file system
#   df-guard <path> <GiB> (absolute path) exit non-zero unless the file system holding <path>
#                         keeps at least $LUNIX_MIN_FREE_GIB (default 120) GiB
#                         free after <GiB> more are used
#
# Environment (the host is never named in a file or in output):
#   LUNIX_HOST         ssh destination                            (required)
#   LUNIX_HOST_BASE    directory on the host, e.g. lunix-p1       (required)
#   LUNIX_M or -m      milestone id, e.g. m1a                     (required)
#   LUNIX_HOST_SRC     override of <base>/<m>/src
#   LUNIX_HOST_TARGET  override of <base>/<m>/target (CARGO_TARGET_DIR)
#   LUNIX_OVERLAY      space-separated paths: sync HEAD plus only these paths
#                      from the working tree (parallel agents in one worktree)
#   LUNIX_JOBS         cargo -j value                             (default 4)
#   LUNIX_CPUS         if set, run commands under `taskset -c $LUNIX_CPUS`
#   LUNIX_MIN_FREE_GIB                                            (default 120)
#   LUNIX_ROOT_TIMEOUT seconds before TERM is sent to a root script (default 3600)
# Signing and ssh agent variables (SSH_AUTH_SOCK) come from the caller. A
# `.env.host` file in the repository root (git-ignored) is sourced if present.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
# shellcheck source=/dev/null
[ -f "$root/.env.host" ] && . "$root/.env.host"

m="${LUNIX_M:-}"
if [ "${1:-}" = "-m" ]; then m="$2"; shift 2; fi

die() { echo "remote.sh: $*" >&2; exit 2; }

[ -n "${LUNIX_HOST:-}" ] || die "LUNIX_HOST is not set"
[ -n "${LUNIX_HOST_BASE:-}" ] || die "LUNIX_HOST_BASE is not set"
[ -n "$m" ] || die "milestone not given (-m or LUNIX_M)"
[[ "$m" =~ ^m[0-9]+[a-z0-9]*$ ]] || die "bad milestone id '$m'"
[ $# -ge 1 ] || die "missing subcommand"
sub="$1"; shift

base="${LUNIX_HOST_BASE%/}"
src="${LUNIX_HOST_SRC:-$base/$m/src}"
target="${LUNIX_HOST_TARGET:-$base/$m/target}"
scratch="$base/$m/scratch"
jobs="${LUNIX_JOBS:-4}"
min_free="${LUNIX_MIN_FREE_GIB:-120}"

ssh_() { ssh -o BatchMode=yes "$LUNIX_HOST" "$@"; }

# Quote a word list for the remote shell.
q() { local out="" a; for a in "$@"; do out+=" $(printf '%q' "$a")"; done; printf '%s' "${out# }"; }

# Environment every remote command gets.
remote_env() {
  printf 'abs() { case "$1" in /*) echo "$1";; *) echo "$HOME/$1";; esac; }; export PATH="$HOME/.cargo/bin:$PATH"; unset CARGO_TARGET_DIR; export CARGO_TARGET_DIR="$(abs %q)" LUNIX_SPIKE_DIR="$(abs %q)" LUNIX_M=%q LUNIX_ON_HOST=1 LUNIX_JOBS=%q; mkdir -p "$LUNIX_SPIKE_DIR" %q; ' \
    "$target" "$scratch" "$m" "$jobs" "$src"
}

wrap() { if [ -n "${LUNIX_CPUS:-}" ]; then printf 'taskset -c %q ' "$LUNIX_CPUS"; fi; }

do_sync() {
  ssh_ "mkdir -p $(printf '%q' "$src")"
  if [ -n "${LUNIX_OVERLAY:-}" ]; then
    # Several agents edit one worktree: sync the committed tree (HEAD) plus
    # only the listed paths from the working tree, so another agent's
    # half-written file never reaches this build.
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' RETURN
    git -C "$root" archive HEAD | tar -x -C "$stage"
    rsync -a --delete --exclude target/ -e "ssh -o BatchMode=yes" "$stage/" "$LUNIX_HOST:$src/"
    # shellcheck disable=SC2086
    (cd "$root" && rsync -a --no-times --relative -e "ssh -o BatchMode=yes" $LUNIX_OVERLAY "$LUNIX_HOST:$src/")
    return 0
  fi
  rsync -a --delete --exclude target/ --exclude /.git -e "ssh -o BatchMode=yes" \
    "$root/" "$LUNIX_HOST:$src/"
  # The host copy has no .git: hand check-ownership the changed paths.
  if [ -d "$root/.git" ] || [ -f "$root/.git" ]; then
    mb="$(git -C "$root" merge-base HEAD main 2>/dev/null || true)"
    if [ -n "$mb" ]; then
      { git -C "$root" diff --name-only "$mb"; git -C "$root" ls-files --others --exclude-standard; } \
        | sort -u | ssh_ "cat > $(printf '%q' "$src")/.lunix-changed-files"
    fi
  fi
}

case "$sub" in
  sync) do_sync ;;

  cargo)
    [ $# -ge 1 ] || die "cargo: missing arguments"
    do_sync
    c="$1"; shift
    case "$c" in
      build|test|clippy|check|run|bench|doc|b|t|r) jflag="-j $jobs" ;;
      *) jflag="" ;;
    esac
    ssh_ "$(remote_env) cd $(printf '%q' "$src") && $(wrap)cargo $c $jflag $(q "$@")"
    ;;

  run)
    [ $# -ge 1 ] || die "run: missing command"
    do_sync
    ssh_ "$(remote_env) cd $(printf '%q' "$src") && $(wrap)$(q "$@")"
    ;;

  fmt)
    do_sync
    ssh_ "$(remote_env) cd $(printf '%q' "$src") && cargo fmt --all"
    # Pull back only *.rs files the milestone owns.
    table="$here/ownership.toml"
    globs="$(LUNIX_M="$m" awk -v m="$m" '
      /^\[[a-z0-9]+\]/ { id = $0; gsub(/[\[\]]/, "", id); next }
      id == m && /"/ { line = $0; while (match(line, /"[^"]*"/)) { print substr(line, RSTART + 1, RLENGTH - 2); line = substr(line, RSTART + RLENGTH) } }
    ' "$table")"
    [ -n "$globs" ] || die "no ownership entry for $m"
    while IFS= read -r g; do
      case "$g" in
        *.rs|*/\*\*) ;;
        *) continue ;;
      esac
      d="${g%%\**}"        # literal directory prefix of the glob
      d="${d%/}"
      [ -n "$d" ] || continue
      if [ -d "$root/$d" ]; then
        rsync -a -e "ssh -o BatchMode=yes" --include='*/' --include='*.rs' --exclude='*' \
          "$LUNIX_HOST:$src/$d/" "$root/$d/"
      elif [ -f "$root/$d" ]; then
        rsync -a -e "ssh -o BatchMode=yes" "$LUNIX_HOST:$src/$d" "$root/$d"
      fi
    done <<< "$globs"
    ;;

  pull)
    [ $# -ge 1 ] || die "pull: missing path"
    for p in "$@"; do
      case "$p" in /*|*..*) die "pull: path must be relative to the repository" ;; esac
      mkdir -p "$root/$(dirname "$p")"
      rsync -a -e "ssh -o BatchMode=yes" "$LUNIX_HOST:$src/$p" "$root/$(dirname "$p")/"
    done
    ;;

  leftovers)
    do_sync
    ssh_ "$(remote_env) cd $(printf '%q' "$src") && bash spikes/common/ublk-leftovers.sh"
    ;;

  df)
    ssh_ "df -h $(printf '%q' "$base") | tail -n 2"
    ;;

  df-guard)
    [ $# -eq 2 ] || die "df-guard: usage: df-guard <path> <GiB>"
    p="$1"; need="$2"
    [[ "$need" =~ ^[0-9]+$ ]] || die "df-guard: <GiB> must be a whole number"
    # A relative path is relative to the host's home (`~` is not expanded).
    avail="$(ssh_ "df -B1G --output=avail $(printf '%q' "$p") | tail -n 1" | tr -d ' ')"
    [[ "$avail" =~ ^[0-9]+$ ]] || die "df-guard: cannot read free space"
    if [ $((avail - need)) -lt "$min_free" ]; then
      echo "df-guard: refusing: $avail GiB free, $need GiB wanted, $min_free GiB must stay free" >&2
      exit 1
    fi
    echo "df-guard: ok ($avail GiB free, $need GiB wanted, $min_free GiB kept)"
    ;;

  root)
    # Root milestones only (M10, M12a smoke, M15, M16, M18b): one root run on
    # the host at a time; the script gets TERM at the timeout, never the
    # daemon it started (W8); a script alive at -k is a wedge (W9).
    [ $# -ge 1 ] || die "root: missing command"
    do_sync
    tmo="${LUNIX_ROOT_TIMEOUT:-3600}"
    lock="$base/root.lock"
    left="$base/leftovers.txt"
    ssh_ "$(remote_env) cd $(printf '%q' "$src") && \
      mkdir -p $(printf '%q' "$base") && \
      if [ -s $(printf '%q' "$left") ]; then echo 'root: previous run left devices behind (W9); refusing' >&2; cat $(printf '%q' "$left") >&2; exit 3; fi && \
      exec 9>$(printf '%q' "$lock") && \
      { flock -w 3600 9 || { echo 'root: lock busy for 1 h' >&2; exit 4; }; } && \
      echo \"root: lock held by \$\$ for $m\" > $(printf '%q' "$lock").holder && \
      bash spikes/common/ublk-leftovers.sh && \
      export LUNIX_ROOT_TESTS=1 && \
      { sudo -n timeout -s TERM -k 180 $tmo $(q "$@"); rc=\$?; \
        if [ \$rc -eq 137 ]; then echo 'wedge: script outlived the grace' > $(printf '%q' "$left"); fi; \
        bash spikes/common/ublk-leftovers.sh || rc=\$?; exit \$rc; }"
    ;;

  *) die "unknown subcommand '$sub'" ;;
esac
