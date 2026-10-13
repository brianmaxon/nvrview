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
# The Windows lock of these tests lives in the work directory.
export NVRVIEW_WINDOWS_LOCK="$work/windows.lock" NVRVIEW_LOCK_POLL=0.2
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
  cp "$here/stamps-lib.sh" "$here/suite-cache-lib.sh" "$here/windows-selftest-areas.txt" "$here/flakes.py" "$here/flakes.sh" "$here/checked.sh" "$here/merge-branch.sh" "$here/suite-cache.py" "$here/windows-lock.sh" "$here/branch-ready.sh" "$here/timing.py" "$here/secret-scan.sh" "$1/scripts/"
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
scripts/checked.sh --record windows 5 "fake" "test (1 of 18 crates)" >/dev/null
expect "a later cached run keeps the full stamp" release_stamp_tree windows "$t1"
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
expect "the merge commit has the branch as its second parent" test "$(git rev-parse -q --verify HEAD^2)" = "$(git rev-parse b1)"
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

# --- parallel checks in merge-branch.sh ---------------------------------------
# The Mac and the Windows fake check each wait for the other to start, so they
# can only pass when they really run at the same time (a condition, not a delay).
r="$work/par"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
log="$work/par.log"
: >"$log"
export NVRVIEW_WINDOWS_HOST=fake
unset NVRVIEW_FULL_WINDOWS_EVERY
full_state "$(date +%s)"
rendezvous() { # ME OTHER
  echo "touch $work/par-$1; n=0; until [ -f $work/par-$2 ]; do n=\$((n + 1)); [ \$n -lt 400 ] || exit 1; sleep 0.05; done"
}
rm -f "$work"/par-mac "$work"/par-win
export NVRVIEW_MERGE_MAC_CMD="$(rendezvous mac win); echo mac >>$log; echo '   (fake mac step: 7s)'"
export NVRVIEW_MERGE_WINDOWS_CMD="$(rendezvous win mac); echo windows >>$log; echo '   (fake windows step: 5s)'; scripts/checked.sh --record windows 1 fake"
git checkout -q -b pb1
commit_file a.txt par1
git checkout -q main
run_merge pb1 "merge pb1" >"$work/outp1" 2>&1 && pass "the Mac and Windows checks run at the same time" || { cat "$work/outp1"; fail "the Mac and Windows checks run at the same time"; }
expect "both summaries are printed" sh -c "grep -q 'Mac check: PASS' $work/outp1 && grep -q 'Windows check: PASS' $work/outp1"
expect "the slowest steps are printed" grep -q 'slowest: fake mac step 7s; fake windows step 5s' "$work/outp1"
expect "the total is printed" grep -Eq '^total [0-9]+ s;' "$work/outp1"
expect "main moved to the merge" test "$(git log -1 --format=%s)" = "merge pb1"
expect "no temporary branch is left" test -z "$(git branch --list 'merge-branch-*')"
expect "the merge left Mac and Windows stamps" sh -c "test -f '$(stamp_dir)/$(tree HEAD).mac' && test -f '$(stamp_dir)/$(tree HEAD).windows'"
timing_log="$(git rev-parse --git-common-dir)/check-timings.jsonl"
expect "the run is in the timing log" sh -c "grep -q '\"kind\":\"merge\"' $timing_log && grep -q 'fake mac step' $timing_log"
# A failure of either side drops the merge, whatever the other side did.
head_par=$(git rev-parse HEAD)
git checkout -q -b pb2
commit_file b.txt par2
git checkout -q main
export NVRVIEW_MERGE_MAC_CMD="echo 'mac broke'; exit 1"
export NVRVIEW_MERGE_WINDOWS_CMD="echo windows >>$log; scripts/checked.sh --record windows 1 fake"
run_merge pb2 "merge pb2" >"$work/outp2" 2>&1 && fail "a Mac failure fails the merge although Windows passed" || pass "a Mac failure fails the merge although Windows passed"
[ "$(git rev-parse HEAD)" = "$head_par" ] && [ "$(git symbolic-ref --short HEAD)" = main ] && [ -z "$(git status --porcelain)" ] && [ -z "$(git branch --list 'merge-branch-*')" ] &&
  pass "...main is untouched and the temporary branch is gone" || fail "...main is untouched and the temporary branch is gone"
