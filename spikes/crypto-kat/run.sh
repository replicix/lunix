#!/bin/sh
# Reproduce P0.8: build, test, emit + cross-check vectors, nonce-reuse demo, cost bench.
# Run on the test host only (plan 00 / common rules): CPU=<core> sets the bench core.
set -eu
cd "$(dirname "$0")"
unset CARGO_TARGET_DIR
cargo build --release
cargo test --release
out=$(mktemp)
./target/release/crypto-kat vectors > "$out"
python3 -I tools/crosscheck.py "$out"
./target/release/crypto-kat demo
taskset -c "${CPU:-8}" ./target/release/crypto-kat bench
rm -f "$out"
