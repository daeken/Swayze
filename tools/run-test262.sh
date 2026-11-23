#!/bin/sh
# Opt-in only. The example reads the reviewed manifest and Test262 bytes from Git objects.
set -eu
case $#:$* in
    0:|1:--self-test) ;;
    *) echo 'usage: tools/run-test262.sh [--self-test]' >&2; exit 2 ;;
esac
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"

# Preserve the example's result codes; build/launch errors are infrastructure errors.
result=0
for profile in debug release; do
    printf 'Test262 slice (%s)\n' "$profile"
    if [ "$profile" = release ]; then
        flag=--release
    else
        flag=
    fi
    status=0
    cargo run --quiet --frozen -p swayze-engine --example run_test262 $flag -- "$@" || status=$?
    case $status in
        0) ;;
        1) if [ "$result" -eq 0 ]; then result=1; fi ;;
        *) echo "infra: $profile runner exited with status $status" >&2; result=2 ;;
    esac
done
exit "$result"
