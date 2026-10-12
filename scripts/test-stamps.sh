#!/bin/sh
# Tests the stamp rules (scripts/stamps-lib.sh), the release version-only rule,
# and scripts/merge-branch.sh in throwaway repositories with fake checks. No
# real check, device, or network is involved.
#
#   scripts/test-stamps.sh
set -eu
# The caller's choices (for example a full check) must not leak in.
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H NVRVIEW_FULL_WINDOWS_EVERY
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
expect() { # NAME CMD...  (the command must succeed)
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}
expect_not() { # NAME CMD...  (the command must fail)
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then fail "$name"; else pass "$name"; fi
}

# A fresh repository with the scripts under test, main holding a few files.
new_repo() { # DIR
  mkdir -p "$1/scripts"
  cp "$here/stamps-lib.sh" "$here/checked.sh" "$here/merge-branch.sh" "$here/suite-cache.py" "$1/scripts/"
  (
    cd "$1"
    git init -q -b main .
    git config user.email test@example.com
    git config user.name test
    git config commit.gpgsign false
    echo 0.1.0 >VERSION
    for f in a b c; do echo "$f" >"$f.txt"; done
    git add -A
    git commit -q -m init
  )
}
tree() { git rev-parse "$1^{tree}"; }
stamp() { # KIND REV
  mkdir -p "$(stamp_dir)"
  echo stamp >"$(stamp_dir)/$(tree "$2").$1"
}
commit_file() { # FILE CONTENT
  echo "$2" >"$1"
  git add -A
  git commit -q -m "change $1"
}

# --- version-only rule ------------------------------------------------------
r="$work/ver"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
stamp mac HEAD
stamp windows HEAD
t0=$(tree HEAD)
expect "exact stamp is found" release_stamp_tree windows "$t0"
commit_file VERSION 0.2.0
t1=$(tree HEAD)
expect "version-only difference reuses the stamp" release_stamp_tree windows "$t1"
case "$(release_stamp_tree mac "$t1")" in
  "$t0 version-only") pass "the answer names the stamped tree" ;;
  *) fail "the answer names the stamped tree" ;;
esac
commit_file a.txt changed
t2=$(tree HEAD)
expect_not "VERSION plus another file is not accepted" release_stamp_tree windows "$t2"
git reset -q --hard HEAD~1
rm -f "$(stamp_dir)/$t0.windows"
echo stamp >"$(stamp_dir)/$t0.windows-reused"
expect_not "a reused-Windows stamp never counts for a release" release_stamp_tree windows "$t1"
# A stamp from a run that took suites from the test result cache serves
# merges but not a release (neither exactly nor by the version-only rule).
scripts/checked.sh --record windows 5 "fake" "test (3 of 18 crates)" >/dev/null
expect "a cached-suite stamp is a stamp" has_stamp windows "$t1"
expect_not "a cached-suite stamp never counts for a release" release_stamp_tree windows "$t1"
case "$(stamp_cached windows "$t1")" in "test (3 of 18 crates)") pass "the stamp names its cached suites" ;; *) fail "the stamp names its cached suites" ;; esac
scripts/checked.sh --record windows 5 "fake" none >/dev/null
expect "a full stamp counts for a release" release_stamp_tree windows "$t1"
rm -f "$(stamp_dir)/$t1.windows"

