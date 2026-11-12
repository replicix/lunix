#!/usr/bin/env bash
# Tests of the tools themselves (no cargo, no network).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tools="$here/.."
fail=0

expect() { # <exit code wanted> <description> <command...>
  local want="$1" what="$2"; shift 2
  set +e
  "$@" >/dev/null 2>&1
  local got=$?
  set -e
  if [ "$got" != "$want" ]; then
    echo "FAIL: $what (exit $got, wanted $want)" >&2
    fail=1
  else
    echo "ok: $what"
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# --- check-features.sh against fixtures
expect 0 "features: clean tree passes" bash "$tools/check-features.sh" --input "$here/fixtures/cargo-tree-ok.txt"
expect 1 "features: crashpoints on lunix-store fails" bash "$tools/check-features.sh" --input "$here/fixtures/cargo-tree-violation.txt"
expect 1 "features: tripwire on lunix-engine fails" bash "$tools/check-features.sh" --input "$here/fixtures/cargo-tree-violation2.txt"

# --- check-ownership.sh against synthetic diffs
own() { bash "$tools/check-ownership.sh" "$@"; }
printf 'crates/lunix-map/src/lib.rs\ncrates/lunix-map/tests/a.rs\ndocs/plans/v1/reports/p1-m5.md\ncrates/lunix-map/README.md\n' > "$tmp/ok.txt"
expect 0 "ownership: m5 may change lunix-map and its report" own m5 --changed-list "$tmp/ok.txt"

printf 'crates/lunix-map/src/lib.rs\ncrates/lunix-index/src/lib.rs\n' > "$tmp/bad.txt"
expect 1 "ownership: m5 may not change lunix-index" own m5 --changed-list "$tmp/bad.txt"

printf 'crates/lunix-format/src/lib.rs\n' > "$tmp/fmt.txt"
expect 1 "ownership: m5 may not change lunix-format" own m5 --changed-list "$tmp/fmt.txt"
expect 0 "ownership: m1a may change lunix-format" own m1a --changed-list "$tmp/fmt.txt"

printf 'crates/lunix-gc/src/lib.rs\ncrates/lunix-gc/tests/churn.rs\n' > "$tmp/gc.txt"
expect 1 "ownership: m17a may not change lunix-gc/tests" own m17a --changed-list "$tmp/gc.txt"
printf 'crates/lunix-gc/tests/churn.rs\ncrates/lunix-fsck/src/gc.rs\n' > "$tmp/gc2.txt"
expect 0 "ownership: m17b may change lunix-gc/tests and fsck gc.rs" own m17b --changed-list "$tmp/gc2.txt"
printf 'crates/lunix-gc/src/lib.rs\n' > "$tmp/gc3.txt"
expect 1 "ownership: m17b may not change lunix-gc/src" own m17b --changed-list "$tmp/gc3.txt"

printf 'crates/lunixd/src/lib.rs\ncrates/lunixd/tests/stack_a.rs\n' > "$tmp/d.txt"
expect 1 "ownership: m12a may not change tests/stack_*.rs" own m12a --changed-list "$tmp/d.txt"
expect 1 "ownership: m12b may not change lunixd/src/lib.rs" own m12b --changed-list "$tmp/d.txt"
printf 'crates/lunixd/tests/stack_a.rs\ncrates/lunixd/src/testsupport.rs\n' > "$tmp/d2.txt"
expect 0 "ownership: m12b may change stack tests and testsupport" own m12b --changed-list "$tmp/d2.txt"

printf 'Cargo.toml\n' > "$tmp/cargo.txt"
expect 0 "ownership: Cargo.toml is allowed by path for m5 (commit title checked in git mode)" own m5 --changed-list "$tmp/cargo.txt"
expect 2 "ownership: unknown milestone is an error" own m99 --changed-list "$tmp/ok.txt"

printf 'docs/plans/v1/reports/p1-m6.md\n' > "$tmp/rep.txt"
expect 1 "ownership: m5 may not write m6's report" own m5 --changed-list "$tmp/rep.txt"

exit $fail
