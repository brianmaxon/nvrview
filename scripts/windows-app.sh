#!/usr/bin/env bash
# Builds the Windows app (apps/windows/NvrView.App) and runs its self-test,
# on a Windows computer under Git Bash. Windows only.
#
#   scripts/windows-app.sh build              builds nvrview.dll, mock-nvr, and the app
#   scripts/windows-app.sh selftest [SECONDS] [APP OPTIONS...]
#                                             runs `NvrView.App.exe --selftest` against a
#                                             mock-nvr the app starts, in the logged-in
#                                             user's desktop session
#   scripts/windows-app.sh startup [SECONDS]  runs `NvrView.App.exe --startup-test`: the plain
#                                             start (as a double-click) on an empty data
#                                             directory, idling SECONDS (default 5)
#   scripts/windows-app.sh publish            the trimmed release layout in target/dotnet/publish (startup, selftest, and
#                                             update run against it with NVRVIEW_APP_PUBLISHED=1)
#   scripts/windows-app.sh package            builds, then zips the app as target/dotnet/package/NvrView-<version>-win-x64.zip
#   scripts/windows-app.sh update             the side-by-side update end to end
#                                             (docs/UPDATES.md): packages the built app as
#                                             NvrView-<version>-win-x64.zip, signs it with a
#                                             key made for the test, stages it with the core
#                                             (`sign-update check --stage`), moves it to
#                                             NvrView-<version>, and starts that copy with
#                                             --wait-for-pid on a running process and
#                                             --startup-test; it must wait, then pass
#
# The app is a GUI program and an SSH session has no desktop, so each test
# runs as a one-time scheduled task that runs only while the user
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
# NVRVIEW_APP_PUBLISHED=1 makes startup, selftest, update, and package use the
# trimmed `publish` output (what the release ships) instead of the build output.
if [ "${NVRVIEW_APP_PUBLISHED:-0}" = 1 ]; then
  exe="$out/publish/NvrView.App/release_win-x64/NvrView.App.exe"
fi

build() {
  cargo build --release -p viewer-ffi -p mock-nvr
  dotnet build apps/windows/NvrView.App -c Release --artifacts-path "$out" -v quiet -clp:ErrorsOnly -nologo
  # The app finds nvrview.dll next to itself.
  cp target/release/nvrview.dll "$(dirname "$exe")/"
  ls -l "$exe"
}

# The release layout: the trimmed, self-contained publish output without
# debug symbols, with nvrview.dll next to the executable.
publish() {
  cargo build --release -p viewer-ffi -p mock-nvr
  rm -rf "$out/publish"
  dotnet publish apps/windows/NvrView.App -c Release --artifacts-path "$out" -v quiet -clp:ErrorsOnly -nologo \
    -p:DebugType=none -p:DebugSymbols=false
  cp target/release/nvrview.dll "$(dirname "$exe")/"
  find "$(dirname "$exe")" -type f | wc -l
  du -sk "$(dirname "$exe")"
  ls -l "$exe"
}

# Runs a .cmd file as a one-time scheduled task in the logged-on user's
# session and waits for a verdict line in its log; prints the log.
#   run_task NAME CMDFILE LOGFILE OUTFILE LIMIT_SECONDS
run_task() {
  local name="$1" cmd="$2" log="$3" output="$4" limit="$5"
  # Task names are per worker folder, so parallel workers never collide.
  local task
  task="nvrview-$(basename "$(dirname "$PWD")")-$name"
  MSYS_NO_PATHCONV=1 schtasks /create /tn "$task" /tr "\"$(cygpath -w "$PWD/$cmd")\"" /sc once /st 00:00 /it /f >/dev/null 2>&1 ||
    { echo "could not create the scheduled task $task" >&2; return 1; }
  MSYS_NO_PATHCONV=1 schtasks /run /tn "$task" >/dev/null
  local waited=0
  while [ ! -s "$log" ] || ! grep -q '^selftest: ' "$log"; do
    sleep 2
    waited=$((waited + 2))
    if [ "$waited" -ge "$limit" ]; then
      break
    fi
  done
  MSYS_NO_PATHCONV=1 schtasks /delete /tn "$task" /f >/dev/null 2>&1 || true
  if [ ! -s "$log" ]; then
    echo "the $name run wrote no verdict within $limit s (is the user logged on?)" >&2
    cat "$output" 2>/dev/null || true
    return 1
  fi
  cat "$log"
  grep -q '^selftest: PASS' "$log"
}

selftest() {
  local seconds="${1:-10}"
  shift || true
  local dir="$out/selftest"
  mkdir -p "$dir"
  rm -f "$dir/selftest.log" "$dir/output.txt"
  local win_exe win_mock win_log win_out
  win_exe="$(cygpath -w "$PWD/$exe")"
  win_mock="$(cygpath -w "$PWD/target/release/mock-nvr.exe")"
  win_log="$(cygpath -w "$PWD/$dir/selftest.log")"
  win_out="$(cygpath -w "$PWD/$dir/output.txt")"
  {
    printf '@echo off\r\n'
    printf 'cd /d "%s"\r\n' "$(cygpath -w "$PWD")"
    printf 'set NVRVIEW_TEST_PASSWORD=example-password\r\n'
    printf '"%s" --mock-nvr "%s" --selftest %s --log "%s"' "$win_exe" "$win_mock" "$seconds" "$win_log"
    for a in "$@"; do printf ' %s' "$a"; done
    printf ' > "%s" 2>&1\r\n' "$win_out"
  } >"$dir/selftest.cmd"
  run_task selftest "$dir/selftest.cmd" "$dir/selftest.log" "$dir/output.txt" $((seconds + 150))
}

