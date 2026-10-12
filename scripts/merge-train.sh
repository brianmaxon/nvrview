#!/bin/sh
# Merges several finished branches together: one Mac check and one Windows
# check for the whole train instead of one pair per branch.
#
#   scripts/merge-train.sh [--messages FILE] BRANCH[=MESSAGE]...
#
# Run it in the checkout that holds main (clean tree). Each branch gets its
# own merge commit (message: its "=MESSAGE", else a line "BRANCH=MESSAGE" in
# FILE, else "Merge branch 'BRANCH'"), made in the order given on a temporary
# branch on top of main.
#
# 1. A branch that conflicts textually is skipped (reported) and the rest
#    continue. A conflict only in Cargo.lock is resolved by regenerating it
#    (NVRVIEW_TRAIN_LOCK_CMD, default "cargo metadata --format-version 1",
#    which fixes the lock to match the manifests and keeps locked versions)
#    and committing it. After the merges, Cargo.lock is regenerated the same
#    way if any branch touched it. A check that leaves any other change in the
#    tree fails the train (stray changes are refused).
# 2. The Mac check (NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh, cached suites
#    allowed) and the Windows check run once each on the final tree, at the
#    same time with their own logs (a failure of either fails the train). The
#    Windows check is skipped when scripts/stamps-lib.sh
#    (windows_reuse_decision, rules A and B of scripts/merge-branch.sh) covers
#    every merge step in turn (each step's "main" is the train so far), and no
#    full Windows run is due. Windows takes the lock at priority merge.
# 3. On success main is fast-forwarded to the train and the stamps for the
#    final tree are recorded. On a check failure the train is bisected: the
#    merged branches are split in halves, the first half is tested, then the
#    second on top of what passed, and so on until the failing branch is
#    isolated. The passing branches are merged; the failing one is reported
#    with the tail of its log.
#
# Prints a table: branch, merged / skipped (conflict) / failed (which check).
# Exits 0 only when every branch merged. Also prints scripts/branch-ready.sh
# for each branch first.
#
# Environment: as scripts/merge-branch.sh (NVRVIEW_MERGE_MAC_CMD and
# NVRVIEW_MERGE_WINDOWS_CMD replace the checks in tests), plus
# NVRVIEW_TRAIN_LOCK_CMD.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh

die() { echo "merge-train: $*" >&2; exit 1; }

NVRVIEW_WINDOWS_PRIORITY=merge
export NVRVIEW_WINDOWS_PRIORITY

mac_cmd="${NVRVIEW_MERGE_MAC_CMD:-NVRVIEW_DOCS_FAST_PATH=0 scripts/check.sh}"
win_cmd="${NVRVIEW_MERGE_WINDOWS_CMD:-NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh}"
lock_cmd="${NVRVIEW_TRAIN_LOCK_CMD:-cargo metadata --format-version 1 >/dev/null}"
every="${NVRVIEW_FULL_WINDOWS_EVERY:-0}"

logs=$(mktemp -d)
keep="${TMPDIR:-/tmp}/merge-train-logs.$$"
: >"$logs/messages"
: >"$logs/results"
branches=""
while [ $# -gt 0 ]; do
  case "$1" in
    --messages)
      [ $# -ge 2 ] || { echo "--messages needs a file" >&2; exit 2; }
      cat "$2" >>"$logs/messages"
      shift 2 ;;
    -*) echo "usage: $0 [--messages FILE] BRANCH[=MESSAGE]..." >&2; exit 2 ;;
    *=*)
      printf '%s\n' "$1" >>"$logs/messages"
      branches="$branches ${1%%=*}"
      shift ;;
    *) branches="$branches $1"; shift ;;
  esac
done
[ -n "$branches" ] || { echo "usage: $0 [--messages FILE] BRANCH[=MESSAGE]..." >&2; exit 2; }

msg_of() { # BRANCH
  m=$(awk -v b="$1" 'index($0, b "=") == 1 { print substr($0, length(b) + 2); exit }' "$logs/messages")
  printf '%s\n' "${m:-Merge branch '$1'}"
}

orig=$(git symbolic-ref --short HEAD) || die "HEAD is not on a branch"
[ -z "$(git status --porcelain)" ] || die "the working tree is not clean"
for b in $branches; do
  git rev-parse --verify "$b^{commit}" >/dev/null 2>&1 || die "no such branch: $b"
done
pre=$(git rev-parse HEAD)
tmpb="merge-train-$$"
dir=$(stamp_dir)
mkdir -p "$dir"
counter="$dir/merges-since-windows"
finished=0

# The intermediate trees of a train get a stand-in windows-reused stamp while
# the decision runs (so the next step sees the train so far as covered).
drop_temp_stamps() {
  [ -f "$logs/temp-stamps" ] || return 0
  while read -r f; do rm -f "$f"; done <"$logs/temp-stamps"
  rm -f "$logs/temp-stamps"
}

