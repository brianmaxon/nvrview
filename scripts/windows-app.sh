#!/usr/bin/env bash
# Builds the Windows app (apps/windows/NvrView.App) and runs its self-test,
# on a Windows computer under Git Bash. Windows only.
#
#   scripts/windows-app.sh build              builds nvrview.dll, mock-nvr, and the app
#   scripts/windows-app.sh selftest [SECONDS] [APP OPTIONS...]
#                                             runs `NvrView.App.exe --selftest` against a
#                                             mock-nvr the app starts, in the logged-in
#                                             user's desktop session
#                                             (--areas a,b among the options runs only those self-test
#                                             areas plus startup; see scripts/windows-selftest-areas.txt)
#   scripts/windows-app.sh startup [SECONDS]  runs `NvrView.App.exe --startup-test`: the plain
#                                             start (as a double-click) on an empty data
#                                             directory, idling SECONDS (default 5)
#   scripts/windows-app.sh publish            the trimmed release layout in target/dotnet/publish (startup, selftest, and
#                                             update run against it with NVRVIEW_APP_PUBLISHED=1)
#   scripts/windows-app.sh mark-tested        after the publish-based tests passed: records the publish output as tested
#                                             (needs NVRVIEW_TREE, the tree being checked); `package` then reuses it
#   scripts/windows-app.sh package            builds (or reuses the tested publish output), then zips the app as target/dotnet/package/NvrView-<version>-win-x64.zip
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
  write_version
  ls -l "$exe"
}

# The app's version is not compiled in (apps/windows/Directory.Build.props): it
# reads version.txt next to the executable. Written here from VERSION after a
# build, and again at packaging, so a version bump changes this one file and
# nothing is rebuilt. The tested-build fingerprint leaves the file out.
write_version() {
  tr -d '[:space:]' <VERSION >"$(dirname "$exe")/version.txt"
}

# The release layout: the trimmed, self-contained publish output without
# debug symbols, with nvrview.dll next to the executable.
publish() {
  cargo build --release -p viewer-ffi -p mock-nvr
  rm -rf "$out/publish" "$out/publish.pending" "$out/publish.tested"
  dotnet publish apps/windows/NvrView.App -c Release --artifacts-path "$out" -v quiet -clp:ErrorsOnly -nologo \
    -p:DebugType=none -p:DebugSymbols=false
  cp target/release/nvrview.dll "$(dirname "$exe")/"
  write_version
  kept_whole
  find "$(dirname "$exe")" -type f | wc -l
  du -sk "$(dirname "$exe")"
  ls -l "$exe"
  # Fingerprint of what was just built, for mark-tested and package. Only for
  # a known tree (scripts/windows-check.sh passes NVRVIEW_TREE; "dirty" or
  # unset means nothing is ever reused).
  if tree_known; then
    publish_fingerprint >"$out/publish.pending"
  fi
}

# The app's own assemblies are trimmer roots (NvrView.App.csproj), so nothing
# they reflect on can be removed. The linker rewrites them (the bytes differ
# from the compiled files), but a whole assembly keeps about its size while a
# trimmed one shrinks a lot: each must keep at least 90% of its compiled size.
# A static check, so the trimmed build needs only a short run of its own
# (`selftest ... --smoke`).
kept_whole() {
  local dir name built size_built size_out status=0
  dir=$(dirname "$exe")
  for name in NvrView.App NvrView.Interop NvrView.Video NvrView.Tiles NvrView.Shell; do
    built=$(find "$out/obj/$name" -path '*release*' -name "$name.dll" -not -path '*/ref/*' -not -path '*/refint/*' 2>/dev/null | head -1)
    if [ -z "$built" ] || [ ! -f "$dir/$name.dll" ]; then
      echo "trim check: $name.dll is missing from the build or the publish output" >&2
      status=1
      continue
    fi
    size_built=$(wc -c <"$built")
    size_out=$(wc -c <"$dir/$name.dll")
    echo "trim check: $name.dll $size_built bytes compiled, $size_out published"
    if [ $((size_out * 10)) -lt $((size_built * 9)) ]; then
      echo "trim check: $name.dll lost more than 10% in the publish output (trimmed?)" >&2
      status=1
    fi
  done
  return "$status"
}

tree_known() {
  case "${NVRVIEW_TREE:-}" in '' | dirty) return 1 ;; esac
  return 0
}

# The tree line (the tree given, default the one being checked), then one
# "sha256  ./path" line per file of the publish output, in a fixed order.
# version.txt is left out: it is the one file a version bump changes.
publish_fingerprint() { # [TREE]
  printf 'tree=%s\n' "${1:-$NVRVIEW_TREE}"
  (cd "$(dirname "$exe")" && find . -type f ! -path ./version.txt -print0 | LC_ALL=C sort -z | xargs -0 sha256sum)
}

