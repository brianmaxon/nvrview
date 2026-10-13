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
# With NVRVIEW_WINDOWS_HOSTS naming several computers, a check that has work
# in both halves (Rust: clippy and tests; everything else) and finds a second
# computer free runs the halves at the same time, one per computer
# (scripts/windows-split.py decides and balances; NVRVIEW_WINDOWS_SPLIT=0 turns
# it off; NVRVIEW_WINDOWS_SPEEDS="host=2 other=1" weights the computers). The
# second computer's lock is a try, never a wait, and is given back as soon as
# its half is done. Results, the test result cache and the stamp are the same
# as for a one-computer run.
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
free_gb() { # [HOST]
  # shellcheck disable=SC2086
  ssh $ssh_opts "${1:-$host}" 'powershell -NoProfile -Command "[math]::Floor((Get-PSDrive ($env:USERPROFILE.Substring(0,1))).Free / 1GB)"' |
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
# Unpacks this working tree into the worker's folder on a host.
# cmd.exe on the far side: recreate src, keep target (a directory junction
# inside src points at it; the junction alone is removed first, with a plain
# rmdir, so deleting src never reaches into target), unpack from stdin with
# Windows' own tar.
# $worker is meant to expand here, on this side.
# COPYFILE_DISABLE and --no-mac-metadata keep macOS from adding ._* files
# (MSBuild took one for a second project file).
# Untracked files go too (work not yet committed), but never build output or
# other checkouts nested in this one.
sync_tree() { # HOST
echo "sending the working tree to $1 (worker $worker)"
# shellcheck disable=SC2086,SC2029
git ls-files -z --cached --others --exclude-standard -- . \
  ':(exclude,glob)**/target/**' ':(exclude).claude/worktrees' ':(exclude).worktrees' |
  COPYFILE_DISABLE=1 tar --no-mac-metadata --null -T - -cf - |
  ssh $ssh_opts "$1" "cd /d %USERPROFILE% && (if not exist nvrview\\worker\\$worker mkdir nvrview\\worker\\$worker) && cd nvrview\\worker\\$worker && (if not exist target mkdir target) && (if exist src\target rmdir src\target) && (if exist src rmdir /s /q src) && mkdir src && C:\Windows\System32\tar.exe -xf - -C src && mklink /J src\\target target >nul"
}
sync_tree "$host"

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
  # The debug build's app self-test is cached by area (scripts/windows-selftest-areas.txt); the trimmed publish
  # build runs a short smoke (app-publish). NVRVIEW_APP_AREAS=a,b asks for those areas only (a partial run: no
  # stamp, not a full pass, no trimmed build).
  for a in $(cache_areas); do
    case ",${NVRVIEW_APP_AREAS:-$a}," in *",$a,"*) wanted="$wanted app-debug.$a" ;; esac
  done
  if [ -z "${NVRVIEW_APP_AREAS:-}" ]; then wanted="$wanted app-publish"; fi
  wanted="$wanted app-update"
fi
for s in $wanted; do
  if cache_want "$s"; then NVRVIEW_SUITES="$NVRVIEW_SUITES${NVRVIEW_SUITES:+,}$s"; fi
done
NVRVIEW_SUITES=${NVRVIEW_SUITES:-none}

echo "running the Windows checks on $host"
# cmd.exe does not treat single quotes as quoting: double-quote bash's script.
# The worker's options travel as variables set in that script.
# worker_vars PART sets $vars for one run of the worker: all (the whole check),
# or one half of a split run (scripts/windows-split.py): rust (clippy, tests
# and doctests only) or rest (every other suite).
worker_vars() { # PART
  vars=""
  for v in NVRVIEW_APP_SELFTEST NVRVIEW_APP_SELFTEST_SECONDS NVRVIEW_WINDOWS_ONLY NVRVIEW_MIC_TEST \
    NVRVIEW_SUITES NVRVIEW_CLIPPY_CRATES NVRVIEW_TEST_CRATES NVRVIEW_DOCTEST_CRATES; do
    eval "val=\${$v:-}"
    case "$1:$v" in
      rust:NVRVIEW_SUITES) val=none ;;
      rest:NVRVIEW_CLIPPY_CRATES | rest:NVRVIEW_TEST_CRATES | rest:NVRVIEW_DOCTEST_CRATES) val=none ;;
    esac
    case "$val" in
      *[!A-Za-z0-9_.,-]*) echo "$v: letters, digits, '.', ',', '-' and '_' only" >&2; exit 2 ;;
    esac
    if [ -n "$val" ]; then
      vars="$vars $v=$val"
    fi
  done
  if [ "$1" != all ]; then vars="$vars NVRVIEW_WINDOWS_PART=$1"; fi
  vars="$vars NVRVIEW_TREE=$tree_sent"
}
worker_vars all
vars_all=$vars
worker_vars rust
vars_rust=$vars
worker_vars rest
vars_rest=$vars

