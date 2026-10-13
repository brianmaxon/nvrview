#!/bin/sh
# One line listing the most frequent flaky check steps recorded by the merge
# scripts (<git common dir>/flakes.jsonl, last 500 lines; see scripts/flakes.py).
#
#   scripts/flakes.sh [TOP]
set -eu
cd "$(git rev-parse --show-toplevel)"
exec python3 -I scripts/flakes.py summary "$@"