cleanup() {
  drop_temp_stamps
  if [ "$finished" != 1 ]; then
    git merge --abort 2>/dev/null || true
    if [ "$(git symbolic-ref --short HEAD 2>/dev/null)" = "$tmpb" ]; then
      git reset -q --hard "$pre" 2>/dev/null || true
      git checkout -q "$orig" 2>/dev/null || true
      git branch -q -D "$tmpb" 2>/dev/null || true
    fi
  fi
  windows_unlock_any
  rm -rf "$logs"
}
trap cleanup EXIT

echo "== readiness of the branches"
for b in $branches; do scripts/branch-ready.sh "$b" || true; done

record() { printf '%s|%s|%s\n' "$1" "$2" "$3" >>"$logs/results"; }
recorded() { grep -q "^$1|" "$logs/results"; }

git checkout -q -b "$tmpb"

# Merges the given branches onto HEAD in order. Sets $built to the ones that
# merged (conflicting ones are recorded and skipped). Regenerates Cargo.lock
# when a branch touched it.
build() { # BRANCH...
  built=""
  base_b=$(git rev-parse HEAD)
  for b in "$@"; do
    if git merge --no-ff -m "$(msg_of "$b")" "$b" >"$logs/merge.log" 2>&1; then
      built="$built $b"
      continue
    fi
    unmerged=$(git diff --name-only --diff-filter=U)
    if [ "$unmerged" = "Cargo.lock" ] && git checkout --ours -- Cargo.lock && sh -c "$lock_cmd" >>"$logs/merge.log" 2>&1 &&
      git add Cargo.lock && git commit -q --no-edit >>"$logs/merge.log" 2>&1; then
      echo "Cargo.lock conflict in $b resolved by regenerating it"
      built="$built $b"
      continue
    fi
    git merge --abort 2>/dev/null || git reset -q --hard HEAD
    echo "conflict merging $b: $(echo "$unmerged" | head -n 3 | tr '\n' ' ')"
    recorded "$b" || record "$b" "skipped (conflict)" "$(echo "$unmerged" | head -n 3 | tr '\n' ' ')"
  done
  if [ -n "$built" ] && ! git diff --quiet "$base_b" HEAD -- Cargo.lock; then
    sh -c "$lock_cmd" >"$logs/lock.log" 2>&1 || { cat "$logs/lock.log" >&2; die "regenerating Cargo.lock failed"; }
    if ! git diff --quiet -- Cargo.lock; then
      git add Cargo.lock
      git commit -q -m "Regenerate Cargo.lock after the merge train"
    fi
  fi
}

# Whether every merge step since $1 is covered by the branches' Windows
# stamps (each step judged with the train so far as "main"). Prints the reason
# for the first step that is not.
train_windows_decision() { # BASE
  tcount=0
  for c in $(git rev-list --first-parent --reverse "$1..HEAD"); do
    parents=$(git rev-list --no-walk --parents "$c" | wc -w | tr -d ' ')
    if [ "$parents" != 3 ]; then
      echo "run: $(git log -1 --format=%s "$c") is not a plain merge"
      return 1
    fi
    reason=$(windows_reuse_decision "$c^2" "$c^1" "$(git rev-parse "$c^{tree}")") || { echo "$reason"; return 1; }
    t=$(git rev-parse "$c^{tree}")
    if ! has_stamp windows "$t" && ! has_stamp windows-reused "$t"; then
      echo "train step" >"$dir/$t.windows-reused"
      echo "$dir/$t.windows-reused" >>"$logs/temp-stamps"
    fi
    tcount=$((tcount + 1))
  done
  echo "every one of the $tcount merge steps is covered by its branch's Windows result"
}

