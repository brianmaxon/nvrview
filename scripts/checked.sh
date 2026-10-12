#!/bin/sh
# Check stamps: proof that a full check passed on an exact tree.
#
#   scripts/checked.sh [--mac] [--windows]
#       Exits 0 if HEAD's tree has every requested stamp (both when none is
#       named); otherwise prints what is missing and exits 1.
#   scripts/checked.sh --record mac|windows SECONDS "STEPS"
#       Writes a stamp for HEAD's tree. Called by scripts/check.sh and
#       scripts/windows-check.sh, only after a full pass on a clean tree.
#
# A stamp is the file <git common dir>/nvrview-checks/<tree hash>.mac or
# .windows (inside the git directory, so never committed). It is keyed by the
# tree hash, not the commit, so an amended or rebased commit with the same
# content keeps its stamps, and any change to the content invalidates them.
set -eu
cd "$(git rev-parse --show-toplevel)"

tree=$(git rev-parse 'HEAD^{tree}')
dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)/nvrview-checks"

if [ "${1:-}" = "--record" ]; then
  kind="${2:?usage: $0 --record mac|windows SECONDS STEPS}"
  case "$kind" in mac | windows) ;; *) echo "unknown stamp kind: $kind" >&2; exit 2 ;; esac
  mkdir -p "$dir"
  {
    printf 'date=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'duration=%ss\n' "${3:-unknown}"
    printf 'commit=%s\n' "$(git rev-parse HEAD)"
    printf 'tree=%s\n' "$tree"
    printf 'steps=%s\n' "${4:-}"
  } >"$dir/$tree.$kind"
  echo "check stamp written: $dir/$tree.$kind"
  exit 0
fi

want_mac=0
want_windows=0
for arg in "$@"; do
  case "$arg" in
    --mac) want_mac=1 ;;
    --windows) want_windows=1 ;;
    *) echo "usage: $0 [--mac] [--windows] | --record mac|windows SECONDS STEPS" >&2; exit 2 ;;
  esac
done
if [ "$want_mac" = 0 ] && [ "$want_windows" = 0 ]; then
  want_mac=1
  want_windows=1
fi

missing=0
report() {
  kind="$1"
  if [ -f "$dir/$tree.$kind" ]; then
    echo "$kind: checked ($(sed -n 's/^date=//p' "$dir/$tree.$kind"), $(sed -n 's/^duration=//p' "$dir/$tree.$kind"))"
  else
    echo "$kind: NOT checked for tree $tree" >&2
    missing=1
  fi
}
if [ "$want_mac" = 1 ]; then report mac; fi
if [ "$want_windows" = 1 ]; then report windows; fi
if [ "$missing" = 1 ]; then
  echo "run scripts/check.sh (mac) and NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh (windows) on a clean tree at HEAD" >&2
  exit 1
fi
