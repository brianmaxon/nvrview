#!/bin/sh
# Runs the Mac check (scripts/check.sh) and the Windows check
# (NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh) at the same time, prints
# both summaries, and fails if either failed. On a clean committed tree each
# leaves its check stamp (scripts/checked.sh), so the two together are what
# scripts/release.sh needs. Takes the Windows lock (scripts/windows-lock.sh,
# priority merge) for the Windows half.
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> [NVRVIEW_WINDOWS_WORKER=<name>] \
#     scripts/release-check.sh
set -eu
cd "$(git rev-parse --show-toplevel)"
# Release checks are full runs: nothing comes from the test result cache
# (scripts/suite-cache.py), so the stamps they write count for a release.
export NVRVIEW_FULL_CHECK=1

. scripts/stamps-lib.sh
[ -n "$(windows_hosts)" ] || { echo "set NVRVIEW_WINDOWS_HOST (or NVRVIEW_WINDOWS_HOSTS) to the SSH host alias of the Windows computer" >&2; exit 2; }
# Releases get the Windows computer before agents' checks.
NVRVIEW_WINDOWS_PRIORITY=merge
export NVRVIEW_WINDOWS_PRIORITY
[ -z "$(git status --porcelain)" ] || echo "release-check: the working tree is not clean, so no stamps will be written" >&2

logs=$(mktemp -d)
trap 'rm -rf "$logs"' EXIT
start=$(date +%s)

# The Windows half runs in a subshell so its lock is held only for its run.
(
  . scripts/stamps-lib.sh
  trap windows_unlock EXIT
  windows_lock
  NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh
) >"$logs/windows.log" 2>&1 &
win_pid=$!
scripts/check.sh >"$logs/mac.log" 2>&1 &
mac_pid=$!

mac_rc=0
win_rc=0
wait "$mac_pid" || mac_rc=$?
wait "$win_pid" || win_rc=$?

summary() { # NAME LOG RC
  if [ "$3" = 0 ]; then
    echo "== $1: PASS"
    tail -n 4 "$2"
  else
    echo "== $1: FAIL (exit $3)"
    grep -E 'FAIL|error|panicked' "$2" | tail -n 8 || true
    tail -n 12 "$2"
  fi
}
summary "Mac check" "$logs/mac.log" "$mac_rc"
summary "Windows check" "$logs/windows.log" "$win_rc"
echo
echo "both checks took $(($(date +%s) - start)) s in parallel."
report_timings release-check "$(($(date +%s) - start))" "$(git rev-parse 'HEAD^{tree}')" "$logs/mac.log" "$logs/windows.log"
if [ "$mac_rc" != 0 ] || [ "$win_rc" != 0 ]; then
  keep="${TMPDIR:-/tmp}/release-check-logs.$$"
  mkdir -p "$keep"
  cp "$logs"/*.log "$keep"/
  echo "release-check: FAILED; full logs in $keep" >&2
  exit 1
fi
scripts/checked.sh || echo "release-check: passed, but a stamp is missing (was the tree clean?)" >&2
