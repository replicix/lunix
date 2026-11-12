#!/usr/bin/env bash
# Fails if the branch changed a path its milestone does not own.
#
#   tools/check-ownership.sh [<milestone>] [--changed-list FILE] [--base REV]
#
# <milestone> defaults to $LUNIX_M, then to a guess from the branch name
# (p1/m1-a -> m1a; $LUNIX_BRANCH overrides the git branch). The changed files are `git diff --name-only` against
# `git merge-base HEAD main` (plus untracked files), or the lines of
# --changed-list / .lunix-changed-files (written by tools/remote.sh sync, for
# hosts that have no .git).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
table="${LUNIX_OWNERSHIP_TOML:-$here/ownership.toml}"

m="${LUNIX_M:-}"
list=""
base=""
while [ $# -gt 0 ]; do
  case "$1" in
    --changed-list) list="$2"; shift 2 ;;
    --base) base="$2"; shift 2 ;;
    -*) echo "check-ownership: unknown option $1" >&2; exit 2 ;;
    *) m="$1"; shift ;;
  esac
done

# Guess the milestone from a branch name: p1/m1-a -> m1a, p1/m5-map -> m5.
from_branch() {
  local br="${1#p1/}" first rest
  first="${br%%-*}"
  rest="${br#*-}"
  rest="${rest%%-*}"
  if [[ "$br" == *-* && "$rest" =~ ^[a-z][0-9]?$ ]]; then echo "$first$rest"; else echo "$first"; fi
}
if [ -z "$m" ] && [ -n "${LUNIX_BRANCH:-}" ]; then m="$(from_branch "$LUNIX_BRANCH")"; fi
if [ -z "$m" ] && { [ -d "$root/.git" ] || [ -f "$root/.git" ]; }; then
  m="$(from_branch "$(git -C "$root" rev-parse --abbrev-ref HEAD 2>/dev/null || true)")"
fi
if [ -z "$m" ]; then
  echo "check-ownership: milestone not given (argument or LUNIX_M)" >&2
  exit 2
fi

# Flatten the table: "<id>\t<paths|exclude>\t<glob>".
flat="$(awk '
  /^\[[a-z0-9]+\]/ { id = $0; gsub(/[\[\]]/, "", id); key = ""; next }
  /^[ \t]*(paths|exclude)[ \t]*=/ {
    key = $1; inarr = 1
  }
  inarr {
    line = $0
    while (match(line, /"[^"]*"/)) {
      print id "\t" key "\t" substr(line, RSTART + 1, RLENGTH - 2)
      line = substr(line, RSTART + RLENGTH)
    }
    if ($0 ~ /\]/) inarr = 0
  }
' "$table")"

if ! printf '%s\n' "$flat" | awk -F'\t' -v m="$m" '$1 == m { f = 1 } END { exit !f }'; then
  echo "check-ownership: no entry for milestone '$m' in $table" >&2
  exit 2
fi

# glob -> anchored extended regex
glob_to_re() {
  local g="$1" out="" i c n
  for ((i = 0; i < ${#g}; i++)); do
    c="${g:i:1}"
    case "$c" in
      '*')
        if [ "${g:i+1:1}" = '*' ]; then out+='.*'; i=$((i + 1)); else out+='[^/]*'; fi ;;
      '?') out+='[^/]' ;;
      '.'|'+'|'('|')'|'|'|'^'|'$'|'{'|'}'|'['|']'|'\\') out+="\\$c" ;;
      *) out+="$c" ;;
    esac
  done
  printf '^%s$' "$out"
}

allow=(); deny=()
while IFS=$'\t' read -r id key glob; do
  [ "$id" = "$m" ] || continue
  if [ "$key" = "paths" ]; then allow+=("$(glob_to_re "$glob")"); else deny+=("$(glob_to_re "$glob")"); fi
done <<< "$flat"

# Always allowed: the report and the owned crates' README (<= 40 lines).
always=("^docs/plans/v1/reports/p1-${m}\\.md$" '^Cargo\.toml$' '^Cargo\.lock$')
while IFS=$'\t' read -r id key glob; do
  [ "$id" = "$m" ] || continue
  case "$glob" in
    crates/*/**) crate="${glob#crates/}"; crate="${crate%%/*}"; always+=("^crates/${crate}/README\\.md$") ;;
  esac
done <<< "$flat"

changed=()
if [ -n "$list" ]; then
  mapfile -t changed < "$list"
elif [ -d "$root/.git" ] || [ -f "$root/.git" ]; then
  [ -n "$base" ] || base="$(git -C "$root" merge-base HEAD main)"
  mapfile -t changed < <({ git -C "$root" diff --name-only "$base"; git -C "$root" ls-files --others --exclude-standard; } | sort -u)
elif [ -f "$root/.lunix-changed-files" ]; then
  mapfile -t changed < "$root/.lunix-changed-files"
else
  echo "check-ownership: no git checkout and no changed-file list" >&2
  exit 2
fi

bad=0
for f in "${changed[@]}"; do
  [ -n "$f" ] || continue
  ok=0
  for re in "${allow[@]}" "${always[@]}"; do
    if [[ "$f" =~ $re ]]; then ok=1; break; fi
  done
  if [ $ok = 1 ]; then
    for re in "${deny[@]}"; do
      if [[ "$f" =~ $re ]]; then ok=0; break; fi
    done
  fi
  # README of an owned crate: at most 40 lines.
  if [ $ok = 1 ] && [[ "$f" == crates/*/README.md ]] && [ -f "$root/$f" ]; then
    if [ "$(wc -l < "$root/$f")" -gt 40 ]; then
      echo "check-ownership: $f is longer than 40 lines" >&2
      bad=1
    fi
  fi
  if [ $ok = 0 ]; then
    echo "check-ownership: $m may not change $f" >&2
    bad=1
  fi
done

# Cargo.toml/Cargo.lock outside m1a: only in commits titled
# `deps: add <crate> =<version> for <m>` that touch nothing else.
if [ "$m" != "m1a" ] && [ -z "$list" ] && { [ -d "$root/.git" ] || [ -f "$root/.git" ]; }; then
  for c in $(git -C "$root" rev-list "$base"..HEAD 2>/dev/null || true); do
    files="$(git -C "$root" show --name-only --format= "$c")"
    if printf '%s\n' "$files" | grep -Eq '^Cargo\.(toml|lock)$'; then
      title="$(git -C "$root" log -1 --format=%s "$c")"
      others="$(printf '%s\n' "$files" | grep -Ev '^(Cargo\.toml|Cargo\.lock|)$' || true)"
      if [[ ! "$title" =~ ^deps:\ add\ .+\ =.+\ for\ ${m}$ ]] || [ -n "$others" ]; then
        echo "check-ownership: commit ${c:0:9} changes Cargo.toml/Cargo.lock outside a 'deps: add <crate> =<version> for $m' commit" >&2
        bad=1
      fi
    fi
  done
fi

if [ $bad = 0 ]; then
  echo "check-ownership: $m ok (${#changed[@]} changed paths)"
fi
exit $bad
