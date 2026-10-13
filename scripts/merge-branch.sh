#!/bin/sh
# Merges a finished branch into the current branch (run it in the checkout
# that holds main), checks the result, and skips the Windows check when the
# branch's own Windows result already covers it.
#
#   scripts/merge-branch.sh BRANCH "merge commit message"
#
# 1. git merge --no-commit --no-ff BRANCH, then the Windows decision (step 3)
#    on the merged tree.
# 2. The merge is committed on a temporary branch (so the checks see a clean
#    committed tree and leave stamps for it), and the Mac check (no docs-only
#    fast path: NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh; cached suites from
#    scripts/suite-cache.py allowed and listed) and the Windows check run AT
#    THE SAME TIME, each with its own log. The current branch moves to the
#    merge (a fast-forward) only when both pass; otherwise it is untouched.
# 3. The Windows check is skipped when scripts/stamps-lib.sh
#    (windows_reuse_decision) says the branch's Windows stamp still covers the
#    result:
#      rule A  the merged tree equals the branch's own tree, which has a
#              Windows stamp (agents run the full Windows check on a clean
#              committed tree, and stamps are shared through the common git
#              directory); or
#      rule B  the files that differ between the merged tree and the branch's
#              stamped tree are exactly the files main changed since the
#              merge base, main's tree before the merge has a Windows result,
#              and no file was changed on both sides.
#    A skipped merge leaves a windows-reused stamp (never enough for a
#    release). The Windows check runs regardless when a full Windows run is
#    due (none passed in NVRVIEW_FULL_MAX_AGE_H hours, default 72; see
#    scripts/suite-cache.py), and, when NVRVIEW_FULL_WINDOWS_EVERY=N is set
#    (off by default), every N merges since the last real Windows run.
#    Otherwise NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh runs, up to 3
#    tries when the failure looks like a known environmental flake (a
#    software H.264 decoder fallback in the video self-test, the real
#    microphone step). A failed check names the suite or area that was
#    running and the failing step's lines (scripts/flakes.py explain). A
#    retry runs only what did not pass: the suites and self-test areas the
#    earlier tries passed come from the test result cache
#    (NVRVIEW_RETRY_SINCE marks where the sequence began, so this holds in a
#    full run too). Passes of a retry are recorded as retried and never let a
#    suite be skipped later. Each flake is appended to flakes.jsonl in the
#    common git directory (scripts/flakes.sh lists the most frequent). A
#    failure that does not look like a flake is not retried. The script
#    prints which suites, on either side, came from the test result cache.
# 4. If a check fails, the merge is dropped and the script exits 1. It ends
#    with the total time and the slowest three steps (scripts/timing.py), and
#    adds a line to the timing log.
#
# Environment: NVRVIEW_WINDOWS_HOST or NVRVIEW_WINDOWS_HOSTS (needed when
# Windows runs), NVRVIEW_WINDOWS_WORKER, NVRVIEW_WINDOWS_LOCK (see
# scripts/windows-lock.sh; merges take the lock at priority "merge"),
# NVRVIEW_FULL_WINDOWS_EVERY. For tests of this script:
# NVRVIEW_MERGE_MAC_CMD and NVRVIEW_MERGE_WINDOWS_CMD replace the two checks.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh

die() { echo "merge-branch: $*" >&2; exit 1; }

# Merges get the Windows computer before agents' checks (scripts/windows-lock.sh).
NVRVIEW_WINDOWS_PRIORITY=merge
export NVRVIEW_WINDOWS_PRIORITY

[ $# -eq 2 ] || { echo "usage: $0 BRANCH \"merge commit message\"" >&2; exit 2; }
branch="$1"
message="$2"
every="${NVRVIEW_FULL_WINDOWS_EVERY:-0}"
mac_cmd="${NVRVIEW_MERGE_MAC_CMD:-NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh}"
win_cmd="${NVRVIEW_MERGE_WINDOWS_CMD:-NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh}"

[ -z "$(git status --porcelain)" ] || die "the working tree is not clean"
tip=$(git rev-parse --verify "$branch^{commit}") || die "no such branch: $branch"
orig=$(git symbolic-ref --short HEAD) || die "HEAD is not on a branch"
pre=$(git rev-parse HEAD)
start=$(date +%s)
scripts/branch-ready.sh "$branch" || true
dir=$(stamp_dir)
mkdir -p "$dir"
counter="$dir/merges-since-windows"
logs=$(mktemp -d)
tmpb="merge-branch-$$"
finished=0

# Back to the original branch with nothing of the merge left, unless it
# finished (then the temporary branch is already gone).
cleanup() {
  windows_unlock_any
  if [ "$finished" != 1 ]; then
    git merge --abort 2>/dev/null || true
    if [ "$(git symbolic-ref --short HEAD 2>/dev/null)" = "$tmpb" ]; then
      git reset -q --hard "$pre" 2>/dev/null || true
      git checkout -q "$orig" 2>/dev/null || true
    fi
    git branch -q -D "$tmpb" 2>/dev/null || true
  fi
  rm -rf "$logs"
}
trap cleanup EXIT

# The merge happens on a temporary branch: the checks need a clean committed
# tree, and the current branch moves only when both pass. The branch is made
# before merging, because switching branches mid-merge drops MERGE_HEAD and
# the commit would lose its second parent.
git checkout -q -b "$tmpb" || die "could not create the temporary branch $tmpb"
if ! git merge --no-commit --no-ff "$branch" >"$logs/merge.log" 2>&1; then
  cat "$logs/merge.log" >&2
  die "the merge has conflicts; resolve them in the branch (merge main into it) and retry"
fi
if ! git rev-parse -q --verify MERGE_HEAD >/dev/null; then
  die "nothing to merge: $(cat "$logs/merge.log")"
fi
merged=$(git write-tree) || die "the merge left unmerged paths"

count=0
[ -f "$counter" ] && count=$(cat "$counter")
case "$count" in '' | *[!0-9]*) count=0 ;; esac