# The plain start, as a double-click makes it (no mock, no test device), on a
# fresh empty data directory (a first run), started from the executable's own
# folder as Explorer does.
startup() {
  local seconds="${1:-5}"
  local dir="${STARTUP_DIR:-$out/startup}"
  local run_exe="${STARTUP_EXE:-$PWD/$exe}"
  rm -rf "$dir"
  mkdir -p "$dir/data"
  local win_exe win_log win_out win_data
  win_exe="$(cygpath -w "$run_exe")"
  win_log="$(cygpath -w "$PWD/$dir/startup.log")"
  win_out="$(cygpath -w "$PWD/$dir/output.txt")"
  win_data="$(cygpath -w "$PWD/$dir/data")"
  {
    printf '@echo off\r\n'
    printf 'cd /d "%s"\r\n' "$(cygpath -w "$(dirname "$run_exe")")"
    printf '"%s" --startup-test %s --log "%s" --data-dir "%s"%s > "%s" 2>&1\r\n' "$win_exe" "$seconds" "$win_log" "$win_data" "${STARTUP_ARGS:-}" "$win_out"
  } >"$dir/startup.cmd"
  local status=0
  run_task startup "$dir/startup.cmd" "$dir/startup.log" "$dir/output.txt" $((seconds + 90)) || status=$?
  # A crash before the verdict leaves its reason here.
  if [ "$status" != 0 ]; then
    cat "$dir"/data/logs/crash-*.log 2>/dev/null || true
  fi
  return "$status"
}

# Zips the built app folder's contents (at the top of the archive) to the
# given path, with Windows' own tar (zip with -a).
make_zip() {
  local zip_win
  mkdir -p "$(dirname "$1")"
  rm -f "$1"
  zip_win="$(cygpath -w "$1")"
  (cd "$(dirname "$exe")" && /c/Windows/System32/tar.exe -a -c -f "$zip_win" -- *)
}

# The update package for a release: builds, then writes
# target/dotnet/package/NvrView-<version>-win-x64.zip (scripts/release.sh
# fetches it).
package() {
  local version
  version="$(tr -d '[:space:]' <VERSION)"
  NVRVIEW_APP_PUBLISHED=1
  exe="$out/publish/NvrView.App/release_win-x64/NvrView.App.exe"
  publish
  rm -rf "$out/package"
  make_zip "$PWD/$out/package/NvrView-$version-win-x64.zip"
  ls -l "$out/package"
}

# The side-by-side update, end to end (see the usage above). The app must
# have been built (`build`).
update() {
  local version dir app_dir sign pid
  version="$(tr -d '[:space:]' <VERSION)"
  dir="$out/update-test"
  app_dir="$(dirname "$exe")"
  rm -rf "$dir"
  mkdir -p "$dir/source" "$dir/keys" "$dir/Apps"
  cargo build -q -p sign-update
  sign=target/debug/sign-update.exe
  # The package: the app folder's contents at the top (Windows' own tar
  # writes zip with -a).
  make_zip "$PWD/$dir/source/NvrView-$version-win-x64.zip"
  ls -l "$dir/source"
  "$sign" keygen --secret "$dir/keys/update.key" --public "$dir/keys/update.pub" >/dev/null
  "$sign" manifest --version "$version" --notes "End-to-end test." --out "$dir/source/updates.json" "$dir/source/NvrView-$version-win-x64.zip"
  "$sign" sign --secret "$dir/keys/update.key" "$dir/source/updates.json"
  "$sign" check --source "$(cygpath -w "$PWD/$dir/source")" --key "$dir/keys/update.pub" --current 0.0.1 \
    --platform win-x64 --stage "$(cygpath -w "$PWD/$dir/Apps/.NvrView-update")"
  mv "$dir/Apps/.NvrView-update/staging-$version/content" "$dir/Apps/NvrView-$version"
  test -f "$dir/Apps/NvrView-$version/NvrView.App.exe"
  # The "previous version": a process that runs a few seconds.
  pid="$(powershell -NoProfile -Command "(Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 8' -WindowStyle Hidden -PassThru).Id" | tr -d '\r')"
  echo "previous version stand-in: process $pid"
  local status=0
  STARTUP_DIR="$dir/run" STARTUP_EXE="$PWD/$dir/Apps/NvrView-$version/NvrView.App.exe" \
    STARTUP_ARGS=" --wait-for-pid $pid" startup 3 || status=$?
  grep -h "started after an update" "$dir"/run/data/logs/*.log || { echo "the new version did not log its wait" >&2; status=1; }
  if grep -h "started after an update" "$dir"/run/data/logs/*.log | grep -q "still running"; then
    echo "the new version did not see the previous one exit" >&2
    status=1
  fi
  rm -rf "$dir"
  return "$status"
}

case "${1:-}" in
  build) build ;;
  publish) NVRVIEW_APP_PUBLISHED=1; exe="$out/publish/NvrView.App/release_win-x64/NvrView.App.exe"; publish ;;
  package) package ;;
  update) update ;;
  selftest) shift; selftest "$@" ;;
  startup) shift; startup "$@" ;;
  *)
    echo "usage: $0 build | publish | package | selftest [SECONDS] [APP OPTIONS...] | startup [SECONDS] | update" >&2
    exit 2
    ;;
esac