# One run of the worker on HOST: its output goes to a log (and to the terminal
# when STREAM is 1), its exit status to a file. $gitbash and $3 are meant to
# expand here, on this side.
# shellcheck disable=SC2086,SC2029
run_part() { # HOST PART VARS STREAM
  {
    if ssh $ssh_opts "$1" "$gitbash -lc \"cd ~/nvrview/worker/$worker/src &&$3 scripts/windows-worker.sh\"" 2>&1; then
      echo 0 >"$logs/rc.$2"
    else
      echo "$?" >"$logs/rc.$2"
    fi
  } | if [ "$4" = 1 ]; then tee "$logs/worker.$2.log"; else cat >"$logs/worker.$2.log"; fi
}

# Splitting the check over two computers (docs: apps/windows/README.md, "Host
# pool"): when the check has work in both halves, a second computer is free
# right now (a try, never a wait, with the lock's usual priority rules), it has
# the same toolchain, and scripts/windows-split.py expects it to be faster, the
# halves run at the same time, each on its own computer. A host is held only
# while it is used: the second one is given back as soon as its half is done.
lock_owner="${NVRVIEW_WINDOWS_LOCK_OWNER:-$$}"
second=""
release_second() {
  if [ -n "$second" ]; then
    scripts/windows-lock.sh release --host "$second" --owner-pid "$lock_owner" || true
    second=""
  fi
}
trap 'finish_cache --failed "Windows worker"; release_second; rm -rf "$logs"; windows_unlock' EXIT
split_plan() { # HOST SECOND
  "$py" -I scripts/windows-split.py plan --explain --hosts "$1" "$2" --rust "$rust_halves" --suites "$NVRVIEW_SUITES" 2>"$logs/plan.detail"
}
# Every run says whether it splits, and why or why not, on one line.
decision() { echo "split decision: $*"; }
rust_halves=""
for kv in clippy:NVRVIEW_CLIPPY_CRATES test:NVRVIEW_TEST_CRATES doctest:NVRVIEW_DOCTEST_CRATES; do
  eval "val=\${${kv#*:}:-none}"
  if [ "$val" != none ]; then rust_halves="$rust_halves${rust_halves:+,}${kv%%:*}"; fi
done
rust_halves=${rust_halves:-none}
parts="all"
host_rust=""
host_rest=""
if [ -n "$only" ]; then
  decision "whole on $host (NVRVIEW_WINDOWS_ONLY is set)"
elif [ "${NVRVIEW_WINDOWS_SPLIT:-1}" = 0 ]; then
  decision "whole on $host (NVRVIEW_WINDOWS_SPLIT=0)"
elif [ "$rust_halves" = none ] || [ "$NVRVIEW_SUITES" = none ]; then
  decision "whole on $host (nothing to run in one half: Rust work $rust_halves, other suites $NVRVIEW_SUITES)"
else
  candidate=$(windows_hosts | grep -vx "$host" | head -n 1 || true)
  if [ -z "$candidate" ]; then
    decision "whole on $host (only one Windows host is configured)"
  else
    plan=$(split_plan "$host" "$candidate" || true)
    detail=$(cat "$logs/plan.detail" 2>/dev/null || true)
    case "$plan" in
      split*)
        if second=$(scripts/windows-lock.sh acquire --try --priority "${NVRVIEW_WINDOWS_PRIORITY:-agent}" --owner-pid "$lock_owner" --exclude "$host" 2>/dev/null); then
          :
        else
          second=""
          decision "whole on $host ($candidate is not free right now, or a waiting merge goes first there; the plan was: $plan)"
        fi
        ;;
      *) decision "whole on $host (${plan#single }; $detail)" ;;
    esac
    if [ -n "$second" ]; then
      # The same toolchain on both computers, or the results would not be the
      # same as a one-computer run's.
      # shellcheck disable=SC2086,SC2029
      ssh $ssh_opts "$second" "$gitbash -lc \"rustc -vV; cargo nextest --version; cargo clippy --version; dotnet --version\"" \
        </dev/null 2>/dev/null | tr -d '\r' >"$logs/tools2" || true
      plan=$(split_plan "$host" "$second" || true)
      detail=$(cat "$logs/plan.detail" 2>/dev/null || true)
      if ! cmp -s "$logs/tools" "$logs/tools2"; then
        decision "whole on $host ($second has other tool versions: $(diff "$logs/tools" "$logs/tools2" | grep '^[<>]' | head -n 3 | tr '\n' ' '))"
        release_second
      else
        case "$plan" in
          split*)
            if sync_tree "$second"; then
              parts="rust rest"
              host_rust=$(printf '%s\n' "$plan" | tr ' ' '\n' | sed -n 's/^\(.*\)=rust$/\1/p')
              host_rest=$(printf '%s\n' "$plan" | tr ' ' '\n' | sed -n 's/^\(.*\)=rest$/\1/p')
              decision "split ($plan; $detail)"
            else
              decision "whole on $host (sending the tree to $second failed)"
              release_second
            fi
            ;;
          *) decision "whole on $host (${plan#single }; $detail)"; release_second ;;
        esac
      fi
    fi
  fi
