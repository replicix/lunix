#!/usr/bin/env bash
# Feature-edge check: the test-only features may not appear on a normal or
# build edge of any crate but the one that declares them.
#
#   test-rng, tripwire  -> lunix-crypto
#   crashpoints         -> lunix-iface
#
# Reads the output of
#   cargo tree --workspace --prefix none -e normal,build -f '{p} {f}'
# from --input FILE (tests) or runs it. Dev-dependency edges are not in that
# output; the explicit `cargo build -p lunixd --features crashpoints` build is
# checked separately by ci.sh and is not an input here.
set -euo pipefail

input=""
if [ "${1:-}" = "--input" ]; then input="$2"; fi

if [ -n "$input" ]; then
  tree="$(cat "$input")"
else
  tree="$(cargo tree --workspace --prefix none -e normal,build -f '{p} {f}')"
fi

printf '%s\n' "$tree" | awk '
  BEGIN { owner["test-rng"] = "lunix-crypto"; owner["tripwire"] = "lunix-crypto"; owner["crashpoints"] = "lunix-iface"; bad = 0 }
  {
    line = $0
    gsub(/\([^)]*\)/, "", line)          # drop (path) and (*) markers
    n = split(line, t, /[ \t]+/)
    k = 0
    for (i = 1; i <= n; i++) if (t[i] != "") w[++k] = t[i]
    if (k < 3) next                        # no feature list on this line
    name = w[1]; feats = w[3]
    nf = split(feats, f, ",")
    for (j = 1; j <= nf; j++) {
      if ((f[j] in owner) && owner[f[j]] != name) {
        print "check-features: " name " enables " f[j] " on a normal or build edge (only " owner[f[j]] " may)" > "/dev/stderr"
        bad = 1
      }
    }
  }
  END { exit bad }
'
echo "check-features: ok"
