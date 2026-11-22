#!/bin/sh
# Inspect the pinned corpus without executing any of its contents.
set -eu
root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
cd "$root"
expected=$(git rev-parse --verify 'HEAD:vendor/test262') || exit 1
if [ ! -e vendor/test262/.git ]; then
    echo 'Test262 is not initialized; run git submodule update --init vendor/test262' >&2
    exit 1
fi
actual=$(git -C vendor/test262 rev-parse --verify HEAD)
if [ "$expected" != "$actual" ]; then
    echo "Test262 revision mismatch: superproject $expected, checkout $actual" >&2
    exit 1
fi
for file in LICENSE harness/assert.js harness/sta.js test/language/expressions/addition/S11.6.1_A1.js; do
    if [ ! -f "vendor/test262/$file" ]; then
        echo "Missing Test262 file: $file" >&2
        exit 1
    fi
done
printf 'Test262 checkout matches superproject gitlink: %s\n' "$actual"
