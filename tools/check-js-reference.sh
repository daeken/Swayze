#!/bin/sh
# Opt-in Number arithmetic smoke check (both profiles); requires Node.js on PATH.
# No corpus or input files. Not part of tools/check.sh.
set -eu
if [ "$#" -ne 0 ]; then
    echo "usage: $0 (no arguments)" >&2
    exit 2
fi
if ! command -v node >/dev/null 2>&1; then
    echo 'check-js-reference: install Node.js and put node on PATH' >&2
    exit 1
fi
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
echo 'JavaScript reference smoke check (debug)'
cargo run --quiet --frozen -p swayze-engine --example js_reference
echo 'JavaScript reference smoke check (release)'
cargo run --quiet --frozen -p swayze-engine --example js_reference --release
