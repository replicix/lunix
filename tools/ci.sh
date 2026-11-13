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

step "lunix-crypto library lint without test features (lead, M2 review)"
cargo clippy -j "$jobs" -p lunix-crypto --lib -- -D warnings

step "feature-edge check"
bash tools/check-features.sh

step "lunixd with crashpoints builds (not tested with --all-features)"
cargo build -j "$jobs" -p lunixd --features crashpoints

if [ "${LUNIX_SKIP_OWNERSHIP:-}" = "1" ]; then
  step "ownership (skipped: not a milestone branch)"
else
  step "ownership"
  bash tools/check-ownership.sh
fi

printf '\nci: all green\n'