# --- reuse decision ---------------------------------------------------------
# main: init; branch X changes a.txt; main moves on with b.txt.
r="$work/dec"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
git checkout -q -b x
commit_file a.txt branch-change
git checkout -q main
merged_tree() { # BRANCH: the tree a --no-ff merge into HEAD would give
  git merge -q --no-commit --no-ff "$1" >/dev/null 2>&1 || true
  t=$(git write-tree)
  git merge --abort 2>/dev/null || true
  echo "$t"
}
expect_not "no branch stamp: run Windows" windows_reuse_decision x main "$(merged_tree x)"
stamp windows x
expect "rule A: merged tree equals the stamped branch tree" windows_reuse_decision x main "$(merged_tree x)"
commit_file b.txt main-change
m=$(merged_tree x)
expect_not "no Windows stamp for main's tree: run" windows_reuse_decision x main "$m"
stamp windows main
expect "rule B: disjoint files, both stamped" windows_reuse_decision x main "$m"
case "$(windows_reuse_decision x main "$m")" in *"rule B"*) pass "rule B is named" ;; *) fail "rule B is named" ;; esac
rm -f "$(stamp_dir)/$(tree main).windows"
echo s >"$(stamp_dir)/$(tree main).windows-reused"
expect "a reused stamp of main's tree is enough on the main side" windows_reuse_decision x main "$m"
# Overlap: both sides change the same file, in different places (no conflict).
printf 'l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n' >c.txt
git add -A && git commit -q -m "multi-line c"
stamp windows main
git checkout -q -b z
printf 'l1\nl2\nl3\nl4\nl5\nl6\nl7\nz8\n' >c.txt
git add -A && git commit -q -m "z side"
stamp windows z
git checkout -q main
printf 'm1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n' >c.txt
git add -A && git commit -q -m "main side"
stamp windows main
expect_not "same file changed on both sides: run" windows_reuse_decision z main "$(merged_tree z)"

# --- merge-branch.sh end to end with fake checks ----------------------------
# Windows stamps in the fake command stand in for windows-check.sh's own.
run_merge() { # BRANCH ... uses the env of the caller
  sh scripts/merge-branch.sh "$@"
}
r="$work/mb"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
log="$work/mb.log"
: >"$log"
export NVRVIEW_WINDOWS_HOST=fake
export NVRVIEW_MERGE_MAC_CMD="echo mac >>$log"
export NVRVIEW_MERGE_WINDOWS_CMD="echo windows >>$log; scripts/checked.sh --record windows 1 fake"
export NVRVIEW_FULL_WINDOWS_EVERY=3
stamp windows main # main's tree has a real Windows result
# A full Windows run passed just now, so the 72-hour rule forces nothing.
full_state() { # EPOCH
  mkdir -p "$(stamp_dir)/suite-cache"
  printf '{"last_full": %s, "runs_since_full": 0}\n' "$1" >"$(stamp_dir)/suite-cache/state-windows.json"
}
full_state "$(date +%s)"
# A branch with its own Windows stamp merges without a Windows run (rule A).
git checkout -q -b b1
commit_file a.txt one
stamp windows b1
git checkout -q main
run_merge b1 "merge b1" >"$work/out1" 2>&1 || { cat "$work/out1"; fail "merge b1"; }
[ "$(grep -c windows "$log")" = 0 ] && pass "rule A merge ran no Windows check" || fail "rule A merge ran no Windows check"
[ "$(grep -c mac "$log")" = 1 ] && pass "the Mac check ran once" || fail "the Mac check ran once"
expect "the merge left a Mac stamp" test -f "$(stamp_dir)/$(tree HEAD).mac"
expect "the merge left a reused-Windows stamp" test -f "$(stamp_dir)/$(tree HEAD).windows-reused"
# A branch without a stamp: Windows runs, resets the counter.
git checkout -q -b b2
commit_file b.txt two
git checkout -q main
run_merge b2 "merge b2" >"$work/out2" 2>&1 || { cat "$work/out2"; fail "merge b2"; }
[ "$(grep -c windows "$log")" = 1 ] && pass "an unstamped branch ran Windows" || fail "an unstamped branch ran Windows"
[ "$(cat "$(stamp_dir)/merges-since-windows")" = 0 ] && pass "a real run resets the counter" || fail "a real run resets the counter"
# Forced run every N merges: two stamped, disjoint merges skip, the third runs.
i=0
for b in b3 b4 b5; do
  i=$((i + 1))
  git checkout -q -b "$b" main
  commit_file "f$i.txt" "$b"
  stamp windows "$b"
  git checkout -q main
