#!/bin/sh
# Quits and reopens the development build, build/NVRView.app.
#
#   scripts/restart-macos-app.sh [--build] [--wait SECONDS] [-- APP ARGS...]
#
#   --build          rebuild it first (scripts/package-macos.sh)
#   --wait SECONDS   how long to wait for it to quit (default 30)
#   -- APP ARGS      arguments for the relaunched app (for example --fake ...)
#
# Only the copy at build/NVRView.app is touched: an installed release has the
# same bundle identifier, so the running copy is found by its executable path,
# not by name. It is asked to quit the normal way (as with Command-Q), so it
# closes its device sessions; NVRs allow only a few logins. If it is still
# running after the wait, the script stops and says so rather than killing it.
set -eu
cd "$(git rev-parse --show-toplevel)"
app="$PWD/build/NVRView.app"
exe="$app/Contents/MacOS/NVRView"
build=0
wait_s=30
while [ $# -gt 0 ]; do
  case "$1" in
    --build) build=1; shift ;;
    --wait) wait_s="${2:?--wait needs seconds}"; shift 2 ;;
    --) shift; break ;;
    *) echo "usage: $0 [--build] [--wait SECONDS] [-- APP ARGS...]" >&2; exit 2 ;;
  esac
done

running() { pgrep -f "^$exe( |\$)" || true; }

pids=$(running)
for pid in $pids; do
  echo "quitting NVRView (pid $pid)"
  # A quit event to that one process runs the app's normal shutdown.
  osascript -l JavaScript -e "ObjC.import('AppKit'); \$.NSRunningApplication.runningApplicationWithProcessIdentifier($pid).terminate" >/dev/null
done
if [ -n "$pids" ]; then
  waited=0
  while [ -n "$(running)" ]; do
    if [ "$waited" -ge "$wait_s" ]; then
      echo "NVRView is still running after ${wait_s}s (pid $(running | tr '\n' ' ')); not killing it." >&2
      echo "Quit it from its window (it may be showing a dialog), or wait longer with --wait." >&2
      exit 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
fi

if [ "$build" = 1 ]; then
  scripts/package-macos.sh
fi
[ -x "$exe" ] || { echo "no app at $app: run scripts/package-macos.sh (or pass --build)" >&2; exit 1; }

echo "opening $app"
# -n: this copy, even if an installed release with the same identifier runs.
if [ $# -gt 0 ]; then
  open -n "$app" --args "$@"
else
  open -n "$app"
fi