echo "== Windows decision"
force=""
if [ "$every" -gt 0 ] && [ "$((count + 1))" -ge "$every" ]; then
  force="$count merges since the last real Windows run; every $every merges one runs"
elif due=$(python3 -I scripts/suite-cache.py full-due windows); then
  force="a full Windows run is due ($due)"
fi
if reason=$(windows_reuse_decision "$tip" "$pre" "$merged"); then
  skip=1
else
  skip=0
fi
echo "$reason"
if [ "$skip" = 1 ] && [ -n "$force" ]; then
  echo "run anyway: $force"
  skip=0
fi
if [ "$skip" = 0 ]; then
  [ -n "$(windows_hosts)" ] || die "set NVRVIEW_WINDOWS_HOST (or NVRVIEW_WINDOWS_HOSTS) to the SSH host alias of the Windows computer"
fi

git commit -q -m "$message" || die "the commit failed (hook?); the merge was dropped"
[ "$(git rev-parse 'HEAD^{tree}')" = "$merged" ] || die "the committed tree differs from the merged tree"
[ "$(git rev-parse -q --verify 'HEAD^2')" = "$tip" ] || die "the merge commit does not have $branch as its second parent"
tree=$merged

if [ "$skip" = 1 ]; then
  echo "== Mac check on the merged tree (Windows check skipped)"
else
  echo "== Mac check and Windows check on the merged tree, at the same time"
fi
parallel_checks "$logs" "$mac_cmd" "$win_cmd" "$skip"

mac_cached=$(sed -n 's/.*; cached suites: //p' "$logs/mac.log" | tail -n 1)
if [ "$mac_rc" = 0 ]; then
  echo "Mac check: PASS in ${mac_secs}s"
  tail -n 2 "$logs/mac.log"
  echo "Mac suites taken from the test result cache: ${mac_cached:-none}"
else
  echo "Mac check: FAIL (exit $mac_rc) after ${mac_secs}s" >&2
  cat "$logs/mac.msgs" >&2
  tail -n 25 "$logs/mac.log" >&2
fi
if [ "$skip" = 1 ]; then
  echo "Windows check: skipped"
elif [ "$win_rc" = 0 ]; then
  echo "Windows check: PASS in ${win_secs}s"
  grep -E 'trying again|try [0-9] of|check: ' "$logs/windows.msgs" || true
  tail -n 3 "$logs/windows.log"
  echo "Windows suites taken from the test result cache: $(sed -n 's/.*; cached suites: //p' "$logs/windows.log" | tail -n 1)"
else
  echo "Windows check: FAIL (exit $win_rc) after ${win_secs}s" >&2
  cat "$logs/windows.msgs" >&2
  tail -n 25 "$logs/windows.log" >&2
fi
if [ "$mac_rc" != 0 ] || [ "$win_rc" != 0 ]; then
  report_timings merge-failed "$(($(date +%s) - start))" "$tree" "$logs/mac.log" "$logs/windows.log" >&2
  if [ "$mac_rc" != 0 ] && [ "$win_rc" != 0 ]; then die "both checks failed; the merge was dropped"; fi
  if [ "$mac_rc" != 0 ]; then die "the Mac check failed; the merge was dropped"; fi
  die "the Windows check failed; the merge was dropped"
fi
[ -z "$(git status --porcelain --untracked-files=all)" ] || die "a check changed files; the merge was dropped"

scripts/checked.sh --record mac "$mac_secs" "check on the merge of $branch" "${mac_cached:-none}" >/dev/null
if [ "$skip" = 1 ]; then
  scripts/checked.sh --record windows-reused 0 "no Windows run: $reason" >/dev/null
  echo "$((count + 1))" >"$counter"
  echo "Windows check skipped ($((count + 1)) merges since the last real run)."
  branch_cached=$(stamp_cached windows "$(git rev-parse "$tip^{tree}")")
  echo "Windows suites the branch's stamp took from the test result cache: ${branch_cached:-none}"
else
  echo 0 >"$counter"
fi

# Both passed: move the original branch to the merge.
finished=1
git checkout -q "$orig"
git merge -q --ff-only "$tmpb" || die "$orig moved while the checks ran; the merge is on branch $tmpb"
git branch -q -D "$tmpb"
total=$(($(date +%s) - start))
report_timings merge "$total" "$tree" "$logs/mac.log" "$logs/windows.log"
echo "Merged $branch as $(git rev-parse --short HEAD)."