grep -q "Windows check: PASS" "$work/outp2" && pass "...and the Windows summary is still printed" || fail "...and the Windows summary is still printed"
export NVRVIEW_MERGE_MAC_CMD="echo mac >>$log"
export NVRVIEW_MERGE_WINDOWS_CMD="echo 'windows broke'; exit 1"
git checkout -q -b pb3 main
commit_file c.txt par3
git checkout -q main
run_merge pb3 "merge pb3" >"$work/outp3" 2>&1 && fail "a Windows failure fails the merge although Mac passed" || pass "a Windows failure fails the merge although Mac passed"
[ "$(git rev-parse HEAD)" = "$head_par" ] && [ -z "$(git branch --list 'merge-branch-*')" ] && pass "...and main is untouched" || fail "...and main is untouched"
grep -q "Mac check: PASS" "$work/outp3" && pass "...and the Mac summary is still printed" || fail "...and the Mac summary is still printed"

# --- narrow retries and the flake log -------------------------------------------
# A fake Windows check wired like scripts/windows-check.sh (the real plan and
# run log of the test result cache, one suite per app self-test area) that
# fails one area the first time. The retry may run only what did not pass.
cat >"$work/fake-win.sh" <<'EOF'
set -eu
py=$(python3 -I -c 'import sys; print(sys.executable)')
. scripts/suite-cache-lib.sh
cache_start windows ${NVRVIEW_RETRIED:+--retried}
echo "try retried=${NVRVIEW_RETRIED:-} since=${NVRVIEW_RETRY_SINCE:-}" >>"$FW_RAN"
for a in $(cache_areas); do
  if cache_want "app-debug.$a" >/dev/null; then
    echo "app-debug.$a" >>"$FW_RAN"
    if [ "$a" = viewer ] && [ -f "$FW_FAIL_ONCE" ]; then
      rm -f "$FW_FAIL_ONCE"
      echo "== Windows app self-test, areas: $a (interactive session)"
      echo "  $FW_PROBLEM"
      echo "selftest: FAIL"
      sc finish "$run_file" --failed "Windows worker" >/dev/null || true
      exit 1
    fi
    cache_pass 1 "app-debug.$a"
  fi
done
sc finish "$run_file" --ok --partial >/dev/null
scripts/checked.sh --record windows 1 fake
EOF
r="$work/narrow"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
echo '{"workspace_root": "'"$r"'", "packages": []}' >"$work/empty-metadata.json"
export NVRVIEW_CACHE_METADATA="$work/empty-metadata.json" NVRVIEW_CACHE_TOOLS="fake tools" NVRVIEW_FLAKE_LOG="$work/flakes.jsonl"
export NVRVIEW_WINDOWS_HOST=fake FW_RAN="$work/fw-ran" FW_FAIL_ONCE="$work/fw-fail-once"
export NVRVIEW_MERGE_MAC_CMD="echo mac"
export NVRVIEW_MERGE_WINDOWS_CMD="sh $work/fake-win.sh"
export NVRVIEW_FULL_WINDOWS_EVERY=1
: >"$FW_RAN"
touch "$FW_FAIL_ONCE"
export FW_PROBLEM="playback pacing: shown pictures are uneven (jitter p95 9.0 ms, max 40.0 ms)"
git checkout -q -b n1
commit_file b.txt narrow1
git checkout -q main
run_merge n1 "merge n1" >"$work/outn1" 2>&1 && pass "a flaky area failure was retried and the merge succeeded" || { cat "$work/outn1"; fail "a flaky area failure was retried and the merge succeeded"; }
check_n() { grep -c "^app-debug\.$1\$" "$FW_RAN" || true; }
[ "$(check_n viewer)" = 2 ] && pass "only the failed area ran again" || fail "only the failed area ran again ($(check_n viewer) runs of viewer)"
others=$(grep '^app-debug\.' "$FW_RAN" | grep -v '^app-debug\.viewer$' | sort | uniq -d)
[ -z "$others" ] && [ "$(check_n startup)" = 1 ] && [ "$(check_n ptz)" = 1 ] && pass "...and no area that passed ran again" || fail "...and no area that passed ran again ($others)"
grep -q '^try retried=1 since=[0-9]' "$FW_RAN" && pass "the retry ran as a retry, with the start of its sequence" || fail "the retry ran as a retry, with the start of its sequence"
grep -q 'Windows check: failed in: Windows app self-test, areas: viewer' "$work/outn1" && grep -q "Windows check: failing step: $FW_PROBLEM" "$work/outn1" &&
  pass "the failure names the area and the failing step" || fail "the failure names the area and the failing step"
grep -q 'trying again' "$work/outn1" && pass "...and the retry is announced" || fail "...and the retry is announced"
retried_entries=$(find "$(git rev-parse --git-common-dir)/nvrview-checks/suite-cache/entries/windows" -name '*.retried.json' | wc -l | tr -d ' ')
[ "$retried_entries" -ge 1 ] && pass "the passes of the retry are recorded as retried" || fail "the passes of the retry are recorded as retried"
[ "$(wc -l <"$work/flakes.jsonl" | tr -d ' ')" = 1 ] && grep -q '"step":"playback pacing"' "$work/flakes.jsonl" && grep -q '"platform":"windows"' "$work/flakes.jsonl" &&
  pass "the flake is logged with its step name and platform" || fail "the flake is logged with its step name ($(cat "$work/flakes.jsonl"))"