# Tries one group of branches on top of what has been accepted so far. The
# Mac check and the Windows check (unless the branches' own Windows results
# cover it) run at the same time. Passing: the branches stay merged and are
# recorded. Failing: HEAD goes back, $built names the branches that merged
# (the ones to split), $fail_kind the check.
attempt() { # BRANCH...
  base=$(git rev-parse HEAD)
  build "$@"
  if [ -z "$built" ]; then return 0; fi
  if [ "$(git rev-parse HEAD)" = "$base" ]; then
    for b in $built; do record "$b" "merged" "already contained in main"; done
    return 0
  fi
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
  if reason=$(train_windows_decision "$base"); then skip=1; else skip=0; fi
  drop_temp_stamps
  echo "$reason"
  if [ "$skip" = 1 ] && [ -n "$force" ]; then
    echo "run anyway: $force"
    skip=0
  fi
  if [ "$skip" = 0 ]; then
    [ -n "$(windows_hosts)" ] || die "set NVRVIEW_WINDOWS_HOST (or NVRVIEW_WINDOWS_HOSTS) to the SSH host alias of the Windows computer"
    echo "== Mac check and Windows check on the train:$built, at the same time"
  else
    echo "== Mac check on the train:$built (Windows check skipped)"
  fi
  attempt_start=$(date +%s)
  parallel_checks "$logs" "$mac_cmd" "$win_cmd" "$skip"
  if [ "$mac_rc" != 0 ]; then
    fail_kind="Mac check"
    cp "$logs/mac.log" "$logs/fail.log"
    git reset -q --hard "$base"
    return 1
  fi
  if [ -n "$(git status --porcelain --untracked-files=all)" ]; then
    fail_kind="Mac check (it changed files: $(git status --porcelain | head -n 3 | tr '\n' ' '))"
    cp "$logs/mac.log" "$logs/fail.log"
    git reset -q --hard "$base"
    return 1
  fi
  if [ "$win_rc" != 0 ]; then
    fail_kind="Windows check"
    { cat "$logs/windows.msgs"; cat "$logs/windows.log"; } >"$logs/fail.log"
    git reset -q --hard "$base"
    return 1
  fi
  echo "Mac check: PASS in ${mac_secs}s"
  tail -n 2 "$logs/mac.log"
  mac_cached=$(sed -n 's/.*; cached suites: //p' "$logs/mac.log" | tail -n 1)
  echo "Mac suites taken from the test result cache: ${mac_cached:-none}"
  scripts/checked.sh --record mac "$mac_secs" "check on the merge train" "${mac_cached:-none}" >/dev/null
  if [ "$skip" = 1 ]; then
    scripts/checked.sh --record windows-reused 0 "no Windows run: $reason" >/dev/null
    echo "$((count + 1))" >"$counter"
    echo "Windows check skipped."
    win_note="Windows reused"
  else
    echo "Windows check: PASS in ${win_secs}s"
    tail -n 3 "$logs/windows.log"
    echo 0 >"$counter"
    win_note="Windows ran"
  fi
  report_timings train "$(($(date +%s) - attempt_start))" "$(git rev-parse 'HEAD^{tree}')" "$logs/mac.log" "$logs/windows.log"
  for b in $built; do record "$b" "merged" "Mac ran; $win_note"; done
  return 0
}

# Work list: one group per line, first line next. Failures split their group.
printf '%s\n' "${branches# }" >"$logs/work"
while [ -s "$logs/work" ]; do
  group=$(head -n 1 "$logs/work")
  tail -n +2 "$logs/work" >"$logs/work.rest"
  mv "$logs/work.rest" "$logs/work"
  [ -n "$group" ] || continue
  # shellcheck disable=SC2086 # a list of branch names without blanks
  if attempt $group; then continue; fi
  # shellcheck disable=SC2086
  set -- $built
  if [ $# -le 1 ]; then
    b="$1"
    mkdir -p "$keep"
    cp "$logs/fail.log" "$keep/$(echo "$b" | tr '/' '_').log"
    record "$b" "failed ($fail_kind)" "log: $keep/$(echo "$b" | tr '/' '_').log"
    echo "== $b failed the $fail_kind; last lines:" >&2
    tail -n 15 "$logs/fail.log" >&2
    continue
  fi
  echo "== the train failed the $fail_kind; splitting $# branches"
  half=$(($# / 2))
  first=""
  rest=""
  i=0
  for b in "$@"; do
    i=$((i + 1))
    if [ "$i" -le "$half" ]; then first="$first $b"; else rest="$rest $b"; fi
  done
  { printf '%s\n' "${first# }"; printf '%s\n' "${rest# }"; cat "$logs/work"; } >"$logs/work.new"
  mv "$logs/work.new" "$logs/work"
done

# Fast-forward the original branch to the train.
train_head=$(git rev-parse HEAD)
git checkout -q "$orig"
if [ "$train_head" != "$(git rev-parse HEAD)" ]; then
  git merge -q --ff-only "$tmpb" || die "$orig moved while the train ran; the train is on branch $tmpb"
fi
git branch -q -D "$tmpb"
finished=1

echo
echo "== merge train result"
printf '%-40s %-28s %s\n' "branch" "result" "detail"
bad=0
for b in $branches; do
  line=$(grep "^$b|" "$logs/results" | head -n 1 || true)
  if [ -z "$line" ]; then line="$b|not run|"; fi
  res=$(echo "$line" | cut -d'|' -f2)
  [ "$res" = merged ] || bad=1
  printf '%-40s %-28s %s\n' "$b" "$res" "$(echo "$line" | cut -d'|' -f3-)"
done
echo "$orig is now at $(git rev-parse --short HEAD)."
[ "$bad" = 0 ]
