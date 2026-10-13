#!/bin/sh
# Runs the Windows checks (scripts/windows-worker.sh) on a local Windows
# computer over SSH, against this working tree as it is now (committed or
# not), so Windows results do not need a push. See scripts/setup-windows.ps1
# for preparing that computer (toolchain, key-only SSH).
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> [NVRVIEW_WINDOWS_WORKER=<name>] \
#     scripts/windows-check.sh [--lock-held]
#
# The script takes the Windows lock itself (scripts/windows-lock.sh; waiting
# in this one command, agents behind any waiting merge) and gives it back on
# every exit, so do not take it by hand. NVRVIEW_WINDOWS_HOSTS="host-a host-b"
# names a pool: the first free host runs the check. --lock-held (or
# NVRVIEW_WINDOWS_LOCK_HELD=1) says the caller already holds it, with
# NVRVIEW_WINDOWS_HOST set to the host it got (merge scripts do this).
#
# A finished worker's folder, its target\ build output included, is deleted with
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> scripts/windows-check.sh --clean <name>
#
# Each run prints the Windows computer's free disk space first and warns
# under 10 GB, then the time of every phase and step, and ends with a table
# of the suites that ran and those taken from the test result cache
# (scripts/suite-cache.py; NVRVIEW_FULL_CHECK=1 runs everything).
#
# Everything lives under %USERPROFILE%\nvrview on the Windows side (the
# setup script's clone is %USERPROFILE%\nvrview\repo). Each worker name (default
# "main"; give parallel agents their own) gets %USERPROFILE%\nvrview\worker\<name>:
# its src\ is unpacked fresh every run (tracked files plus untracked files that
# are not ignored; never captures/ or other ignored files), while its target\
# is kept between runs so builds are incremental. Never move or rename a
# worker folder: test binaries embed their source path, and Cargo would reuse
# them; delete its target\ instead.
set -eu
cd "$(git rev-parse --show-toplevel)"

. scripts/stamps-lib.sh
. scripts/suite-cache-lib.sh
[ -n "$(windows_hosts)" ] || { echo "set NVRVIEW_WINDOWS_HOST (or NVRVIEW_WINDOWS_HOSTS) to the SSH host alias of the Windows computer" >&2; exit 2; }
clean=0
package_to=""
if [ "${1:-}" = "--lock-held" ]; then
  # The caller holds the Windows lock (scripts/windows-lock.sh) and has set
  # NVRVIEW_WINDOWS_HOST to the host it got.
  NVRVIEW_WINDOWS_LOCK_HELD=1
  export NVRVIEW_WINDOWS_LOCK_HELD
  shift
fi
defaults_args=""
release_notes=""
notes_history=""
if [ "${1:-}" = "--clean" ]; then
  clean=1
  NVRVIEW_WINDOWS_WORKER="${2:?usage: $0 --clean <worker name>}"
elif [ "${1:-}" = "--package" ]; then
  # Builds the release app on the Windows computer instead of running the
  # checks, and fetches NvrView-<version>-win-x64.zip into the directory.
  # Used by scripts/release.sh.
  package_to="${2:?usage: $0 --package <directory> [--trusted-key-file FILE]... [--update-source SOURCE]}"
  shift 2
  # Bundled update defaults (docs/UPDATES.md, "Bundled defaults"): validated
  # now, before the build, and added to the fetched zip as update-defaults.json
  # at its top, next to the executable. Never committed, never sent to the
  # Windows side. Kept as one argument per line.
  while [ $# -gt 0 ]; do
    case "$1" in
      --trusted-key-file | --update-source)
        [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; }
        defaults_args="$defaults_args
$1
$2"
        shift 2 ;;
      --release-notes)
        [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; }
        release_notes="$2"; shift 2 ;;
      --notes-history)
        [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; }
        notes_history="$2"; shift 2 ;;
      *) echo "usage: $0 --package <directory> [--trusted-key-file FILE]... [--update-source SOURCE] [--release-notes FILE [--notes-history DIR]]" >&2; exit 2 ;;
    esac
  done
  if [ -n "$defaults_args" ]; then
    oldifs=$IFS
    IFS='
