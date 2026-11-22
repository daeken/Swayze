#!/bin/sh
# Run the local merge gate. Set up the pinned toolchain and Test262 first.
set -eu
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
sh tools/check-test262.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo test --workspace --frozen
cargo test --workspace --frozen --release
