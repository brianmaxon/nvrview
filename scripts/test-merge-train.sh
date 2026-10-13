#!/bin/sh
# Tests scripts/windows-lock.sh (priority order, stale locks, host pool),
# scripts/branch-ready.sh, and scripts/merge-train.sh in throwaway
# repositories with fake checks. No real check, device, or network is involved.
#
#   scripts/test-merge-train.sh
set -eu
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H NVRVIEW_FULL_WINDOWS_EVERY NVRVIEW_WINDOWS_HOSTS NVRVIEW_WINDOWS_PRIORITY NVRVIEW_WINDOWS_LOCK_HELD
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
bgpids=""
cleanup() {
  for p in $bgpids; do kill "$p" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
check() { # NAME CONDITION-COMMAND...
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}
# Waits until the command succeeds (a condition, never a guessed delay).
wait_for() { # CMD...
  n=0
  until "$@" >/dev/null 2>&1; do
    n=$((n + 1))
    [ "$n" -lt 400 ] || return 1
    sleep 0.05
  done
}
nonempty() { [ -s "$1" ]; }
waiter_count() { # PRIORITY
  [ "$(ls "$NVRVIEW_WINDOWS_LOCK.waiters" 2>/dev/null | grep -c "^$1-")" -ge "$2" ]
}
longlived() { # sets $lp to the pid of a process that stays alive
  sleep 600 &
  lp=$!
  bgpids="$bgpids $lp"
}
lock="$here/windows-lock.sh"

# --- the Windows lock ---------------------------------------------------------
export NVRVIEW_WINDOWS_LOCK="$work/l1/windows.lock" NVRVIEW_LOCK_POLL=0.05 NVRVIEW_WINDOWS_HOST=win-test-1
longlived; holder=$lp
longlived; other=$lp
got=$(sh "$lock" acquire --priority agent --owner-pid "$holder" 2>/dev/null)
[ "$got" = win-test-1 ] && pass "acquire prints the host" || fail "acquire prints the host"
check "the lock records its owner" grep -q "^pid=$holder$" "$NVRVIEW_WINDOWS_LOCK-win-test-1/owner"
sh "$lock" release --owner-pid "$other"
check "release by another owner leaves the lock" test -d "$NVRVIEW_WINDOWS_LOCK-win-test-1"
sh "$lock" release --owner-pid "$holder"
check "release by the owner removes it" test ! -d "$NVRVIEW_WINDOWS_LOCK-win-test-1"

# Priority: a merge waiter beats an agent waiter that came earlier.
sh "$lock" acquire --priority agent --owner-pid "$holder" >/dev/null 2>&1
sh "$lock" acquire --priority agent --owner-pid "$other" >"$work/agent.out" 2>/dev/null &
bgpids="$bgpids $!"
wait_for waiter_count agent 1
longlived; mergepid=$lp
sh "$lock" acquire --priority merge --owner-pid "$mergepid" >"$work/merge.out" 2>/dev/null &
bgpids="$bgpids $!"
wait_for waiter_count merge 1
sh "$lock" release --owner-pid "$holder"
wait_for nonempty "$work/merge.out"
check "the merge waiter got the host first" grep -q win-test-1 "$work/merge.out"
check "...while the earlier agent waiter still waits" test ! -s "$work/agent.out"
sh "$lock" release --owner-pid "$mergepid"
wait_for nonempty "$work/agent.out"
check "the agent gets it after the merge" grep -q win-test-1 "$work/agent.out"
sh "$lock" release --owner-pid "$other"

# A dead owner's lock is broken after the stale time, with a log line.
export NVRVIEW_WINDOWS_LOCK="$work/l2/windows.lock" NVRVIEW_LOCK_STALE_SECONDS=1
sleep 0 &
dead=$!
wait "$dead" || true
sh "$lock" acquire --owner-pid "$dead" >/dev/null 2>&1
# The stale time is the thing under test, so this one waits about a second.
got=$(sh "$lock" acquire --owner-pid "$holder" 2>"$work/stale.err")
[ "$got" = win-test-1 ] && pass "a stale lock is broken and taken" || fail "a stale lock is broken and taken"
check "...with a log line" grep -q "breaking the stale lock" "$work/stale.err"
check "a live owner's lock was not broken first" grep -q "^pid=$holder$" "$NVRVIEW_WINDOWS_LOCK-win-test-1/owner"
sh "$lock" release --owner-pid "$holder"
# A lock made by hand (no owner file) is never broken.
mkdir -p "$NVRVIEW_WINDOWS_LOCK-win-test-1"
sh "$lock" acquire --owner-pid "$holder" >"$work/hand.out" 2>/dev/null &
handpid=$!
bgpids="$bgpids $handpid"
wait_for waiter_count agent 1
rmdir "$NVRVIEW_WINDOWS_LOCK-win-test-1"
wait "$handpid" || true
check "a hand-made lock is waited for, not broken" grep -q win-test-1 "$work/hand.out"
sh "$lock" release --owner-pid "$holder"
unset NVRVIEW_LOCK_STALE_SECONDS

# Host pool: the first free host is taken; one lock per host.
export NVRVIEW_WINDOWS_LOCK="$work/l3/windows.lock" NVRVIEW_WINDOWS_HOSTS="win-test-1 win-test-2"
a=$(sh "$lock" acquire --owner-pid "$holder" 2>/dev/null)
b=$(sh "$lock" acquire --owner-pid "$other" 2>/dev/null)
[ "$a" = win-test-1 ] && [ "$b" = win-test-2 ] && pass "the pool hands out the first free host" || fail "the pool hands out the first free host ($a, $b)"
sh "$lock" release --owner-pid "$holder"
c=$(sh "$lock" acquire --owner-pid "$mergepid" 2>/dev/null)
[ "$c" = win-test-1 ] && pass "a freed first host is chosen again" || fail "a freed first host is chosen again ($c)"
sh "$lock" release --owner-pid "$other"
d=$(sh "$lock" acquire --owner-pid "$holder" 2>/dev/null)
[ "$d" = win-test-2 ] && pass "the free second host is chosen when the first is busy" || fail "the free second host is chosen when the first is busy ($d)"
sh "$lock" release --owner-pid "$holder"
sh "$lock" release --owner-pid "$mergepid"
# Locks are named after the host, not its place in the list: while the first
# host is held, a check that lists only the second host gets it at once.
a=$(sh "$lock" acquire --owner-pid "$holder" 2>/dev/null)
NVRVIEW_WINDOWS_HOSTS=win-test-2 sh "$lock" acquire --owner-pid "$other" >"$work/alone.out" 2>/dev/null &
alonepid=$!
bgpids="$bgpids $alonepid"
if wait_for nonempty "$work/alone.out"; then
  check "a host listed alone has its own lock" grep -q win-test-2 "$work/alone.out"
else
  fail "a host listed alone has its own lock (it waited for the first host's)"
  kill "$alonepid" 2>/dev/null || true
fi
NVRVIEW_WINDOWS_HOSTS=win-test-2 sh "$lock" release --owner-pid "$other"
sh "$lock" release --owner-pid "$holder"
unset NVRVIEW_WINDOWS_HOSTS

# --- merge trains -------------------------------------------------------------
export NVRVIEW_WINDOWS_LOCK="$work/l4/windows.lock"
new_repo() { # DIR
  mkdir -p "$1/scripts"
  cp "$here/stamps-lib.sh" "$here/checked.sh" "$here/merge-train.sh" "$here/branch-ready.sh" "$here/windows-lock.sh" "$here/suite-cache.py" "$here/timing.py" "$here/flakes.py" "$1/scripts/"
  (
    cd "$1"
    git init -q -b main .
    git config user.email test@example.com
    git config user.name test
    git config commit.gpgsign false
    echo 0.1.0 >VERSION
    for f in a b c; do echo "$f" >"$f.txt"; done
    printf 'lock-1\nlock-2\nlock-3\nlock-4\nlock-5\nlock-6\n' >Cargo.lock
    git add -A
    git commit -q -m init
  )
}
branch_with() { # NAME FILE CONTENT
  git checkout -q -b "$1" main
  printf '%s\n' "$3" >"$2"
  git add -A
  git commit -q -m "change $2"
  git checkout -q main
}
tree() { git rev-parse "$1^{tree}"; }
stamp() { # KIND REV
  mkdir -p "$(stamp_dir)"
  echo stamp >"$(stamp_dir)/$(tree "$2").$1"
}
full_state() { # EPOCH
  mkdir -p "$(stamp_dir)/suite-cache"
  printf '{"last_full": %s, "runs_since_full": 0}\n' "$1" >"$(stamp_dir)/suite-cache/state-windows.json"
}
export NVRVIEW_WINDOWS_HOST=win-test-1
log="$work/train.log"
# The Mac check fails when a file holds MACFAIL; Windows when one holds WINFAIL.
export NVRVIEW_MERGE_MAC_CMD="echo mac >>$log; ! grep -rqs MACFAIL --include='*.txt' ."
export NVRVIEW_MERGE_WINDOWS_CMD="echo windows >>$log; ! grep -rqs WINFAIL --include='*.txt' . && scripts/checked.sh --record windows 1 fake"

# A train of 4: t1 and t2 conflict (t2 is skipped), t3 fails the Mac check,
# t4 is fine. The bisection isolates t3.
r="$work/train1"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
: >"$log"
full_state "$(date +%s)"
branch_with t1 a.txt one
branch_with t2 a.txt two
branch_with t3 f3.txt MACFAIL
branch_with t4 f4.txt four
head0=$(git rev-parse HEAD)
rc=0
sh scripts/merge-train.sh "t1=Merge t1 message" t2 t3 t4 >"$work/out1" 2>&1 || rc=$?
[ "$rc" = 1 ] && pass "a train with a skipped and a failed branch exits 1" || { cat "$work/out1"; fail "a train with a skipped and a failed branch exits 1 (rc $rc)"; }
check "t1 merged" grep -Eq '^t1 +merged' "$work/out1"
check "t2 skipped for its conflict" grep -Eq '^t2 +skipped \(conflict\)' "$work/out1"
check "t3 failed the Mac check" grep -Eq '^t3 +failed \(Mac check\)' "$work/out1"
check "t4 merged" grep -Eq '^t4 +merged' "$work/out1"
check "main has t1 and t4" test -f f4.txt
check "main lacks the failing branch" test ! -e f3.txt
check "main lacks the conflicting branch" test "$(cat a.txt)" = one
check "each branch keeps its own merge commit and message" git log --format=%s --merges -1 --grep="Merge t1 message"
check "the tree is clean on main" test -z "$(git status --porcelain)"
check "the temporary branch is gone" test -z "$(git branch --list 'merge-train-*')"
[ "$(git symbolic-ref --short HEAD)" = main ] && pass "main is checked out again" || fail "main is checked out again"
check "the final tree has a Mac stamp" test -f "$(stamp_dir)/$(tree HEAD).mac"
check "the failure log tail was printed" grep -q "failed the Mac check" "$work/out1"
check "branch-ready was printed per branch" grep -q "^t4: tree" "$work/out1"
[ "$(grep -c windows "$log")" -ge 1 ] && pass "Windows ran for the unstamped branches" || fail "Windows ran for the unstamped branches"
# The Mac check ran: whole train (fail), t1 (pass), t3+t4 (fail), t3 (fail), t4 (pass).
[ "$(grep -c mac "$log")" = 5 ] && pass "the bisection ran the Mac check 5 times" || fail "the bisection ran the Mac check 5 times ($(grep -c mac "$log"))"

# A clean train of 3 runs each check once; Windows ran once.
r="$work/train2"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
: >"$log"
full_state "$(date +%s)"
branch_with u1 f1.txt one
branch_with u2 f2.txt two
branch_with u3 f3.txt three
sh scripts/merge-train.sh u1 u2 u3 >"$work/out2" 2>&1 && pass "a clean train exits 0" || { cat "$work/out2"; fail "a clean train exits 0"; }
[ "$(grep -c mac "$log")" = 1 ] && [ "$(grep -c windows "$log")" = 1 ] && pass "one Mac and one Windows check for 3 branches" || fail "one Mac and one Windows check for 3 branches"
[ "$(git rev-list --merges --count main)" = 3 ] && pass "three merge commits" || fail "three merge commits"
check "the final tree has Mac and Windows stamps" test -f "$(stamp_dir)/$(tree HEAD).windows"

# Branches with Windows stamps (and a stamped main) skip the Windows check.
r="$work/train3"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
: >"$log"
full_state "$(date +%s)"
stamp windows main
branch_with v1 f1.txt one
branch_with v2 f2.txt two
stamp windows v1
stamp windows v2
sh scripts/merge-train.sh v1 v2 >"$work/out3" 2>&1 && pass "a stamped train exits 0" || { cat "$work/out3"; fail "a stamped train exits 0"; }
[ "$(grep -c windows "$log")" = 0 ] && pass "stamped disjoint branches skip the Windows check" || fail "stamped disjoint branches skip the Windows check"
check "the final tree has a reused-Windows stamp" test -f "$(stamp_dir)/$(tree HEAD).windows-reused"
check "no stand-in stamps are left for the intermediate tree" test "$(ls "$(stamp_dir)" | grep -c windows-reused)" = 1
check "branch-ready says a branch with only a Windows stamp is not ready" test "$(sh scripts/branch-ready.sh v1 >/dev/null 2>&1 && echo ready || echo no)" = no
stamp mac v1
check "branch-ready accepts a branch with both stamps" sh scripts/branch-ready.sh v1
check "branch-ready says a stamped Windows result is present" sh -c 'sh scripts/branch-ready.sh v1 2>&1 | grep -q "Windows full"'
# Rerun: a Windows failure in a train fails (and isolates) the branch.
branch_with v3 f3.txt WINFAIL
head3=$(git rev-parse HEAD)
sh scripts/merge-train.sh v3 >"$work/out3b" 2>&1 && fail "a Windows failure fails the train" || pass "a Windows failure fails the train"
check "...naming the Windows check" grep -Eq '^v3 +failed \(Windows check\)' "$work/out3b"
[ "$(git rev-parse HEAD)" = "$head3" ] && pass "...and leaves main alone" || fail "...and leaves main alone"
check "branch-ready flags an unstamped branch" test "$(sh scripts/branch-ready.sh v3 >/dev/null 2>&1 && echo ready || echo no)" = no

# Cargo.lock: two branches change the same lines; the conflict is resolved by
# regenerating the lock.
r="$work/train4"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
: >"$log"
full_state "$(date +%s)"
git checkout -q -b w1 main
sed 's/lock-3/lock-3-w1/' Cargo.lock >Cargo.lock.new && mv Cargo.lock.new Cargo.lock
echo one >f1.txt
git add -A && git commit -q -m w1
git checkout -q -b w2 main
sed 's/lock-3/lock-3-w2/' Cargo.lock >Cargo.lock.new && mv Cargo.lock.new Cargo.lock
echo two >f2.txt
git add -A && git commit -q -m w2
git checkout -q main
export NVRVIEW_TRAIN_LOCK_CMD="printf 'regenerated\\n' >Cargo.lock"
sh scripts/merge-train.sh w1 w2 >"$work/out4" 2>&1 && pass "a Cargo.lock conflict does not stop the train" || { cat "$work/out4"; fail "a Cargo.lock conflict does not stop the train"; }
check "both branches merged" test -f f1.txt -a -f f2.txt
check "Cargo.lock was regenerated and committed" test "$(cat Cargo.lock)" = regenerated
check "the tree is clean after the regeneration" test -z "$(git status --porcelain)"
check "the resolution was announced" grep -q "Cargo.lock conflict in w2 resolved" "$work/out4"
unset NVRVIEW_TRAIN_LOCK_CMD

# Stray changes made by a check are refused.
r="$work/train5"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
full_state "$(date +%s)"
branch_with x1 f1.txt one
head5=$(git rev-parse HEAD)
NVRVIEW_MERGE_MAC_CMD="echo stray >stray.dat" sh scripts/merge-train.sh x1 >"$work/out5" 2>&1 && fail "a check that leaves stray files fails the train" || pass "a check that leaves stray files fails the train"
check "...and says it changed files" grep -q "it changed files" "$work/out5"
[ "$(git rev-parse HEAD)" = "$head5" ] && pass "...and main is untouched" || fail "...and main is untouched"
rm -f stray.dat


# The Mac and Windows checks of a train run at the same time: each fake check
# waits for the other to start, which only works when they overlap.
r="$work/train6"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
: >"$log"
full_state "$(date +%s)"
rendezvous() { # ME OTHER
  echo "touch $work/tr-$1; n=0; until [ -f $work/tr-$2 ]; do n=\$((n + 1)); [ \$n -lt 400 ] || exit 1; sleep 0.05; done"
}
rm -f "$work"/tr-mac "$work"/tr-win
branch_with y1 f1.txt one
branch_with y2 f2.txt two
NVRVIEW_MERGE_MAC_CMD="$(rendezvous mac win); echo mac >>$log; echo '   (fake mac step: 6s)'" \
  NVRVIEW_MERGE_WINDOWS_CMD="$(rendezvous win mac); echo windows >>$log; scripts/checked.sh --record windows 1 fake" \
  sh scripts/merge-train.sh y1 y2 >"$work/out6" 2>&1 && pass "a train runs its Mac and Windows checks at the same time" || { cat "$work/out6"; fail "a train runs its Mac and Windows checks at the same time"; }
check "the train prints both summaries" sh -c "grep -q 'Mac check: PASS' $work/out6 && grep -q 'Windows check: PASS' $work/out6"
check "the train prints the total and the slowest steps" sh -c "grep -q 'slowest: fake mac step 6s' $work/out6"
check "the train is in the timing log" sh -c "grep -q '\"kind\":\"train\"' '$(git rev-parse --git-common-dir)/check-timings.jsonl'"
# One side failing fails the train (here the Windows side, while Mac passes).
branch_with y3 f3.txt WINFAIL
head6=$(git rev-parse HEAD)
NVRVIEW_MERGE_MAC_CMD="echo mac >>$log" \
  NVRVIEW_MERGE_WINDOWS_CMD="echo windows >>$log; ! grep -rqs WINFAIL --include='*.txt' ." \
  sh scripts/merge-train.sh y3 >"$work/out6b" 2>&1 && fail "a Windows failure fails a parallel train" || pass "a Windows failure fails a parallel train"
check "...naming the Windows check" grep -Eq '^y3 +failed \(Windows check\)' "$work/out6b"
[ "$(git rev-parse HEAD)" = "$head6" ] && pass "...and main stays where it was" || fail "...and main stays where it was"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all merge train tests passed"