'
    # shellcheck disable=SC2086 # split on newlines only
    "$py" -I scripts/update-defaults.py $defaults_args --out /dev/null >/dev/null
    IFS=$oldifs
  fi
  if [ -n "$release_notes" ]; then
    # Validated now, before the build; added to the zip below as release-notes.json.
    "$py" -I scripts/release-notes-bundle.py --version "$(tr -d '[:space:]' <VERSION)" --notes "$release_notes" \
      ${notes_history:+--history "$notes_history"} --out /dev/null >/dev/null
  fi
elif [ $# -gt 0 ]; then
  echo "usage: $0 [--clean <worker name> | --package <directory> [--trusted-key-file FILE]... [--update-source SOURCE]]" >&2
  exit 2
fi
if [ -n "$package_to" ] && [ -z "${NVRVIEW_WINDOWS_WORKER:-}" ]; then
  # Package from the worker whose full check stamped HEAD's tree, so its
  # tested publish output can be reused (the stamp's steps end "worker <name>").
  stamp="$(cd "$(git rev-parse --git-common-dir)" && pwd)/nvrview-checks/$(git rev-parse 'HEAD^{tree}').windows"
  if [ -f "$stamp" ]; then
    NVRVIEW_WINDOWS_WORKER=$(sed -n 's/^steps=.*worker \([A-Za-z0-9_-]*\)$/\1/p' "$stamp")
  fi
fi
worker="${NVRVIEW_WINDOWS_WORKER:-main}"
case "$worker" in
  *[!A-Za-z0-9_-]* | '') echo "NVRVIEW_WINDOWS_WORKER: letters, digits, - and _ only" >&2; exit 2 ;;
esac
# The lock (scripts/windows-lock.sh): taken here unless the caller holds it,
# at priority $NVRVIEW_WINDOWS_PRIORITY (agent by default; merges and releases
# use merge). It picks the first free host of NVRVIEW_WINDOWS_HOSTS. --clean
# touches only a worker's folder and needs no lock.
if [ "$clean" = 1 ]; then
  host="${NVRVIEW_WINDOWS_HOST:-$(windows_hosts | head -n 1)}"
else
  trap windows_unlock EXIT
  windows_lock
  host="${NVRVIEW_WINDOWS_HOST:-$(windows_hosts | head -n 1)}"
fi
ssh_opts="-o BatchMode=yes -o ConnectTimeout=10"
gitbash='"C:\Program Files\Git\bin\bash.exe"'

# shellcheck disable=SC2086 # ssh_opts is a list of options
ssh $ssh_opts "$host" "echo connected" >/dev/null

# Free space on the drive holding %USERPROFILE%, in whole GB.
free_gb() {
  # shellcheck disable=SC2086
  ssh $ssh_opts "$host" 'powershell -NoProfile -Command "[math]::Floor((Get-PSDrive ($env:USERPROFILE.Substring(0,1))).Free / 1GB)"' |
    tr -d '\r' | grep -E '^[0-9]+$' | tail -n 1
}

if [ "$clean" = 1 ]; then
  echo "deleting worker $worker on $host"
  # The junction goes first, with a plain rmdir, so deleting src never
  # reaches into target through it; then the folder with everything in it.
  # shellcheck disable=SC2086,SC2029
  ssh $ssh_opts "$host" "cd /d %USERPROFILE% && (if exist nvrview\\worker\\$worker\\src\\target rmdir nvrview\\worker\\$worker\\src\\target) && (if exist nvrview\\worker\\$worker rmdir /s /q nvrview\\worker\\$worker) && echo deleted"
  echo "free disk space on $host: $(free_gb || echo unknown) GB"
  exit 0
fi

check_start=$(date +%s)
free=$(free_gb || true)
echo "free disk space on $host: ${free:-unknown} GB"
if [ -n "$free" ] && [ "$free" -lt 10 ]; then
  echo "WARNING: under 10 GB free; each worker keeps about 5 GB of build output." >&2
  echo "         Delete finished workers with: $0 --clean <name>" >&2
fi