# Called by the worker after every test on the publish output passed: the
# output must still be exactly what publish wrote (the tests changed nothing),
# and is then the tested build that `package` reuses.
mark_tested() {
  exe="$out/publish/NvrView.App/release_win-x64/NvrView.App.exe"
  if ! tree_known || [ ! -f "$out/publish.pending" ]; then
    echo "mark-tested: no fingerprint for the publish output (unknown tree); it will be rebuilt for a package"
    return 0
  fi
  if [ "$(cat "$out/publish.pending")" = "$(publish_fingerprint)" ]; then
    mv "$out/publish.pending" "$out/publish.tested"
    echo "publish output recorded as tested for tree $NVRVIEW_TREE"
    keep_tested_publish
  else
    rm -f "$out/publish.pending"
    echo "mark-tested: the publish output changed during the tests; it will be rebuilt for a package"
  fi
}

# True when the publish output on disk is, file by file, the build that passed
# the tests for the tree being packaged, or for the tree NVRVIEW_VERSION_ONLY_OF
# names (scripts/windows-check.sh passes it when the tree being packaged
# differs from a tested one in VERSION alone; the version is not compiled in,
# so the build is the same).
tested_publish_reusable() {
  local tested
  tree_known && [ -f "$out/publish.tested" ] && [ -f "$exe" ] || return 1
  tested=$(sed -n 's/^tree=//p' "$out/publish.tested")
  if [ "$tested" != "$NVRVIEW_TREE" ] && [ "$tested" != "${NVRVIEW_VERSION_ONLY_OF:-}" ]; then return 1; fi
  [ "$(cat "$out/publish.tested")" = "$(publish_fingerprint "$tested")" ]
}

# Where tested publish outputs are kept: outside every worker folder, so
# deleting a finished worker (windows-check.sh --clean, --remove-worker) does
# not take the build a release would package. The newest 3 trees are kept
# (scripts/tested-builds-lib.sh); NVRVIEW_TESTED_STORE overrides the place.
tested_store() { echo "${NVRVIEW_TESTED_STORE:-$HOME/nvrview/tested}"; }

# Copies the publish output that just passed the tests, with its fingerprint,
# to the store.
keep_tested_publish() {
  . scripts/tested-builds-lib.sh
  if tb_save "$(tested_store)" "$NVRVIEW_TREE" "$out/publish" "$out/publish.tested"; then
    echo "publish output kept for tree $NVRVIEW_TREE in $(tested_store)"
  else
    echo "mark-tested: could not keep the publish output; a package will rebuild it if this worker is removed"
  fi
}

# When the worker has no usable tested publish output (a new worker, after the
# old one was deleted), restores the kept one for the tree being packaged or
# its VERSION-only ancestor. The fingerprint is checked afterwards by
# tested_publish_reusable, as for a local one.
restore_tested_publish() {
  local cand store
  tree_known || return 1
  . scripts/tested-builds-lib.sh
  store=$(tested_store)
  for cand in "$NVRVIEW_TREE" "${NVRVIEW_VERSION_ONLY_OF:-}"; do
    [ -n "$cand" ] && [ -f "$store/$cand/publish.tested" ] && [ -d "$store/$cand/publish" ] || continue
    echo "restoring the tested publish output kept for tree $cand"
    rm -rf "$out/publish" "$out/publish.tested" "$out/publish.pending"
    cp -Rp "$store/$cand/publish" "$out/publish"
    cp -p "$store/$cand/publish.tested" "$out/publish.tested"
    if tested_publish_reusable; then tb_touch "$store" "$cand"; return 0; fi
    echo "the kept publish output does not match its fingerprint; ignoring it"
    rm -rf "$out/publish" "$out/publish.tested"
  done
  return 1
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
  if tested_publish_reusable || restore_tested_publish; then
    echo "reusing the publish output that passed the tests for tree $NVRVIEW_TREE or its version-only ancestor ${NVRVIEW_VERSION_ONLY_OF:-none} (hashes of all files match); only version.txt is written"
  else
    echo "no matching tested publish output; building it"
    publish
  fi
  write_version
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
  write_version
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
  mark-tested) mark_tested ;;
  update) update ;;
  selftest) shift; selftest "$@" ;;
  startup) shift; startup "$@" ;;
  *)
    echo "usage: $0 build | publish | mark-tested | package | selftest [SECONDS] [APP OPTIONS...] | startup [SECONDS] | update" >&2
    exit 2
    ;;
esac
