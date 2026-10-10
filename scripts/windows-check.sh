#!/bin/sh
# Runs the Windows checks (scripts/windows-worker.sh) on a local Windows
# computer over SSH, against this working tree as it is now (committed or
# not), so Windows results do not need a push. See scripts/setup-windows.ps1
# for preparing that computer (toolchain, key-only SSH).
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> [NVRVIEW_WINDOWS_WORKER=<name>] \
#     scripts/windows-check.sh
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
worker="${NVRVIEW_WINDOWS_WORKER:-main}"
case "$worker" in
  *[!A-Za-z0-9_-]* | '') echo "NVRVIEW_WINDOWS_WORKER: letters, digits, - and _ only" >&2; exit 2 ;;
esac
ssh_opts="-o BatchMode=yes -o ConnectTimeout=10"
gitbash='"C:\Program Files\Git\bin\bash.exe"'

# shellcheck disable=SC2086 # ssh_opts is a list of options
ssh $ssh_opts "$host" "echo connected" >/dev/null

echo "sending the working tree to $host (worker $worker)"
# cmd.exe on the far side: recreate src, keep target (a directory junction
# inside src points at it; the junction alone is removed first, with a plain
# rmdir, so deleting src never reaches into target), unpack from stdin with
# Windows' own tar.
# $worker is meant to expand here, on this side.
# shellcheck disable=SC2086,SC2029
# COPYFILE_DISABLE and --no-mac-metadata keep macOS from adding ._* files
# (MSBuild took one for a second project file).
git ls-files -z --cached --others --exclude-standard |
  COPYFILE_DISABLE=1 tar --no-mac-metadata --null -T - -cf - |
  ssh $ssh_opts "$host" "cd /d %USERPROFILE% && (if not exist nvrview\\worker\\$worker mkdir nvrview\\worker\\$worker) && cd nvrview\\worker\\$worker && (if not exist target mkdir target) && (if exist src\target rmdir src\target) && (if exist src rmdir /s /q src) && mkdir src && C:\Windows\System32\tar.exe -xf - -C src && mklink /J src\\target target >nul"

echo "running the Windows checks on $host"
# cmd.exe does not treat single quotes as quoting: double-quote bash's script.
# The worker's options travel as variables set in that script.
vars=""
for v in NVRVIEW_APP_SELFTEST NVRVIEW_APP_SELFTEST_SECONDS NVRVIEW_WINDOWS_ONLY; do
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