tree_sent=$(git rev-parse 'HEAD^{tree}')
if [ -n "$(git status --porcelain)" ]; then tree_sent="dirty"; fi
echo "sending the working tree to $host (worker $worker)"
# cmd.exe on the far side: recreate src, keep target (a directory junction
# inside src points at it; the junction alone is removed first, with a plain
# rmdir, so deleting src never reaches into target), unpack from stdin with
# Windows' own tar.
# $worker is meant to expand here, on this side.
# shellcheck disable=SC2086,SC2029
# COPYFILE_DISABLE and --no-mac-metadata keep macOS from adding ._* files
# (MSBuild took one for a second project file).
# Untracked files go too (work not yet committed), but never build output or
# other checkouts nested in this one.
git ls-files -z --cached --others --exclude-standard -- . \
  ':(exclude,glob)**/target/**' ':(exclude).claude/worktrees' ':(exclude).worktrees' |
  COPYFILE_DISABLE=1 tar --no-mac-metadata --null -T - -cf - |
  ssh $ssh_opts "$host" "cd /d %USERPROFILE% && (if not exist nvrview\\worker\\$worker mkdir nvrview\\worker\\$worker) && cd nvrview\\worker\\$worker && (if not exist target mkdir target) && (if exist src\target rmdir src\target) && (if exist src rmdir /s /q src) && mkdir src && C:\Windows\System32\tar.exe -xf - -C src && mklink /J src\\target target >nul"

sent=$(date +%s)
printf '   (sync: %ss)\n' "$((sent - check_start))"

if [ -n "$package_to" ]; then
  version=$(tr -d '[:space:]' <VERSION)
  zip="NvrView-$version-win-x64.zip"
  echo "building the Windows app package on $host"
  # shellcheck disable=SC2086,SC2029
  ssh $ssh_opts "$host" "$gitbash -lc \"cd ~/nvrview/worker/$worker/src && NVRVIEW_TREE=$tree_sent scripts/windows-app.sh package\""
  mkdir -p "$package_to"
  # scp reads the Windows path relative to the home directory.
  # shellcheck disable=SC2086
  scp $ssh_opts "$host:nvrview/worker/$worker/src/target/dotnet/package/$zip" "$package_to/$zip"
  if [ -n "$defaults_args" ]; then
    oldifs=$IFS; IFS='
'
    # shellcheck disable=SC2086
    "$py" -I scripts/update-defaults.py $defaults_args --add-to-zip "$package_to/$zip"
    IFS=$oldifs
  fi
  if [ -n "$release_notes" ]; then
    "$py" -I scripts/release-notes-bundle.py --version "$version" --notes "$release_notes" \
      ${notes_history:+--history "$notes_history"} --add-to-zip "$package_to/$zip"
  fi
  ls -l "$package_to/$zip"
  exit 0
fi

# Test result cache (scripts/suite-cache.py; it lives on this Mac): the
# suites whose inputs have a clean Windows pass within the last 72 hours are
# skipped, the rest are passed to the worker, and the worker's
# "suite-result NAME SECONDS" lines are recorded. Full runs as for the Mac
# check (NVRVIEW_FULL_CHECK=1, no full Windows pass in NVRVIEW_FULL_MAX_AGE_H
# hours, NVRVIEW_FULL_EVERY). NVRVIEW_RETRIED=1 (a retry after a known flake)
# records passes that never let a suite be skipped.
logs=$(mktemp -d)
run_file=""
finish_cache() {
  if [ -n "$run_file" ]; then sc finish "$run_file" "$@" || true; run_file=""; fi
}
trap 'finish_cache --failed "Windows worker"; rm -rf "$logs"; windows_unlock' EXIT
# The worker's tool versions are part of every Windows fingerprint.
# shellcheck disable=SC2086,SC2029
ssh $ssh_opts "$host" "$gitbash -lc \"rustc -vV; cargo nextest --version; cargo clippy --version; dotnet --version\"" \
  </dev/null | tr -d '\r' >"$logs/tools"
cache_start windows --tools-file "$logs/tools" ${NVRVIEW_RETRIED:+--retried}
only="${NVRVIEW_WINDOWS_ONLY:-}"
# Exported only so that they count as used; they travel in $vars below.
export NVRVIEW_CLIPPY_CRATES=none NVRVIEW_TEST_CRATES=none NVRVIEW_DOCTEST_CRATES=none
if [ -z "$only" ]; then
  NVRVIEW_CLIPPY_CRATES=$(cache_crates clippy list)
  NVRVIEW_TEST_CRATES=$(cache_crates test list)
  NVRVIEW_DOCTEST_CRATES=$(cache_crates doctest list)
