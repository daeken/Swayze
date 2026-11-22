#!/bin/sh
# Opt-in Wasm i32 smoke check (both profiles); requires Node.js v22 on PATH.
# No corpus or input files. Not part of tools/check.sh.
set -eu
if [ "$#" -ne 0 ]; then
    echo "usage: $0 (no arguments)" >&2
    exit 2
fi
if ! command -v node >/dev/null 2>&1; then
    echo 'check-wasm-reference: install Node.js v22 and put node on PATH' >&2
    exit 1
fi
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
echo 'WebAssembly reference smoke check (debug)'
cargo run --quiet --frozen -p swayze-engine --example wasm_reference
echo 'WebAssembly reference smoke check (release)'
cargo run --quiet --frozen -p swayze-engine --example wasm_reference --release
