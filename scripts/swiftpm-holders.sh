#!/bin/sh
# Names the processes that may hold SwiftPM's lock on a package directory, so a
# check does not silently wait on "Another instance of SwiftPM is already
# running" for its whole time limit.
#
#   scripts/swiftpm-holders.sh [PACKAGE_DIR]     (default: apps/macos)
#
# A holder is a SwiftPM or test process (swift-build, swift-test, swift-package,
# swiftpm-testing-helper, xctest, swift-frontend) whose working directory is the
# package directory. Prints one line per holder: "PID PPID AGE COMMAND". It marks
# an orphan (parent 1, left behind by a check that timed out) as "stale".
#
# Exit status: 0 no holder, 3 a stale holder (the check should stop and say so),
# 4 a live holder (another check is using the package; SwiftPM will wait for it).
set -u
here=$(cd "$(dirname "$0")" && pwd)
dir=$(cd "${1:-$here/../apps/macos}" && pwd -P)
# NVRVIEW_FAKE_PS lets the script tests feed a process table: "PID PPID AGE COMMAND".
if [ -n "${NVRVIEW_FAKE_PS:-}" ]; then
  table=$(cat "$NVRVIEW_FAKE_PS")
else
  # Processes with this working directory (lsof prints p<pid> then n<path>).
  pids=$(lsof -nP -d cwd -Fpn 2>/dev/null | awk -v d="$dir" '/^p/ { p = substr($0, 2) } /^n/ && substr($0, 2) == d { print p }')
  table=""
  for p in $pids; do
    row=$(ps -o pid=,ppid=,etime=,command= -p "$p" 2>/dev/null) || continue
    [ -n "$row" ] && table="$table
$row"
  done
fi
status=0
printf '%s\n' "$table" | while read -r pid ppid age command; do
  [ -n "$pid" ] || continue
  case "$command" in
    *swift-build*|*swift-test*|*swift-package*|*swiftpm-testing-helper*|*xctest*|*swift-frontend*|*swift\ test*|*swift\ build*) ;;
    *) continue ;;
  esac
  [ "$pid" != "$$" ] || continue
  if [ "$ppid" = 1 ]; then kind=stale; else kind=live; fi
  echo "$kind $pid $ppid $age $command"
done >"${TMPDIR:-/tmp}/swiftpm-holders.$$"
out="${TMPDIR:-/tmp}/swiftpm-holders.$$"
cat "$out"
if grep -q '^stale ' "$out"; then status=3; elif [ -s "$out" ]; then status=4; fi
rm -f "$out"
exit "$status"