fi
NVRVIEW_SUITES=""
wanted="shell-tests"
if [ "$only" != app ]; then wanted="csharp-smoke tiles-tests video-selftest $wanted"; fi
if [ "${NVRVIEW_APP_SELFTEST:-}" = 1 ]; then
  # The app self-test is cached by area (scripts/windows-selftest-areas.txt), for the debug and the publish build.
  # NVRVIEW_APP_AREAS=a,b asks for those areas only (a partial run: no stamp, not a full pass).
  for a in $(cache_areas); do
    case ",${NVRVIEW_APP_AREAS:-$a}," in *",$a,"*) wanted="$wanted app-debug.$a app-publish.$a" ;; esac
  done
  wanted="$wanted app-update"
fi
for s in $wanted; do
  if cache_want "$s"; then NVRVIEW_SUITES="$NVRVIEW_SUITES${NVRVIEW_SUITES:+,}$s"; fi
done
NVRVIEW_SUITES=${NVRVIEW_SUITES:-none}

echo "running the Windows checks on $host"
# cmd.exe does not treat single quotes as quoting: double-quote bash's script.
# The worker's options travel as variables set in that script.
vars=""
for v in NVRVIEW_APP_SELFTEST NVRVIEW_APP_SELFTEST_SECONDS NVRVIEW_WINDOWS_ONLY NVRVIEW_MIC_TEST \
  NVRVIEW_SUITES NVRVIEW_CLIPPY_CRATES NVRVIEW_TEST_CRATES NVRVIEW_DOCTEST_CRATES; do
  eval "val=\${$v:-}"
  case "$val" in
    *[!A-Za-z0-9_.,-]*) echo "$v: letters, digits, '.', ',', '-' and '_' only" >&2; exit 2 ;;
  esac
  if [ -n "$val" ]; then
    vars="$vars $v=$val"
  fi
done
vars="$vars NVRVIEW_TREE=$tree_sent"
# $gitbash and $vars are meant to expand here, on this side. The output goes
# to the terminal and to a log, from which the suite results are read.
# shellcheck disable=SC2086,SC2029
{
  if ssh $ssh_opts "$host" "$gitbash -lc \"cd ~/nvrview/worker/$worker/src &&$vars scripts/windows-worker.sh\"" 2>&1; then
    echo 0 >"$logs/rc"
  else
    echo "$?" >"$logs/rc"
  fi
} | tee "$logs/worker.log"
rc=$(cat "$logs/rc")
tr -d '\r' <"$logs/worker.log" | sed -n 's/^suite-result \([A-Za-z0-9_.-]*\) \([0-9]*\)$/\1 \2/p' >"$logs/results"
while read -r name secs; do
  cache_pass "$secs" "$name"
done <"$logs/results"
if [ "$rc" != 0 ]; then
  exit "$rc"
fi
partial=""
if [ "${NVRVIEW_APP_SELFTEST:-}" != 1 ] || [ -n "$only" ] || [ -n "${NVRVIEW_APP_AREAS:-}" ]; then partial=--partial; fi
finish_cache --ok $partial | tee "$logs/summary"
cached=$(sed -n 's/.*; cached suites: //p' "$logs/summary")
now=$(date +%s)
# A pass (app self-tests included) on a clean tree, i.e. exactly the tree of
# HEAD, leaves a stamp that scripts/checked.sh --windows looks for; with
# cached suites it serves merges, and scripts/release.sh needs a full one.
if [ "${NVRVIEW_APP_SELFTEST:-}" = 1 ] && [ -z "${NVRVIEW_WINDOWS_ONLY:-}" ] && [ -z "${NVRVIEW_APP_AREAS:-}" ] &&
  [ -z "$(git status --porcelain)" ] && [ "$tree_sent" = "$(git rev-parse 'HEAD^{tree}')" ]; then
  scripts/checked.sh --record windows "$((now - check_start))" "app self-tests${NVRVIEW_MIC_TEST:+ with the real microphone}; worker $worker" "${cached:-none}"
else
  echo "no check stamp written (needs NVRVIEW_APP_SELFTEST=1, no NVRVIEW_WINDOWS_ONLY, and a clean tree)"
fi
printf '\nWindows checks on %s took %ss in all (disk check and sync %ss).\n' "$host" "$((now - check_start))" "$((sent - check_start))"
# Step times: the sync here, the worker's steps and background builds.
printf '   (sync: %ss)\n' "$((sent - check_start))" >"$logs/steps"
"$py" -I scripts/timing.py table "$logs/steps" "$logs/worker.log"
"$py" -I scripts/timing.py log windows "$((now - check_start))" --tree "$tree_sent" "$logs/steps" "$logs/worker.log" || true
