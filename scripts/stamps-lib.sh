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

# The answer is remembered per directory (and GIT_DIR) in the current shell,
# since it only changes when the repository does; a bare `stamp_dir >/dev/null`
# (not in a subshell) fills the memory for the `$(stamp_dir)` calls after it.
stamp_dir() {
  if [ "${_sd_key:-}" != "$PWD|${GIT_DIR:-}" ]; then
    _sd_val=$(cd "$(git rev-parse --git-common-dir)" && pwd)/nvrview-checks
    _sd_key="$PWD|${GIT_DIR:-}"
  fi
  printf '%s\n' "$_sd_val"
}

has_stamp() { # KIND TREE
  stamp_dir >/dev/null
  [ -f "$(stamp_dir)/$2.$1" ]
}

# A stamp from a run that took nothing from the test result cache
# (scripts/suite-cache.py). Stamps written before the cache are full.
full_stamp() { # KIND TREE
  has_stamp "$1" "$2" && ! grep -q '^full=no$' "$(stamp_dir)/$2.$1"
}

# The suites a stamp took from the cache, or nothing.
stamp_cached() { # KIND TREE
  stamp_dir >/dev/null
  sed -n 's/^cached=//p' "$(stamp_dir)/$2.$1" 2>/dev/null || true
}

# Succeeds when the trees differ and the only files that differ between them
# are VERSION and markdown (*.md) files: nothing compiled, run, or bundled
# changes (docs/DEVELOPMENT.md, "Docs-only changes"). Markdown is read only by
# the checks in scripts/check-docs.sh, which still run (docs_checks_ok).
nonmaterial_diff() { # TREE_A TREE_B
  nd_files=$(git diff --name-only "$1" "$2")
  [ -n "$nd_files" ] || return 1
  ! printf '%s\n' "$nd_files" | grep -qvE '^(VERSION|.*\.md)$'
}

# Succeeds when a markdown file differs between the two trees.
docs_in_diff() { # TREE_A TREE_B
  git diff --name-only "$1" "$2" | grep -q '\.md$'
}

# "docs-only" when a markdown file differs between the trees, else "VERSION-only".
nonmaterial_label() { # TREE_A TREE_B
  if docs_in_diff "$1" "$2"; then echo docs-only; else echo VERSION-only; fi
}

# Succeeds when the working tree is exactly TREE (index and files, nothing
# untracked), so the markdown checks can run on it now.
docs_here() { # TREE
  [ "$(git write-tree 2>/dev/null)" = "$1" ] && git diff --quiet &&
    [ -z "$(git ls-files --others --exclude-standard)" ]
}

# Succeeds when the markdown checks have passed for TREE (a <tree>.docs marker
# next to the stamps) or can run now, on the working tree.
docs_ready() { # TREE
  stamp_dir >/dev/null
  [ -f "$(stamp_dir)/$1.docs" ] || docs_here "$1"
}

# Runs the markdown checks (NVRVIEW_DOCS_CHECK, default scripts/check-docs.sh)
# for TREE unless they passed already, and leaves the marker. They run on the
# working tree, so it must be TREE. Their output goes to the error stream.
docs_checks_ok() { # TREE
  stamp_dir >/dev/null
  [ -f "$(stamp_dir)/$1.docs" ] && return 0
  docs_here "$1" || return 1
  if ${NVRVIEW_DOCS_CHECK:-sh scripts/check-docs.sh} >&2; then
    mkdir -p "$(stamp_dir)"
    echo "markdown checks passed" >"$(stamp_dir)/$1.docs"
    return 0
  fi
  return 1
}

