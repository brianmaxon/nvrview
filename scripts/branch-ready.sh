#!/bin/sh
# Says whether a branch arrives ready to merge: its tip's tree has a Mac stamp
# and a Windows stamp (full, or backed by the test result cache; see
# scripts/checked.sh and scripts/stamps-lib.sh).
#
#   scripts/branch-ready.sh BRANCH
#
# A tip whose tree differs from a checked tree in VERSION alone is ready: the
# stamps are carried over (scripts/stamps-lib.sh, carry_version_stamps) and the
# line says "VERSION-only".
#
# Prints one line (the tree hash and each stamp: full, cached with the suites
# it took from the cache, or missing) and exits 0 when both stamps exist, 1
# when one is missing. Used by scripts/merge-branch.sh and
# scripts/merge-train.sh; agents run it on their own branch before reporting.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh

[ $# -eq 1 ] || { echo "usage: $0 BRANCH" >&2; exit 2; }
tip=$(git rev-parse --verify "$1^{commit}") || { echo "branch-ready: no such branch: $1" >&2; exit 2; }
tree=$(git rev-parse "$tip^{tree}")

carry_version_stamps "$tree" >/dev/null || true

describe() { # KIND
  carried=$(sed -n 's/^carried_from=//p' "$(stamp_dir)/$tree.$1" 2>/dev/null || true)
  note=""
  if [ -n "$carried" ]; then note=", VERSION-only change from the checked tree $carried"; fi
  if ! has_stamp "$1" "$tree"; then
    printf 'missing'
  elif full_stamp "$1" "$tree"; then
    printf 'full%s' "$note"
  else
    printf 'cached (%s%s)' "$(stamp_cached "$1" "$tree")" "$note"
  fi
}

ok=0
has_stamp mac "$tree" && has_stamp windows "$tree" && ok=1
printf '%s: tree %s; Mac %s; Windows %s%s\n' "$1" "$tree" "$(describe mac)" "$(describe windows)" \
  "$([ "$ok" = 1 ] && echo ' (ready)' || echo ' (NOT READY: rerun the missing check on the clean committed branch)')"
[ "$ok" = 1 ]
