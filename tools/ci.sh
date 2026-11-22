#!/usr/bin/env bash
# The common gate of every milestone: fmt, clippy, tests, ownership, the
# feature-edge check and the tool tests.
#
# Never run this on a developer workstation: it compiles. Use
#   tools/remote.sh run tools/ci.sh
# (remote.sh sets LUNIX_ON_HOST=1 on the test host, plus CARGO_TARGET_DIR).
# On a hosted CI runner set LUNIX_ON_HOST=1 yourself (see
# .github/workflows/ci.yml).
set -euo pipefail

if [ "${LUNIX_ON_HOST:-}" != "1" ]; then
  echo "ci.sh compiles: run it with 'tools/remote.sh run tools/ci.sh' (or set LUNIX_ON_HOST=1 on a CI runner)" >&2
  exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here/.."
jobs="${LUNIX_JOBS:-4}"

step() { printf '\n== %s\n' "$*"; }

step "tool tests"
bash tools/tests/run.sh

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy -j "$jobs" --workspace --all-targets -- -D warnings

step "cargo test --workspace"
cargo test -j "$jobs" --workspace

step "lunix-iface with crashpoints: clippy and tests (lead-approved ci.sh line, M1.B1 review 15)"
cargo clippy -j "$jobs" -p lunix-iface --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-iface --features crashpoints

step "lunix-store with crashpoints: clippy and kill-sweep tests (lead, M7 review major 2)"
cargo clippy -j "$jobs" -p lunix-store --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-store --features crashpoints

step "lunix-journal with crashpoints: clippy and kill tests (lead, M3)"
cargo clippy -j "$jobs" -p lunix-journal --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-journal --features crashpoints --test crashpoints

step "lunix-destage with crashpoints: clippy and tests (lead, M9)"
cargo clippy -j "$jobs" -p lunix-destage --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-destage --features crashpoints

step "lunix-engine with crashpoints: clippy and tests (lead, M11)"
cargo clippy -j "$jobs" -p lunix-engine --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-engine --features crashpoints

step "lunix-ublk with crashpoints: clippy and unit tests (lead, M10; root cells run via tools/remote.sh root only)"
cargo clippy -j "$jobs" -p lunix-ublk --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-ublk --features crashpoints

step "lunix-index with crashpoints: clippy and the rebuild kill tests (lead, M17.B review B-3)"
cargo clippy -j "$jobs" -p lunix-index --all-targets --features crashpoints -- -D warnings
cargo test -j "$jobs" -p lunix-index --features crashpoints --test rebuild_crash --test batch_crash

step "lunix-gc with crashpoints: clippy (lead, M17.A)"
cargo clippy -j "$jobs" -p lunix-gc --all-targets --features crashpoints -- -D warnings

step "lunix-crypto library lint without test features (lead, M2 review)"
cargo clippy -j "$jobs" -p lunix-crypto --lib -- -D warnings

step "allow_floor_reset is used only by lunix-fsck (ADR-31: only offline repair lowers the floor)"
# The identifier itself, in any form (`: true`, `: flag`, a struct update),
# outside the store that defines it, lunix-fsck and the testkit; doc comments
# that name it are not uses.
if grep -rnw 'allow_floor_reset' crates --include=*.rs \
    | grep -vE '^crates/(lunix-fsck/|lunix-store/src/|lunix-testkit/)' \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//'; then
  echo "allow_floor_reset must not be used outside lunix-store, lunix-fsck and lunix-testkit" >&2
  exit 1
fi

step "feature-edge check"
bash tools/check-features.sh

step "lunixd with crashpoints: the build warns at start (lead, M12a final review F2; not tested with --all-features)"
cargo test -j "$jobs" -p lunixd --features crashpoints --test startup

step "lunixd with testsupport: clippy, then the stack tests in release with lunix-fsck at their default case counts (lead, M12b R1 and review)"
# `cargo test --workspace` builds lunixd without `testsupport`, so the stack
# tests compile to nothing there. They run in release (the debug profile
# spends most of its time scanning ring files at every open), with a release
# lunix-fsck next to them that they must find (LUNIX_STACK_REQUIRE_FSCK),
# built with crashpoints (stack_gc kills `--truncate-tip` between its steps;
# inert without LUNIX_CRASH_AT). The kill loops (stack_kill, crashpoints, minutes) stay out of CI; their command
# is in the M12b report.
cargo clippy -j "$jobs" -p lunixd --all-targets --features testsupport,crashpoints -- -D warnings
cargo build -j "$jobs" --release -p lunix-fsck --features crashpoints
stack_tests=(--lib)
for t in stack_support stack_model stack_gc stack_gc_ladder stack_gc_rate stack_throttle control_gc status_lockfree stack_window stack_jread stack_seal; do
  [ -f "crates/lunixd/tests/$t.rs" ] && stack_tests+=(--test "$t")
done
LUNIX_STACK_REQUIRE_FSCK=1 cargo test -j "$jobs" --release -p lunixd --features testsupport "${stack_tests[@]}"

step "lunixd with crashpoints: a write after a kill between C6 and C9 and the recovery does not stall (milestone jcut)"
cargo test -j "$jobs" --release -p lunixd --features testsupport,crashpoints --test stack_ring_recover

if [ "${LUNIX_SKIP_OWNERSHIP:-}" = "1" ]; then
  step "ownership (skipped: not a milestone branch)"
else
  step "ownership"
  bash tools/check-ownership.sh
fi

printf '\nci: all green\n'