sh scripts/flakes.sh >"$work/flakes.out" 2>&1; grep -q 'playback pacing x1' "$work/flakes.out" && pass "flakes.sh lists the most frequent flaky steps" || fail "flakes.sh lists the most frequent flaky steps ($(cat "$work/flakes.out"))"
# A failure that does not look environmental is not retried.
: >"$FW_RAN"
touch "$FW_FAIL_ONCE"
export FW_PROBLEM="tile 0: picture is flat (spread 1.0)"
git checkout -q -b n2
commit_file c.txt narrow2
git checkout -q main
head_before=$(git rev-parse HEAD)
run_merge n2 "merge n2" >"$work/outn2" 2>&1 && fail "a real failure fails the merge" || pass "a real failure fails the merge"
[ "$(grep -c '^try ' "$FW_RAN")" = 1 ] && pass "...and was not retried" || fail "...and was not retried"
grep -q 'failing step: tile 0: picture is flat' "$work/outn2" && pass "...and the failing step is named" || fail "...and the failing step is named"
[ "$(wc -l <"$work/flakes.jsonl" | tr -d ' ')" = 1 ] && [ "$(git rev-parse HEAD)" = "$head_before" ] && pass "...nor logged as a flake" || fail "...nor logged as a flake"
unset NVRVIEW_FLAKE_LOG NVRVIEW_CACHE_METADATA NVRVIEW_CACHE_TOOLS NVRVIEW_FULL_WINDOWS_EVERY

# --- timing log -----------------------------------------------------------------
export NVRVIEW_TIMING_LOG="$work/timings.jsonl" NVRVIEW_TIMING_KEEP=5
printf 'x\n   (first step: 2s)\n   (second step: 9s)\n   (third step: 4s)\n   (fourth step: 1s)\n' >"$work/steps.log"
for i in 1 2 3 4 5 6 7 8; do
  NVRVIEW_TIMING_NOW=$((1000 + i)) python3 -I scripts/timing.py log mac "$i" --tree "t$i" "$work/steps.log"
done
[ "$(wc -l <"$work/timings.jsonl" | tr -d ' ')" = 5 ] && pass "the timing log keeps only its last lines" || fail "the timing log keeps only its last lines"
[ "$(head -n 1 "$work/timings.jsonl" | sed 's/.*"total":\([0-9]*\).*/\1/')" = 4 ] && [ "$(tail -n 1 "$work/timings.jsonl" | sed 's/.*"total":\([0-9]*\).*/\1/')" = 8 ] &&
  pass "...the oldest ones go" || fail "...the oldest ones go"
grep -q '"steps":\[\["first step",2\],\["second step",9\]' "$work/timings.jsonl" && pass "a line holds the step times" || fail "a line holds the step times"
[ "$(python3 -I scripts/timing.py slowest "$work/steps.log")" = "4 steps, 16s of step time; slowest: second step 9s; third step 4s; first step 2s" ] &&
  pass "the slowest three steps are named" || fail "the slowest three steps are named"
python3 -I scripts/timing.py table "$work/steps.log" | grep -q "second step *9s" && pass "the table lists each step" || fail "the table lists each step"
grep -q '^KEEP = 500$' scripts/timing.py && pass "the default limit is 500 lines" || fail "the default limit is 500 lines"
unset NVRVIEW_TIMING_LOG NVRVIEW_TIMING_KEEP

# --- the incremental secret scan ----------------------------------------------
r="$work/scan"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
calls="$work/gl.calls"
: >"$calls"
cat >"$work/fakegl.sh" <<EOF
echo "\$*" >>"$calls"
case "\$1" in detect) echo "7 commits scanned." ;; esac
[ ! -f "$work/leak" ]
EOF
export NVRVIEW_GITLEAKS_CMD="sh $work/fakegl.sh \"\$@\"" NVRVIEW_SCAN_NOW=1000000
unset NVRVIEW_FULL_CHECK
rm -f "$work/leak"
scan() { sh scripts/secret-scan.sh 2>&1; }
detects() { grep -c '^detect' "$calls" || true; }
scan >"$work/scan1" && pass "the first scan passes" || fail "the first scan passes"
grep -q "full history: no earlier scan on record" "$work/scan1" && [ "$(detects)" = 1 ] && ! grep -q 'log-opts' "$calls" &&
  pass "the first scan is a full-history scan" || fail "the first scan is a full-history scan"