fi

if [ "$parts" = all ]; then
  logfiles="$logs/worker.all.log"
  run_part "$host" all "$vars_all" 1
else
  echo "splitting the check: Rust half (clippy, tests, doctests) on $host_rust, everything else on $host_rest"
  logfiles="$logs/worker.rust.log $logs/worker.rest.log"
  # The half on the second computer gives its lock back when it is done.
  (
    run_part "$host_rust" rust "$vars_rust" 0
    if [ "$host_rust" = "$second" ]; then release_second; fi
  ) &
  rust_pid=$!
  (
    run_part "$host_rest" rest "$vars_rest" 0
    if [ "$host_rest" = "$second" ]; then release_second; fi
  ) &
  rest_pid=$!
  wait "$rust_pid" || true
  wait "$rest_pid" || true
  second=""
fi
rc=0
failed_parts=""
for pt in $parts; do
  prc=$(cat "$logs/rc.$pt" 2>/dev/null || echo 1)
  if [ "$prc" != 0 ]; then
    rc=$prc
    failed_parts="$failed_parts $pt"
  fi
done
if [ "$parts" != all ]; then
  # Each half's output in turn, a passing one first, so that the output ends
  # with the failing step the way a one-computer run's does.
  for pt in $parts; do
    case " $failed_parts " in *" $pt "*) continue ;; esac
    printf '\n---- %s half, on %s: passed ----\n' "$pt" "$([ "$pt" = rust ] && echo "$host_rust" || echo "$host_rest")"
    cat "$logs/worker.$pt.log"
  done
  for pt in $failed_parts; do
    printf '\n---- %s half, on %s: FAILED ----\n' "$pt" "$([ "$pt" = rust ] && echo "$host_rust" || echo "$host_rest")"
    cat "$logs/worker.$pt.log"
  done
fi
: >"$logs/results"
for pt in $parts; do
  tr -d '\r' <"$logs/worker.$pt.log" | sed -n 's/^suite-result \([A-Za-z0-9_.-]*\) \([0-9]*\)$/\1 \2/p' >>"$logs/results"
done
while read -r name secs; do
  cache_pass "$secs" "$name"
done <"$logs/results"
if [ "$rc" != 0 ]; then
  # Name the failure: the half, the suite or area that was running, and the failing step.
  for pt in $failed_parts; do
    if [ "$parts" != all ]; then
      echo "Windows check: the $pt half failed on $([ "$pt" = rust ] && echo "$host_rust" || echo "$host_rest")" >&2
    fi
    tr -d '\r' <"$logs/worker.$pt.log" | "$py" -I scripts/flakes.py explain /dev/stdin | sed 's/^/Windows check: /' >&2
  done
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
if [ "$parts" = all ]; then
  printf '\nWindows checks on %s took %ss in all (disk check and sync %ss).\n' "$host" "$((now - check_start))" "$((sent - check_start))"
else
  printf '\nWindows checks (Rust half on %s, the rest on %s) took %ss in all (disk check and sync %ss).\n' "$host_rust" "$host_rest" "$((now - check_start))" "$((sent - check_start))"
fi
# Step times: the sync here, the worker's steps and background builds.
printf '   (sync: %ss)\n' "$((sent - check_start))" >"$logs/steps"
"$py" -I scripts/timing.py table "$logs/steps" $logfiles
"$py" -I scripts/timing.py log windows "$((now - check_start))" --tree "$tree_sent" "$logs/steps" $logfiles || true
