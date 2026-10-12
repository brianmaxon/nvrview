#!/bin/sh
# Merges a finished branch into the current branch (run it in the checkout
# that holds main), checks the result, and skips the Windows check when the
# branch's own Windows result already covers it.
#
#   scripts/merge-branch.sh BRANCH "merge commit message"
#
# 1. git merge --no-commit --no-ff BRANCH, then the Mac check without the
#    docs-only fast path (NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh) on the
#    merged working tree. It may take suites from the test result cache
#    (scripts/suite-cache.py); the script prints which.
# 2. The merge is committed and the Mac stamp recorded for the merged tree.
# 3. Windows: skipped when scripts/stamps-lib.sh (windows_reuse_decision)
#    says the branch's Windows stamp still covers the result:
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
#    microphone step); passes of a retry are recorded as retried and never
#    let a suite be skipped later. The script prints which suites, on
#    either side, came from the test result cache.
# 4. If a check fails, the merge is undone (merge --abort before the commit,
#    reset to the old HEAD after it) and the script exits 1.
#
# Environment: NVRVIEW_WINDOWS_HOST (needed when Windows runs),
# NVRVIEW_WINDOWS_WORKER, NVRVIEW_WINDOWS_LOCK (see scripts/stamps-lib.sh),
# NVRVIEW_FULL_WINDOWS_EVERY. For tests of this script:
# NVRVIEW_MERGE_MAC_CMD and NVRVIEW_MERGE_WINDOWS_CMD replace the two checks.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh

die() { echo "merge-branch: $*" >&2; exit 1; }

[ $# -eq 2 ] || { echo "usage: $0 BRANCH \"merge commit message\"" >&2; exit 2; }
branch="$1"
message="$2"
every="${NVRVIEW_FULL_WINDOWS_EVERY:-0}"
mac_cmd="${NVRVIEW_MERGE_MAC_CMD:-NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh}"
win_cmd="${NVRVIEW_MERGE_WINDOWS_CMD:-NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh}"

[ -z "$(git status --porcelain)" ] || die "the working tree is not clean"
tip=$(git rev-parse --verify "$branch^{commit}") || die "no such branch: $branch"
pre=$(git rev-parse HEAD)
dir=$(stamp_dir)
mkdir -p "$dir"
counter="$dir/merges-since-windows"
logs=$(mktemp -d)
trap 'windows_unlock; rm -rf "$logs"' EXIT

if ! git merge --no-commit --no-ff "$branch" >"$logs/merge.log" 2>&1; then
  cat "$logs/merge.log" >&2
  git merge --abort 2>/dev/null || true
  die "the merge has conflicts; resolve them in the branch (merge main into it) and retry"
fi
if ! git rev-parse -q --verify MERGE_HEAD >/dev/null; then
  die "nothing to merge: $(cat "$logs/merge.log")"
fi
merged=$(git write-tree) || { git merge --abort; die "the merge left unmerged paths"; }

echo "== Mac check on the merged tree"
mac_start=$(date +%s)
if ! sh -c "$mac_cmd" >"$logs/mac.log" 2>&1; then
  tail -n 25 "$logs/mac.log" >&2
  git merge --abort 2>/dev/null || git reset -q --hard "$pre"
  die "the Mac check failed; the merge was undone"
fi
tail -n 2 "$logs/mac.log"
mac_cached=$(sed -n 's/.*; cached suites: //p' "$logs/mac.log" | tail -n 1)
echo "Mac suites taken from the test result cache: ${mac_cached:-none}"
mac_secs=$(($(date +%s) - mac_start))
[ -z "$(git status --porcelain --untracked-files=all | grep -v '^[MADRC] ')" ] ||
  { git merge --abort 2>/dev/null || git reset -q --hard "$pre"; die "the Mac check changed files; the merge was undone"; }

git commit -q -m "$message" || { git merge --abort 2>/dev/null || true; die "the commit failed (hook?); the merge was undone"; }
[ "$(git rev-parse 'HEAD^{tree}')" = "$merged" ] || die "the committed tree differs from the checked tree"
scripts/checked.sh --record mac "$mac_secs" "check on the merge of $branch" "${mac_cached:-none}" >/dev/null

undo() {
  if [ "$(git rev-parse HEAD)" = "$merge_commit" ] && [ -z "$(git status --porcelain)" ]; then
    git reset -q --hard "$pre"
    echo "merge-branch: the merge was undone (HEAD is back at $pre)" >&2
  else
    echo "merge-branch: HEAD or the tree changed since the merge; left as is (merge commit $merge_commit, previous HEAD $pre)" >&2
  fi
}
merge_commit=$(git rev-parse HEAD)

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

if [ "$skip" = 1 ]; then
  scripts/checked.sh --record windows-reused 0 "no Windows run: $reason" >/dev/null
  echo "$((count + 1))" >"$counter"
  echo "Windows check skipped ($((count + 1)) merges since the last real run)."
  branch_cached=$(stamp_cached windows "$(git rev-parse "$tip^{tree}")")
  echo "Windows suites the branch's stamp took from the test result cache: ${branch_cached:-none}"
else
  : "${NVRVIEW_WINDOWS_HOST:?set NVRVIEW_WINDOWS_HOST to the SSH host alias of the Windows computer}"
  windows_lock
  try=1
  while :; do
    echo "== Windows check on the merged tree (try $try of 3)"
    # A retry after a known flake: its passes never let a suite be skipped.
    retried=""
    if [ "$try" -gt 1 ]; then retried=1; fi
    if NVRVIEW_RETRIED=$retried sh -c "$win_cmd" >"$logs/windows.log" 2>&1; then
      tail -n 3 "$logs/windows.log"
      echo "Windows suites taken from the test result cache: $(sed -n 's/.*; cached suites: //p' "$logs/windows.log" | tail -n 1)"
      break
    fi
    tail -n 25 "$logs/windows.log" >&2
    if [ "$try" -lt 3 ] && windows_flake "$logs/windows.log"; then
      echo "merge-branch: looks like a known environmental flake; trying again" >&2
      try=$((try + 1))
      continue
    fi
    windows_unlock
    undo
    die "the Windows check failed"
  done
  windows_unlock
  echo 0 >"$counter"
fi
echo "Merged $branch as $(git rev-parse --short HEAD)."
