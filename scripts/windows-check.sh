#!/bin/sh
# Runs the Windows checks (scripts/windows-worker.sh) on a local Windows
# computer over SSH, against this working tree as it is now (committed or
# not), so Windows results do not need a push. See scripts/setup-windows.ps1
# for preparing that computer (toolchain, key-only SSH).
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> [NVRVIEW_WINDOWS_WORKER=<name>] \
#     scripts/windows-check.sh
#
# A finished worker's folder, its target\ build output included, is deleted with
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> scripts/windows-check.sh --clean <name>
#
# Each run prints the Windows computer's free disk space first and warns
# under 10 GB, then the time of every phase and step.
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

host="${NVRVIEW_WINDOWS_HOST:?set NVRVIEW_WINDOWS_HOST to the SSH host alias of the Windows computer}"
clean=0
package_to=""
defaults_args=""
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
      *) echo "usage: $0 --package <directory> [--trusted-key-file FILE]... [--update-source SOURCE]" >&2; exit 2 ;;
    esac
  done
  if [ -n "$defaults_args" ]; then
    oldifs=$IFS
    IFS='
'
    # shellcheck disable=SC2086 # split on newlines only
    python3 -I scripts/update-defaults.py $defaults_args --out /dev/null >/dev/null
    IFS=$oldifs
  fi
elif [ $# -gt 0 ]; then
  echo "usage: $0 [--clean <worker name> | --package <directory> [--trusted-key-file FILE]... [--update-source SOURCE]]" >&2
  exit 2
fi
worker="${NVRVIEW_WINDOWS_WORKER:-main}"
case "$worker" in
  *[!A-Za-z0-9_-]* | '') echo "NVRVIEW_WINDOWS_WORKER: letters, digits, - and _ only" >&2; exit 2 ;;
esac
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
  ssh $ssh_opts "$host" "$gitbash -lc \"cd ~/nvrview/worker/$worker/src && scripts/windows-app.sh package\""
  mkdir -p "$package_to"
  # scp reads the Windows path relative to the home directory.
  # shellcheck disable=SC2086
  scp $ssh_opts "$host:nvrview/worker/$worker/src/target/dotnet/package/$zip" "$package_to/$zip"
  if [ -n "$defaults_args" ]; then
    oldifs=$IFS; IFS='
'
    # shellcheck disable=SC2086
    python3 -I scripts/update-defaults.py $defaults_args --add-to-zip "$package_to/$zip"
    IFS=$oldifs
  fi
  ls -l "$package_to/$zip"
  exit 0
fi

echo "running the Windows checks on $host"
# cmd.exe does not treat single quotes as quoting: double-quote bash's script.
# The worker's options travel as variables set in that script.
vars=""
for v in NVRVIEW_APP_SELFTEST NVRVIEW_APP_SELFTEST_SECONDS NVRVIEW_WINDOWS_ONLY NVRVIEW_MIC_TEST; do
  eval "val=\${$v:-}"
  case "$val" in
    *[!A-Za-z0-9_.-]*) echo "$v: letters, digits, '.', '-' and '_' only" >&2; exit 2 ;;
  esac
  if [ -n "$val" ]; then
    vars="$vars $v=$val"
  fi
done
# $gitbash and $vars are meant to expand here, on this side.
# shellcheck disable=SC2086,SC2029
ssh $ssh_opts "$host" "$gitbash -lc \"cd ~/nvrview/worker/$worker/src &&$vars scripts/windows-worker.sh\""
now=$(date +%s)
# A full pass (app self-tests included) on a clean tree, i.e. exactly the
# tree of HEAD, leaves a stamp that scripts/checked.sh --windows and
# scripts/release.sh look for.
if [ "${NVRVIEW_APP_SELFTEST:-}" = 1 ] && [ -z "${NVRVIEW_WINDOWS_ONLY:-}" ] &&
  [ -z "$(git status --porcelain)" ] && [ "$tree_sent" = "$(git rev-parse 'HEAD^{tree}')" ]; then
  scripts/checked.sh --record windows "$((now - check_start))" "app self-tests${NVRVIEW_MIC_TEST:+ with the real microphone}; worker $worker"
else
  echo "no check stamp written (needs NVRVIEW_APP_SELFTEST=1, no NVRVIEW_WINDOWS_ONLY, and a clean tree)"
fi
printf '\nWindows checks on %s took %ss in all (disk check and sync %ss).\n' "$host" "$((now - check_start))" "$((sent - check_start))"
