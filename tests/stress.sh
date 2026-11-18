#!/bin/sh
# Run the concurrency exit programs many times, compiled in release mode and in debug mode with
# the collector running before every allocation (docs/06-roadmap.md, milestone 6).
#
# Usage: tests/stress.sh [runs]   (default 1000; the GC stress runs are a tenth of that)
set -eu
runs=${1:-1000}
root=$(cd "$(dirname "$0")/.." && pwd)
compiler="$root/target/release/weby"
[ -x "$compiler" ] || compiler="$root/target/debug/weby"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
for program in worker_pool shared_counter; do
    source="$root/tests/run/$program.wb"
    expected="$work/$program.expected"
    sed -n 's/^# expect: \{0,1\}//p' "$source" > "$expected"
    "$compiler" build --release "$source" -o "$work/$program"
    "$compiler" build "$source" -o "$work/$program.debug"
    i=0
    while [ "$i" -lt "$runs" ]; do
        if ! "$work/$program" > "$work/out" 2>&1 || ! cmp -s "$work/out" "$expected"; then
            failures=$((failures + 1))
            echo "$program: run $i failed"
            cat "$work/out"
        fi
        i=$((i + 1))
    done
    i=0
    while [ "$i" -lt $((runs / 10)) ]; do
        if ! WEBY_GC_STRESS=1 "$work/$program.debug" > "$work/out" 2>&1 || ! cmp -s "$work/out" "$expected"; then
            failures=$((failures + 1))
            echo "$program (GC stress): run $i failed"
            cat "$work/out"
        fi
        i=$((i + 1))
    done
    echo "$program: $runs release runs and $((runs / 10)) GC-stress runs done"
done
[ "$failures" -eq 0 ] && echo "no failures" || { echo "$failures failures"; exit 1; }
