#!/bin/sh
# A full check (NVRVIEW_FULL_CHECK=1, Mac and Windows: scripts/release-check.sh)
# of a commit, in the background, so that a release of main finds full check
# stamps for its tree and only packages (scripts/release.sh without
# --full-check). scripts/merge-branch.sh and scripts/merge-train.sh start it
# after a merge.
#
#   scripts/full-check-bg.sh start [COMMIT]   supersede any running one and check COMMIT (default HEAD)
#   scripts/full-check-bg.sh stop             stop the running one, if any
#   scripts/full-check-bg.sh status           what is running or finished last
#
# The check runs in its own worktree (<repository>/.worktrees/fullcheck, kept
# between runs so its build output is reused) on a detached checkout of COMMIT,
# so the next merge can go on in the main checkout. Its Windows work uses a
# worker named fullcheck, at lock priority "background" (scripts/windows-lock.sh):
# it takes a Windows computer only when no merge and no agent is waiting for
# it. A check that already started is never interrupted by a waiting agent, so
# an agent can wait for the length of one check at most.
#
# A newer start supersedes an older one: the older check's process group is
# stopped and its Windows locks given back before the new one begins. Nothing
# starts when COMMIT's tree already has full Mac and Windows stamps. A check that
# was stopped or failed leaves no stamps; a release then needs --full-check.
#
# State (common git directory, nvrview-checks/fullcheck/): pid, commit, tree,
# started, result ("pass" or "fail N", written when it ends), and fullcheck.log.
#
# Environment: NVRVIEW_FULLCHECK_CMD replaces the check (tests),
# NVRVIEW_FULLCHECK_WORKER the Windows worker name (default fullcheck),
# NVRVIEW_FULLCHECK_DIR the worktree.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh

cmd="${1:-status}"
[ $# -eq 0 ] || shift
dir="$(stamp_dir)/fullcheck"
mkdir -p "$dir"
wt="${NVRVIEW_FULLCHECK_DIR:-$(pwd)/.worktrees/fullcheck}"

running_pid() {
  p=$(cat "$dir/pid" 2>/dev/null || true)
  # Alive and still our wrapper (its command line names FC_WT), not a process that reused the pid.
  if [ -n "$p" ] && kill -0 "$p" 2>/dev/null && ps -o command= -p "$p" 2>/dev/null | grep -q 'FC_WT'; then echo "$p"; fi
}

stop_running() {
  p=$(running_pid)
  [ -n "$p" ] || return 0
  echo "full check: stopping the older check of $(cat "$dir/commit" 2>/dev/null | cut -c1-9) (pid $p)"
  members=$(pgrep -g "$p" 2>/dev/null || true)
  kill -TERM -"$p" 2>/dev/null || kill -TERM "$p" 2>/dev/null || true
  # Wait for the group to be gone (a condition, bounded by a count).
  n=0
  while [ -n "$(pgrep -g "$p" 2>/dev/null || true)" ] && [ "$n" -lt 200 ]; do
    n=$((n + 1))
    sleep 0.05
  done
  if [ -n "$(pgrep -g "$p" 2>/dev/null || true)" ]; then kill -KILL -"$p" 2>/dev/null || true; fi
  # Its Windows locks belong to processes of the group; give them back now
  # instead of waiting for the stale-lock rule.
  for m in $p $members; do scripts/windows-lock.sh release --owner-pid "$m" >/dev/null 2>&1 || true; done
  rm -f "$dir/pid"
  echo "stopped (superseded or asked to)" >"$dir/result"
}

case "$cmd" in
  status)
    p=$(running_pid)
    if [ -n "$p" ]; then
      echo "full check running: commit $(cut -c1-9 "$dir/commit"), tree $(cat "$dir/tree"), pid $p, log $dir/fullcheck.log"
    elif [ -f "$dir/result" ]; then
      echo "full check not running; last: commit $(cut -c1-9 "$dir/commit" 2>/dev/null), $(cat "$dir/result")"
    else
      echo "no background full check has run"
    fi
    ;;
  stop)
    stop_running
    ;;
  start)
    commit=$(git rev-parse --verify "${1:-HEAD}^{commit}")
    tree=$(git rev-parse "$commit^{tree}")
    if [ -n "$(running_pid)" ] && [ "$(cat "$dir/tree" 2>/dev/null)" = "$tree" ]; then
      echo "full check: already running for tree $tree"
      exit 0
    fi
    stop_running
    if full_stamp mac "$tree" && full_stamp windows "$tree"; then
      echo "full check: tree $tree already has full Mac and Windows stamps; nothing to start"
      exit 0
    fi
    # The check's own worktree, detached at the commit.
    if [ -e "$wt/.git" ]; then
      git -C "$wt" checkout -q --detach "$commit"
      git -C "$wt" reset -q --hard "$commit"
    else
      mkdir -p "$(dirname "$wt")"
      git worktree add -q --detach "$wt" "$commit"
    fi
    echo "$commit" >"$dir/commit"
    echo "$tree" >"$dir/tree"
    date +%s >"$dir/started"
    rm -f "$dir/result"
    : >"$dir/fullcheck.log"
    # Detached into its own session (so that stopping it reaches the whole
    # check), at the lowest Windows priority. The wrapper records how it ended.
    NVRVIEW_WINDOWS_PRIORITY=background \
    NVRVIEW_WINDOWS_WORKER="${NVRVIEW_FULLCHECK_WORKER:-fullcheck}" \
    FC_DIR="$dir" FC_WT="$wt" FC_CMD="${NVRVIEW_FULLCHECK_CMD:-scripts/release-check.sh}" \
      python3 -I - <<'PY'
import os, subprocess
d, wt, cmd = os.environ["FC_DIR"], os.environ["FC_WT"], os.environ["FC_CMD"]
script = 'cd "$FC_WT" && %s; rc=$?; if [ "$rc" = 0 ]; then echo pass; else echo "fail $rc"; fi >"$FC_DIR/result"' % cmd
log = open(os.path.join(d, "fullcheck.log"), "ab")
p = subprocess.Popen(["sh", "-c", script], stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                     start_new_session=True, env=os.environ)
with open(os.path.join(d, "pid"), "w") as f:
    f.write("%d\n" % p.pid)
PY
    echo "full check: started for $(echo "$commit" | cut -c1-9) (tree $tree); log $dir/fullcheck.log"
    ;;
  *)
    echo "usage: $0 start [COMMIT] | stop | status" >&2
    exit 2
    ;;
esac