done
before=$(grep -c windows "$log")
run_merge b3 "merge b3" >"$work/out3" 2>&1 || { cat "$work/out3"; fail "merge b3"; }
run_merge b4 "merge b4" >"$work/out4" 2>&1 || { cat "$work/out4"; fail "merge b4"; }
after2=$(grep -c windows "$log")
[ "$before" = "$after2" ] && pass "two stamped merges skipped Windows" || fail "two stamped merges skipped Windows"
run_merge b5 "merge b5" >"$work/out5" 2>&1 || { cat "$work/out5"; fail "merge b5"; }
[ "$(grep -c windows "$log")" = $((after2 + 1)) ] && pass "the Nth merge forced a real Windows run" || fail "the Nth merge forced a real Windows run"
# By default no merge count forces a run; the 72-hour full-run rule does.
unset NVRVIEW_FULL_WINDOWS_EVERY
for b in b8 b9 b10 b11 b12 b13; do
  git checkout -q -b "$b" main
  commit_file "$b.txt" "$b"
  stamp windows "$b"
  git checkout -q main
done
before=$(grep -c windows "$log")
for b in b8 b9 b10 b11 b12; do
  run_merge "$b" "merge $b" >"$work/out-$b" 2>&1 || { cat "$work/out-$b"; fail "merge $b"; }
done
[ "$(grep -c windows "$log")" = "$before" ] && pass "five stamped merges skip Windows by default" || fail "five stamped merges skip Windows by default"
full_state $(($(date +%s) - 73 * 3600))
run_merge b13 "merge b13" >"$work/out-b13" 2>&1 || { cat "$work/out-b13"; fail "merge b13"; }
[ "$(grep -c windows "$log")" = $((before + 1)) ] && pass "no full Windows pass in 72 hours forces a run" || fail "no full Windows pass in 72 hours forces a run"
grep -q "a full Windows run is due" "$work/out-b13" && pass "...and says why" || fail "...and says why"
full_state "$(date +%s)"
# A known flake is retried, marked as a retry; a real failure undoes the merge.
git checkout -q -b b6
commit_file b.txt six
git checkout -q main
state="$work/flake-state"
rm -f "$state"
export NVRVIEW_MERGE_WINDOWS_CMD="if [ -f $state ]; then echo retried=\$NVRVIEW_RETRIED >>$log; scripts/checked.sh --record windows 1 fake; else touch $state; echo '== Windows app self-test'; echo 'microphone step failed'; exit 1; fi"
run_merge b6 "merge b6" >"$work/out6" 2>&1 && pass "a microphone flake was retried and passed" || { cat "$work/out6"; fail "a microphone flake was retried and passed"; }
grep -q '^retried=1$' "$log" && pass "the retry runs with NVRVIEW_RETRIED=1" || fail "the retry runs with NVRVIEW_RETRIED=1"
git checkout -q -b b7
commit_file c.txt seven
git checkout -q main
head_before=$(git rev-parse HEAD)
export NVRVIEW_MERGE_WINDOWS_CMD="echo 'tests failed'; exit 1"
if run_merge b7 "merge b7" >"$work/out7" 2>&1; then fail "a real Windows failure fails the merge"; else pass "a real Windows failure fails the merge"; fi
[ "$(git rev-parse HEAD)" = "$head_before" ] && pass "...and the merge is undone" || fail "...and the merge is undone"
export NVRVIEW_MERGE_MAC_CMD="exit 1"
if run_merge b7 "merge b7" >"$work/out8" 2>&1; then fail "a Mac failure fails the merge"; else pass "a Mac failure fails the merge"; fi
[ "$(git rev-parse HEAD)" = "$head_before" ] && [ -z "$(git status --porcelain)" ] && pass "...and leaves a clean tree" || fail "...and leaves a clean tree"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all stamp tests passed"