state="$(stamp_dir)/secret-scan.state"
grep -q "^clean=$(git rev-parse HEAD)$" "$state" && pass "the clean commit is recorded in the common git directory" || fail "the clean commit is recorded in the common git directory"
first=$(git rev-parse HEAD)
NVRVIEW_SCAN_NOW=1000100 scan >"$work/scan2"
grep -q "no commits since the last clean scan" "$work/scan2" && [ "$(detects)" = 1 ] && pass "nothing new: nothing is scanned" || fail "nothing new: nothing is scanned"
commit_file a.txt one
NVRVIEW_SCAN_NOW=1000200 scan >"$work/scan3"
grep -q "incremental): 1 new commits" "$work/scan3" && grep -q "log-opts=HEAD --not $first" "$calls" && pass "a new commit: only the range since the clean scan" || fail "a new commit: only the range since the clean scan"
second=$(git rev-parse HEAD)
echo staged >s.txt
git add s.txt
echo edit >>a.txt
NVRVIEW_SCAN_NOW=1000300 scan >"$work/scan4"
grep -q '^protect --staged' "$calls" && grep -q '^protect --redact' "$calls" && pass "staged and unstaged changes are scanned too" || fail "staged and unstaged changes are scanned too"
git reset -q --hard
git clean -qfd
touch "$work/leak"
commit_file a.txt leak
leaky=$(git rev-parse HEAD)
before_state=$(cat "$state")
NVRVIEW_SCAN_NOW=1000400 scan >"$work/scan5" && fail "a finding fails the scan" || pass "a finding fails the scan"
[ "$(cat "$state")" = "$before_state" ] && pass "a failed scan is never recorded as clean" || fail "a failed scan is never recorded as clean"
rm -f "$work/leak"
: >"$calls"
NVRVIEW_SCAN_NOW=1000500 scan >"$work/scan6"
grep -q "log-opts=HEAD --not $second" "$calls" && pass "the next scan covers the same range again" || fail "the next scan covers the same range again"
grep -q "^clean=$leaky$" "$state" && pass "...and records it once it passes" || fail "...and records it once it passes"
: >"$calls"
NVRVIEW_SCAN_NOW=$((1000000 + 25 * 3600)) scan >"$work/scan7"
grep -q "full history: the last full scan was 25 h ago" "$work/scan7" && pass "a full scan runs when the last one is 24 hours old" || fail "a full scan runs when the last one is 24 hours old"
NVRVIEW_SCAN_NOW=$((1000000 + 26 * 3600)) scan >"$work/scan8"
grep -q "no commits since" "$work/scan8" && pass "...and the full scan restarts the 24 hours" || fail "...and the full scan restarts the 24 hours"
NVRVIEW_SECRET_FULL_MAX_AGE_H=48 NVRVIEW_SCAN_NOW=$((1000000 + 60 * 3600)) scan >"$work/scan9"
grep -q "no commits since" "$work/scan9" && pass "the age limit can be changed" || fail "the age limit can be changed"
unset NVRVIEW_SECRET_FULL_MAX_AGE_H # (variables set in front of a function call stay set)
NVRVIEW_FULL_CHECK=1 scan >"$work/scan10"
grep -q "full history: NVRVIEW_FULL_CHECK=1" "$work/scan10" && pass "NVRVIEW_FULL_CHECK=1 (releases) scans everything" || fail "NVRVIEW_FULL_CHECK=1 (releases) scans everything"
unset NVRVIEW_FULL_CHECK
echo "0000000000000000000000000000000000000000:a.txt:generic:1" >.gitleaksignore
git add .gitleaksignore
git commit -q -m "ignore list"
NVRVIEW_SCAN_NOW=$((1000000 + 60 * 3600)) scan >"$work/scan11"
grep -q "full history: the gitleaks rules or ignore list changed" "$work/scan11" && pass "a changed ignore list forces a full scan" || fail "a changed ignore list forces a full scan"
touch "$work/leak"
before_state=$(cat "$state")
NVRVIEW_FULL_CHECK=1 scan >"$work/scan12" && fail "a failing full scan fails" || pass "a failing full scan fails"
[ "$(cat "$state")" = "$before_state" ] && pass "...and is not recorded either" || fail "...and is not recorded either"
rm -f "$work/leak"
unset NVRVIEW_GITLEAKS_CMD NVRVIEW_SCAN_NOW
expect "release-check.sh forces full scans through NVRVIEW_FULL_CHECK=1" grep -q 'export NVRVIEW_FULL_CHECK=1' "$here/release-check.sh"


echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all stamp tests passed"
