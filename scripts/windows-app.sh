#!/usr/bin/env bash
# Builds the Windows app (apps/windows/NvrView.App) and runs its self-test,
# on a Windows computer under Git Bash. Windows only.
#
#   scripts/windows-app.sh build              builds nvrview.dll, mock-nvr, and the app
#   scripts/windows-app.sh selftest [SECONDS] [APP OPTIONS...]
#                                             runs `NvrView.App.exe --selftest` against a
#                                             mock-nvr the app starts, in the logged-in
#                                             user's desktop session
#
# The app is a GUI program and an SSH session has no desktop, so the
# self-test runs as a one-time scheduled task that runs only while the user
# is logged on (interactive), started at once and deleted afterwards. Its
# window opens behind every other window and never takes focus. The task
# writes its verdict to a log file in the build directory, which this script
# waits for and prints. Output lives in target/dotnet (kept between runs by
# scripts/windows-check.sh, so builds are incremental).
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
out=target/dotnet
exe="$out/bin/NvrView.App/release_win-x64/NvrView.App.exe"

build() {
  cargo build --release -p viewer-ffi -p mock-nvr
  dotnet build apps/windows/NvrView.App -c Release --artifacts-path "$out" -v quiet -clp:ErrorsOnly -nologo
  # The app finds nvrview.dll next to itself.
  cp target/release/nvrview.dll "$(dirname "$exe")/"
  ls -l "$exe"
}

selftest() {
  local seconds="${1:-10}"
  shift || true
  local dir="$out/selftest"
  mkdir -p "$dir"
  rm -f "$dir/selftest.log" "$dir/output.txt"
  local win_exe win_mock win_log win_out win_cmd
  win_exe="$(cygpath -w "$PWD/$exe")"
  win_mock="$(cygpath -w "$PWD/target/release/mock-nvr.exe")"
  win_log="$(cygpath -w "$PWD/$dir/selftest.log")"
  win_out="$(cygpath -w "$PWD/$dir/output.txt")"
  win_cmd="$(cygpath -w "$PWD/$dir/selftest.cmd")"
  # Task names are per worker folder, so parallel workers never collide.
  local task
  task="nvrview-$(basename "$(dirname "$PWD")")-selftest"
  {
    printf '@echo off\r\n'
    printf 'cd /d "%s"\r\n' "$(cygpath -w "$PWD")"
    printf 'set NVRVIEW_TEST_PASSWORD=example-password\r\n'
    printf '"%s" --mock-nvr "%s" --selftest %s --log "%s"' "$win_exe" "$win_mock" "$seconds" "$win_log"
    for a in "$@"; do printf ' %s' "$a"; done
    printf ' > "%s" 2>&1\r\n' "$win_out"
  } >"$dir/selftest.cmd"
  MSYS_NO_PATHCONV=1 schtasks /create /tn "$task" /tr "\"$win_cmd\"" /sc once /st 00:00 /it /f >/dev/null 2>&1 ||
    { echo "could not create the scheduled task $task" >&2; return 1; }
  MSYS_NO_PATHCONV=1 schtasks /run /tn "$task" >/dev/null
  local limit=$((seconds + 150)) waited=0
  while [ ! -s "$dir/selftest.log" ] || ! grep -q '^selftest: ' "$dir/selftest.log"; do
    sleep 2
    waited=$((waited + 2))
    if [ "$waited" -ge "$limit" ]; then
      break
    fi
  done
  MSYS_NO_PATHCONV=1 schtasks /delete /tn "$task" /f >/dev/null 2>&1 || true
  if [ ! -s "$dir/selftest.log" ]; then
    echo "the self-test wrote no verdict within $limit s (is the user logged on?)" >&2
    cat "$dir/output.txt" 2>/dev/null || true
    return 1
  fi
  cat "$dir/selftest.log"
  grep -q '^selftest: PASS' "$dir/selftest.log"
}

case "${1:-}" in
  build) build ;;
  selftest) shift; selftest "$@" ;;
  *)
    echo "usage: $0 build | selftest [SECONDS] [APP OPTIONS...]" >&2
    exit 2
    ;;
esac
