#!/bin/sh
# Opt-in only. The example reads the reviewed manifest and Test262 bytes from Git objects.
set -eu
case $#:$* in
    0:|1:--self-test) ;;
    *) echo 'usage: tools/run-test262.sh [--self-test]' >&2; exit 2 ;;
esac
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"

# Build separately so Cargo cannot mask the runner's result codes.
if ! version=$(rustc -vV) || ! host=$(printf '%s\n' "$version" | sed -n 's/^host: //p'); then
    echo 'infra: cannot determine Rust host target' >&2
    exit 2
fi
case $host in
    ''|*[!a-zA-Z0-9._-]*) echo 'infra: cannot determine Rust host target' >&2; exit 2 ;;
esac
target_dir="$PWD/target/test262"
result=0
for profile in debug release; do
    printf 'Test262 slice (%s)\n' "$profile"
    if [ "$profile" = release ]; then
        flag=--release
    else
        flag=
    fi
    if ! cargo build --quiet --frozen -p swayze-engine --example run_test262 \
        --target-dir "$target_dir" --target "$host" $flag; then
        echo "infra: $profile runner build failed" >&2
        exit 2
    fi
    status=0
    "$target_dir/$host/$profile/examples/run_test262" "$@" || status=$?
    case $status in
        0) ;;
        1) if [ "$result" -eq 0 ]; then result=1; fi ;;
        *) echo "infra: $profile runner exited with status $status" >&2; result=2 ;;
    esac
done
exit "$result"
