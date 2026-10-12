# Shared decisions about check stamps (sourced by scripts/release.sh and
# scripts/merge-branch.sh; tested by scripts/test-stamps.sh). POSIX sh.
#
# Also the Windows lock helpers (windows_lock, windows_unlock, windows_hosts).
#
# Stamps live in <git common dir>/nvrview-checks/ as <tree>.<kind>:
#   mac, windows    a full check passed on a clean checkout of that exact tree
#   windows-reused  the merged tree was accepted without a Windows run of its
#                   own (scripts/merge-branch.sh); never enough for a release
# Functions use the repository of the current directory.

stamp_dir() {
  printf '%s/nvrview-checks\n' "$(cd "$(git rev-parse --git-common-dir)" && pwd)"
}

has_stamp() { # KIND TREE
  [ -f "$(stamp_dir)/$2.$1" ]
}

# A stamp from a run that took nothing from the test result cache
# (scripts/suite-cache.py). Stamps written before the cache are full.
full_stamp() { # KIND TREE
  has_stamp "$1" "$2" && ! grep -q '^full=no$' "$(stamp_dir)/$2.$1"
}

# The suites a stamp took from the cache, or nothing.
stamp_cached() { # KIND TREE
  sed -n 's/^cached=//p' "$(stamp_dir)/$2.$1" 2>/dev/null || true
}

# Succeeds when the only file that differs between the two trees is VERSION.
version_only_diff() { # TREE_A TREE_B
  [ "$(git diff --name-only "$1" "$2")" = "VERSION" ]
}