# Prints the changed files and succeeds when the working tree (tracked changes
# and untracked files) differs from a tree with a stamp of KIND only in
# markdown files and VERSION, with at least one markdown file: a docs edit on
# top of a checked tree, however many unpushed commits that tree is ahead of
# the upstream. Looks at the newest 20 stamps.
docs_only_since_stamp() { # KIND
  ds_kind="$1"
  stamp_dir >/dev/null
  # shellcheck disable=SC2012 # stamp names are tree hashes
  for ds_f in $(ls -t "$(stamp_dir)"/*."$ds_kind" 2>/dev/null | head -n 20); do
    ds_t=$(basename "$ds_f" ".$ds_kind")
    git cat-file -e "$ds_t^{tree}" 2>/dev/null || continue
    ds_changed=$( { git diff --name-only "$ds_t"; git ls-files --others --exclude-standard; } | LC_ALL=C sort -u)
    printf '%s\n' "$ds_changed" | grep -q '\.md$' || continue
    printf '%s\n' "$ds_changed" | grep -qvE '^(VERSION|.*\.md)$' && continue
    printf '%s\n' "$ds_changed"
    return 0
  done
  return 1
}

# Prints the tree whose stamp of KIND counts for TREE and why, and succeeds:
# the tree itself, or (the non-material rule) a stamped tree that differs from
# it in VERSION and markdown files alone ("VERSION-only" when only VERSION
# differs, else "docs-only"; for the Mac a docs-only tree also needs its
# markdown checks, docs_ready). Only full stamps count (nothing taken from the
# test result cache). Fails, printing nothing, when there is none.
release_stamp_tree() { # KIND TREE
  if full_stamp "$1" "$2"; then
    printf '%s exact\n' "$2"
    return 0
  fi
  stamp_dir >/dev/null
  for f in "$(stamp_dir)"/*."$1"; do
    [ -f "$f" ] || continue
    t=$(basename "$f" ".$1")
    full_stamp "$1" "$t" || continue
    if git cat-file -e "$t^{tree}" 2>/dev/null && nonmaterial_diff "$t" "$2"; then
      if [ "$1" = mac ] && docs_in_diff "$t" "$2" && ! docs_ready "$2"; then continue; fi
      printf '%s %s\n' "$t" "$(nonmaterial_label "$t" "$2")"
      return 0
    fi
  done
  return 1
}

# The tree whose check actually ran for a stamp of KIND on TREE: the stamp's
# carried_from, or TREE itself.
stamp_root() { # KIND TREE
  sr_root=$(sed -n 's/^carried_from=//p' "$(stamp_dir)/$2.$1" 2>/dev/null || true)
  printf '%s\n' "${sr_root:-$2}"
}

# The tree of a stamp of KIND (mac or windows) that differs from TREE in
# VERSION and markdown files alone, and so counts for it: nothing a check looks
# at depends on the version's value, and markdown is read only by the checks in
# scripts/check-docs.sh (docs/DEVELOPMENT.md, "Version bumps", "Docs-only
# changes"). For the Mac, when markdown differs from the tree whose check ran,
# those checks must have passed for TREE or be able to run now (docs_ready). With MODE "full" (the
# default) only a full stamp (nothing taken from the test result cache) counts;
# with "any", a full one is still preferred. Prints the tree and succeeds;
# fails, printing nothing, when there is none. A windows-reused stamp is never
# looked at (its file names differ).
version_only_source() { # KIND TREE [full|any]
  vs_kind="$1"; vs_tree="$2"; vs_mode="${3:-full}"
  stamp_dir >/dev/null
  for vs_pass in full any; do
    if [ "$vs_mode" = full ] && [ "$vs_pass" = any ]; then break; fi
    for vs_f in "$(stamp_dir)"/*."$vs_kind"; do
      [ -f "$vs_f" ] || continue
      vs_t=$(basename "$vs_f" ".$vs_kind")
      [ "$vs_t" != "$vs_tree" ] || continue
      if [ "$vs_pass" = full ]; then full_stamp "$vs_kind" "$vs_t" || continue; fi
      if git cat-file -e "$vs_t^{tree}" 2>/dev/null && nonmaterial_diff "$vs_t" "$vs_tree"; then
        if [ "$vs_kind" = mac ] && docs_in_diff "$(stamp_root mac "$vs_t")" "$vs_tree" && ! docs_ready "$vs_tree"; then continue; fi
        printf '%s\n' "$vs_t"
        return 0
      fi
    done
  done
  return 1
}

# Succeeds when TREE has, or would get by the non-material rule, a stamp of
# every KIND given (mac, windows). Writes nothing.
version_carry_possible() { # TREE KIND...
  vc_tree="$1"; shift
  for vc_kind in "$@"; do
    has_stamp "$vc_kind" "$vc_tree" && continue
    version_only_source "$vc_kind" "$vc_tree" any >/dev/null || return 1
  done
  return 0
}

# The non-material rule for check stamps: a tree that differs from a stamped
# tree in VERSION and markdown files alone gets the same stamps, because that
# changes nothing a build or test looks at. A version bump runs no check; a
# markdown change first passes the checks that read markdown (docs_checks_ok,
# scripts/check-docs.sh) for the Mac stamp. For each kind (mac,
# windows) that TREE lacks, or has only from the test result cache while a
# full one exists to carry, writes <TREE>.<kind> as a copy of the source stamp
# with `carried_from=<the tree whose check actually ran>` and a note, and
# prints one line saying so. A carried stamp is full exactly when its source is.
# Succeeds when TREE ends up with every kind given (default: mac and windows).
carry_version_stamps() { # TREE [KIND...]
  cv_tree="$1"; shift
  [ $# -gt 0 ] || set -- mac windows
  stamp_dir >/dev/null
  cv_ok=0
  for cv_kind in "$@"; do
    if full_stamp "$cv_kind" "$cv_tree"; then continue; fi
    if cv_src=$(version_only_source "$cv_kind" "$cv_tree" full); then :
    elif has_stamp "$cv_kind" "$cv_tree"; then continue
    elif cv_src=$(version_only_source "$cv_kind" "$cv_tree" any); then :
    else cv_ok=1; continue
    fi
    cv_file="$(stamp_dir)/$cv_src.$cv_kind"
    cv_root=$(sed -n 's/^carried_from=//p' "$cv_file")
    [ -n "$cv_root" ] || cv_root="$cv_src"
    cv_label=$(nonmaterial_label "$cv_root" "$cv_tree")
    if [ "$cv_kind" = mac ] && [ "$cv_label" = docs-only ]; then
      docs_checks_ok "$cv_tree" || { cv_ok=1; continue; }
    fi
    mkdir -p "$(stamp_dir)"
    {
      printf 'date=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf 'duration=0s\n'
      printf 'tree=%s\n' "$cv_tree"
      printf 'steps=%s\n' "$(sed -n 's/^steps=//p' "$cv_file")"
      if grep -q '^full=no$' "$cv_file"; then
        printf 'full=no\ncached=%s\n' "$(sed -n 's/^cached=//p' "$cv_file")"
      else
        printf 'full=yes\n'
      fi
      printf 'carried_from=%s\n' "$cv_root"
      printf 'change=%s\nnote=%s change from a checked tree; no build or test ran for this tree\n' "$cv_label" "$cv_label"
    } >"$(stamp_dir)/$cv_tree.$cv_kind.tmp.$$"
    mv "$(stamp_dir)/$cv_tree.$cv_kind.tmp.$$" "$(stamp_dir)/$cv_tree.$cv_kind"
    echo "$cv_kind: carried the check stamp of tree $cv_root to tree $cv_tree ($cv_label change: only VERSION and markdown files differ; no build or test runs)"
  done
  return "$cv_ok"
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
# decoder, the real-microphone step, or the playback pacing step's jitter
# bound. Reads the log file.
windows_flake() { # LOGFILE
  last=$(grep '^== ' "$1" | tail -n 1)
  case "$last" in *"video self-test"*) return 0 ;; esac
  grep -qiE 'microphone|mic-test|mic test' "$1" && return 0
  grep -q 'playback pacing: shown pictures are uneven' "$1" && return 0
  grep -qiE 'software' "$1" && grep -qiE 'h\.?264' "$1" && return 0
  return 1
}

# Names a failed check: the suite or area that was running (the last "== "
# step header) and the failing step's lines, one "NAME check: ..." line each.
#   failure_note NAME LOG
failure_note() {
  python3 -I scripts/flakes.py explain "$2" | sed "s/^/$1 check: /"
}

# The Windows test computers: NVRVIEW_WINDOWS_HOSTS (space-separated ssh
# aliases), else NVRVIEW_WINDOWS_HOST. One alias per line; nothing when unset.
windows_hosts() {
  # shellcheck disable=SC2086 # split on blanks on purpose
  for h in ${NVRVIEW_WINDOWS_HOSTS:-${NVRVIEW_WINDOWS_HOST:-}}; do echo "$h"; done
}

# Whether a merge starts a full check of the new main in the background
# (scripts/full-check-bg.sh). FLAG is the script's option: 1 (--full-check-after),
# 0 (--no-full-check-after) or empty. Empty falls back to
# NVRVIEW_BACKGROUND_FULL_CHECK=1|0, and then to "on when two or more Windows
# computers are configured" (one computer would be kept busy after every merge).
background_full_check_wanted() { # FLAG
  case "${1:-${NVRVIEW_BACKGROUND_FULL_CHECK:-}}" in
    1) return 0 ;;
    0) return 1 ;;
  esac
  [ "$(windows_hosts | wc -l | tr -d ' ')" -ge 2 ]
}

# Starts it for HEAD (a failure to start is only reported: the merge is done).
start_background_full_check() {
  scripts/full-check-bg.sh start HEAD || echo "the background full check could not be started; a release will run its own checks" >&2
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
  # The owner of the lock, so that a nested scripts/windows-check.sh can add a
  # second host to it for a split run (and give that one back) as the same owner.
  NVRVIEW_WINDOWS_LOCK_OWNER=$$
  export NVRVIEW_WINDOWS_LOCK_HELD NVRVIEW_WINDOWS_LOCK_OWNER
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
# with NVRVIEW_RETRIED=1, and NVRVIEW_RETRY_SINCE=<start of the first try> so
# that the suites the earlier tries passed come from the test result cache and
# only the rest runs again). Every failure is named in the .msgs file (which
# suite or area, which step), and each flake is appended to flakes.jsonl
# (scripts/flakes.py). Leaves LOGDIR/mac.log, windows.log, mac.msgs, windows.msgs
# and sets mac_rc, win_rc (0 when skipped), mac_secs, win_secs. The caller
# decides what a failure means; a lock that cannot be taken counts as one.
parallel_checks() {
  pc_logs="$1"; pc_mac="$2"; pc_win="$3"; pc_skip="$4"
  rm -f "$pc_logs/mac.rc" "$pc_logs/win.rc"
  : >"$pc_logs/mac.log"
  : >"$pc_logs/windows.log"
  : >"$pc_logs/windows.msgs"
  : >"$pc_logs/mac.msgs"
  (
    t0=$(date +%s)
    rc=0
    sh -c "$pc_mac" >"$pc_logs/mac.log" 2>&1 || rc=$?
    if [ "$rc" != 0 ]; then failure_note "Mac" "$pc_logs/mac.log" >"$pc_logs/mac.msgs"; fi
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
        since=$(date +%s)
        while :; do
          retried=""
          if [ "$try" -gt 1 ]; then retried=1; fi
          echo "== Windows check (try $try of 3)" >>"$pc_logs/windows.msgs"
          rc=0
          NVRVIEW_RETRIED=$retried NVRVIEW_RETRY_SINCE=$since sh -c "$pc_win" >"$pc_logs/windows.log" 2>&1 || rc=$?
          if [ "$rc" != 0 ]; then
            failure_note "Windows" "$pc_logs/windows.log" >>"$pc_logs/windows.msgs"
          fi
          if [ "$rc" != 0 ] && [ "$try" -lt 3 ] && windows_flake "$pc_logs/windows.log"; then
            python3 -I scripts/flakes.py record --platform windows --tree "$(git rev-parse 'HEAD^{tree}')" "$pc_logs/windows.log" || true
            echo "looks like a known environmental flake; trying again (only the suites and areas that did not pass run again; the others come from the test result cache)" >>"$pc_logs/windows.msgs"
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
