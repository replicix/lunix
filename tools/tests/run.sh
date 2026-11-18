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

# --- remote.sh milestone ids: a plain word with an ownership entry (m18a excepted)
rem() { env -u LUNIX_HOST -u LUNIX_M bash "$tools/remote.sh" "$@"; }
expect 0 "remote: gcrate is a known milestone id" rem -m gcrate check-id
expect 0 "remote: m12c is a known milestone id" rem -m m12c check-id
expect 0 "remote: m18a needs no entry" rem -m m18a check-id
expect 2 "remote: an id without an ownership entry is refused" rem -m cargo check-id
expect 2 "remote: an id with a slash is refused" rem -m "m1/x" check-id
expect 2 "remote: an id with a dot is refused" rem -m "m1.x" check-id

# --- the ownership base is origin/main, not a stale local main
repo="$tmp/repo"
mkdir -p "$repo"
git_() { git -C "$repo" -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@" >/dev/null 2>&1; }
git_ init -q -b main
mkdir -p "$repo/tools" "$repo/crates/lunix-gc/src" "$repo/crates/lunix-space/src"
cp "$tools/check-ownership.sh" "$repo/tools/"
printf '[gcrate]\npaths = ["crates/lunix-gc/**"]\n' > "$repo/tools/ownership.toml"
echo a > "$repo/crates/lunix-gc/src/a.rs"
git_ add -A && git_ commit -q -m base
git_ checkout -q -b upstream
echo b > "$repo/crates/lunix-space/src/b.rs"
git_ add -A && git_ commit -q -m "another milestone, merged upstream"
git_ update-ref refs/remotes/origin/main upstream
git_ checkout -q -b p1/gcrate main
echo c > "$repo/crates/lunix-gc/src/c.rs"
git_ add -A && git_ commit -q -m own
git_ merge -q --no-edit upstream
# local main is still at the base: only origin/main gives the right base
expect 0 "ownership: a branch merged with origin/main is judged against origin/main" \
  env -u LUNIX_M LUNIX_BRANCH=p1/gcrate bash "$repo/tools/check-ownership.sh"
git_ update-ref -d refs/remotes/origin/main
expect 1 "ownership: without origin/main the stale local main is the base" \
  env -u LUNIX_M LUNIX_BRANCH=p1/gcrate bash "$repo/tools/check-ownership.sh"

# --- milestone guess from branch names (all names the plan's branches use)
guess() { # <branch> <wanted milestone>
  set +e
  out="$(env -u LUNIX_M LUNIX_BRANCH="$1" bash "$tools/check-ownership.sh" --changed-list "$tmp/none.txt" 2>&1)"
  set -e
  if [[ "$out" == "check-ownership: $2 ok"* ]]; then echo "ok: branch $1 -> $2"; else echo "FAIL: branch $1 -> $2 (got: $out)" >&2; fail=1; fi
}
: > "$tmp/none.txt"
for pair in p1/m1-a:m1a p1/m1-a0:m1a0 p1/m1-b1:m1b1 p1/m1-b2a:m1b2a p1/m1-b2b:m1b2b \
            p1/m2-crypto:m2 p1/m5-map:m5 p1/m12-a:m12a p1/m12-b:m12b p1/m13-a:m13a \
            p1/m13-b:m13b p1/m17-a:m17a p1/m17-b:m17b p1/m18-b:m18b p1/gcrate:gcrate; do
  guess "${pair%%:*}" "${pair##*:}"
done

exit $fail
