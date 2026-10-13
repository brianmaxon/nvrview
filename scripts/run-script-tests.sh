#!/bin/sh
# Runs the tests of the check, merge, lock, and cache scripts at the same time,
# one process each, with each one's output buffered and printed in order.
# Exits nonzero when any of them failed and names which.
#
#   scripts/run-script-tests.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."
py=$(python3 -I -c 'import sys; print(sys.executable)')
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

tests="test-stamps.sh test-merge-train.sh test-suite-cache.sh test_suite_cache.py test_release_notes_bundle.py"
for t in $tests; do
  case "$t" in
    *.py) cmd="$py -I scripts/$t" ;;
    *) cmd="sh scripts/$t" ;;
  esac
  (
    # shellcheck disable=SC2086 # cmd is a command and one argument
    if $cmd >"$out/$t.log" 2>&1; then echo 0 >"$out/$t.rc"; else echo "$?" >"$out/$t.rc"; fi
  ) &
done
wait

failed=""
for t in $tests; do
  rc=$(cat "$out/$t.rc" 2>/dev/null || echo 1)
  printf '== scripts/%s: %s\n' "$t" "$([ "$rc" = 0 ] && echo passed || echo "FAILED (exit $rc)")"
  cat "$out/$t.log"
  if [ "$rc" != 0 ]; then failed="$failed $t"; fi
done
if [ -n "$failed" ]; then
  echo "script tests failed:$failed" >&2
  exit 1
fi
