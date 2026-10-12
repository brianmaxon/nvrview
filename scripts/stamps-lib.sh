# Shared decisions about check stamps (sourced by scripts/release.sh and
# scripts/merge-branch.sh; tested by scripts/test-stamps.sh). POSIX sh.
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

# Optional exclusive use of the shared Windows computer: when
# NVRVIEW_WINDOWS_LOCK names a path, take it with mkdir (polling every 60 s)
# before a Windows run, and give it back with windows_unlock. Without the
# variable both do nothing.
windows_lock() {
  [ -n "${NVRVIEW_WINDOWS_LOCK:-}" ] || return 0
  until mkdir "$NVRVIEW_WINDOWS_LOCK" 2>/dev/null; do
    echo "waiting for the Windows lock ($NVRVIEW_WINDOWS_LOCK)..."
    sleep 60
  done
  windows_locked=1
}

windows_unlock() {
  if [ "${windows_locked:-0}" = 1 ]; then
    rmdir "$NVRVIEW_WINDOWS_LOCK" 2>/dev/null || true
    windows_locked=0
  fi
}