# Prints the tree whose stamp of KIND counts for TREE and why, and succeeds:
# the tree itself, or (the version-only rule) a stamped tree that differs from
# it in VERSION alone. Only full stamps count (nothing taken from the test
# result cache). Fails, printing nothing, when there is none.
release_stamp_tree() { # KIND TREE
  if full_stamp "$1" "$2"; then
    printf '%s exact\n' "$2"
    return 0
  fi
  for f in "$(stamp_dir)"/*."$1"; do
    [ -f "$f" ] || continue
    t=$(basename "$f" ".$1")
    full_stamp "$1" "$t" || continue
    if git cat-file -e "$t^{tree}" 2>/dev/null && version_only_diff "$t" "$2"; then
      printf '%s version-only\n' "$t"
      return 0
    fi
  done
  return 1
}

# Files that differ between two trees/commits, one per line, sorted.
changed_files() { # A B
  git diff --name-only "$1" "$2" | LC_ALL=C sort
}

# Decides whether a merge needs its own Windows check.
#   windows_reuse_decision BRANCH_TIP MAIN_BEFORE MERGED_TREE
# MAIN_BEFORE is the commit being merged into; MERGED_TREE the tree of the
# merge result. Prints one line naming the rule applied. Succeeds (skip the
# Windows check) or fails (run it).
windows_reuse_decision() {
  tip="$1"; main="$2"; merged="$3"
  branch_tree=$(git rev-parse "$tip^{tree}")
  main_tree=$(git rev-parse "$main^{tree}")
  if ! has_stamp windows "$branch_tree"; then
    echo "run: the branch's own tree $branch_tree has no Windows stamp (agents run the full Windows check on a clean committed tree)"
    return 1
  fi
  if [ "$merged" = "$branch_tree" ]; then
    echo "skip, rule A: the merged tree is identical to the branch tree that passed Windows"
    return 0
  fi
  if ! has_stamp windows "$main_tree" && ! has_stamp windows-reused "$main_tree"; then
    echo "run: the merged tree differs from the branch tree, and main's tree $main_tree has no Windows stamp"
    return 1
  fi
  base=$(git merge-base "$main" "$tip") || { echo "run: no merge base"; return 1; }
  main_changed=$(changed_files "$base" "$main")
  branch_changed=$(changed_files "$base" "$tip")
  differing=$(changed_files "$tip" "$merged")
  if [ "$differing" != "$main_changed" ]; then
    echo "run: the merge changed files beyond what main changed since the merge base"
    return 1
  fi
  overlap=$(printf '%s\n%s\n' "$main_changed" "$branch_changed" | grep -v '^$' | LC_ALL=C sort | uniq -d)
  if [ -n "$overlap" ]; then
    echo "run: both sides changed the same files ($(printf '%s' "$overlap" | head -n 3 | tr '\n' ' ')...)"
    return 1
  fi
  echo "skip, rule B: the branch's tree passed Windows; the merge only adds main's changes (no file changed on both sides), and main's tree had a Windows result"
  return 0
}

# Whether the output of a failed Windows check looks like a known
# environmental flake: the video self-test falling back to a software H.264
# decoder, or the real-microphone step. Reads the log file.
windows_flake() { # LOGFILE
  last=$(grep '^== ' "$1" | tail -n 1)
  case "$last" in *"video self-test"*) return 0 ;; esac
  grep -qiE 'microphone|mic-test|mic test' "$1" && return 0
  grep -qiE 'software' "$1" && grep -qiE 'h\.?264' "$1" && return 0
  return 1
}

# The Windows test computers: NVRVIEW_WINDOWS_HOSTS (space-separated ssh
# aliases), else NVRVIEW_WINDOWS_HOST. One alias per line; nothing when unset.
windows_hosts() {
  # shellcheck disable=SC2086 # split on blanks on purpose
  for h in ${NVRVIEW_WINDOWS_HOSTS:-${NVRVIEW_WINDOWS_HOST:-}}; do echo "$h"; done
}

# Exclusive use of a Windows computer (scripts/windows-lock.sh): waits for a
# free host at priority $NVRVIEW_WINDOWS_PRIORITY (merge or agent, default
# agent), exports the chosen host as NVRVIEW_WINDOWS_HOST, and marks the lock
# as held (NVRVIEW_WINDOWS_LOCK_HELD=1) so a nested scripts/windows-check.sh
# does not take it again. Does nothing when the caller already holds it.
windows_lock() {
  [ "${NVRVIEW_WINDOWS_LOCK_HELD:-0}" = 1 ] && return 0
  chosen=$(scripts/windows-lock.sh acquire --priority "${NVRVIEW_WINDOWS_PRIORITY:-agent}" --owner-pid $$) || return 1
  if [ -n "$chosen" ]; then
    NVRVIEW_WINDOWS_HOST="$chosen"
    export NVRVIEW_WINDOWS_HOST
  fi
  NVRVIEW_WINDOWS_LOCK_HELD=1
  export NVRVIEW_WINDOWS_LOCK_HELD
  windows_locked=1
}

windows_unlock() {
  if [ "${windows_locked:-0}" = 1 ]; then
    scripts/windows-lock.sh release --owner-pid $$ || true
    windows_locked=0
    NVRVIEW_WINDOWS_LOCK_HELD=0
    export NVRVIEW_WINDOWS_LOCK_HELD
  fi
}

# Gives back a Windows lock that a background half of parallel_checks may
# still hold (the lock belongs to this shell's pid); safe when none is held.
windows_unlock_any() {
  scripts/windows-lock.sh release --owner-pid $$ >/dev/null 2>&1 || true
}

# Runs the Mac check and the Windows check at the same time (merge-branch.sh,
# merge-train.sh; release-check.sh does the same), each with its own log.
#   parallel_checks LOGDIR MAC_CMD WINDOWS_CMD SKIP_WINDOWS(0|1)
# The commands run through `sh -c` in the current directory, which must be a
# clean committed tree. The Windows half takes the Windows lock for its run
# and retries up to 3 times after a known environmental flake (a retry runs
# with NVRVIEW_RETRIED=1). Leaves LOGDIR/mac.log, windows.log, windows.msgs
# and sets mac_rc, win_rc (0 when skipped), mac_secs, win_secs. The caller
# decides what a failure means; a lock that cannot be taken counts as one.
parallel_checks() {
  pc_logs="$1"; pc_mac="$2"; pc_win="$3"; pc_skip="$4"
  rm -f "$pc_logs/mac.rc" "$pc_logs/win.rc"
  : >"$pc_logs/mac.log"
  : >"$pc_logs/windows.log"
  : >"$pc_logs/windows.msgs"
  (
    t0=$(date +%s)
    rc=0
    sh -c "$pc_mac" >"$pc_logs/mac.log" 2>&1 || rc=$?
    echo "$rc $(($(date +%s) - t0))" >"$pc_logs/mac.rc"
  ) &
  pc_mac_pid=$!
  pc_win_pid=""
  if [ "$pc_skip" != 1 ]; then
    (
      t0=$(date +%s)
      rc=0
      if windows_lock 2>>"$pc_logs/windows.msgs"; then
        try=1
        while :; do
          retried=""
          if [ "$try" -gt 1 ]; then retried=1; fi
          echo "== Windows check (try $try of 3)" >>"$pc_logs/windows.msgs"
          rc=0
          NVRVIEW_RETRIED=$retried sh -c "$pc_win" >"$pc_logs/windows.log" 2>&1 || rc=$?
          if [ "$rc" != 0 ] && [ "$try" -lt 3 ] && windows_flake "$pc_logs/windows.log"; then
            echo "looks like a known environmental flake; trying again" >>"$pc_logs/windows.msgs"
            try=$((try + 1))
            continue
          fi
          break
        done
        windows_unlock
      else
        rc=1
        echo "the Windows lock could not be taken" >>"$pc_logs/windows.msgs"
      fi
      echo "$rc $(($(date +%s) - t0))" >"$pc_logs/win.rc"
    ) &
    pc_win_pid=$!
  fi
  wait "$pc_mac_pid" || true
  if [ -n "$pc_win_pid" ]; then wait "$pc_win_pid" || true; fi
  # shellcheck disable=SC2034 # the results are read by the caller
  read -r mac_rc mac_secs <"$pc_logs/mac.rc" || { mac_rc=1; mac_secs=0; }
  win_rc=0
  win_secs=0
  if [ "$pc_skip" != 1 ]; then
    # shellcheck disable=SC2034
    read -r win_rc win_secs <"$pc_logs/win.rc" || { win_rc=1; win_secs=0; }
  fi
}

# The total and the slowest three steps over the check logs, and a line in the
# timing log (scripts/timing.py).
#   report_timings KIND TOTAL_SECONDS TREE LOG...
report_timings() {
  rt_kind="$1"; rt_total="$2"; rt_tree="$3"; shift 3
  echo "total $rt_total s; $(python3 -I scripts/timing.py slowest "$@")"
  python3 -I scripts/timing.py log "$rt_kind" "$rt_total" --tree "$rt_tree" "$@" || true
}
