#!/bin/sh
# Tests the full check that follows a merge (scripts/full-check-bg.sh, the
# "background" Windows lock priority, and the merge scripts' start of it) in
# throwaway repositories with fake checks: no real check, device, or network,
# and no waiting on the clock (conditions only, bounded by counts).
#
#   scripts/test-full-check-bg.sh
set -eu
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H NVRVIEW_FULL_WINDOWS_EVERY \
  NVRVIEW_WINDOWS_HOSTS NVRVIEW_WINDOWS_HOST NVRVIEW_WINDOWS_PRIORITY NVRVIEW_WINDOWS_LOCK_HELD NVRVIEW_WINDOWS_LOCK_OWNER \
  NVRVIEW_BACKGROUND_FULL_CHECK NVRVIEW_FULLCHECK_CMD NVRVIEW_FULLCHECK_DIR NVRVIEW_FULLCHECK_WORKER
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap '"$here/full-check-bg.sh" stop >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
check() { # NAME CONDITION-COMMAND...
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}
wait_for() { # CMD...: until it succeeds (a condition, bounded by a count)
  n=0
  until "$@" >/dev/null 2>&1; do
    n=$((n + 1))
    [ "$n" -lt 600 ] || return 1
    sleep 0.05
  done
}
nonempty() { [ -s "$1" ]; }

new_repo() { # DIR
  mkdir -p "$1/scripts"
  cp "$here/stamps-lib.sh" "$here/checked.sh" "$here/merge-train.sh" "$here/branch-ready.sh" "$here/windows-lock.sh" \
    "$here/suite-cache.py" "$here/timing.py" "$here/flakes.py" "$here/full-check-bg.sh" "$1/scripts/"
  (
    cd "$1"
    git init -q -b main .
    git config user.email test@example.com
    git config user.name test
    git config commit.gpgsign false
    printf '%s\n' .worktrees/ >.gitignore
    echo 0.1.0 >VERSION
    for f in a b c; do echo "$f" >"$f.txt"; done
    printf 'lock-1\nlock-2\n' >Cargo.lock
    git add -A
    git commit -q -m init
  )
}
commit_file() { # NAME CONTENT
  printf '%s\n' "$2" >"$1"
  git add -A
  git commit -q -m "change $1"
}

# The fake check: records where and how it ran, then passes.
cat >"$work/fake-check.sh" <<'EOF'
#!/bin/sh
{
  echo "dir=$(pwd)"
  echo "head=$(git rev-parse HEAD)"
  echo "priority=${NVRVIEW_WINDOWS_PRIORITY:-}"
  echo "worker=${NVRVIEW_WINDOWS_WORKER:-}"
} >"$FAKE_OUT.tmp"
mv "$FAKE_OUT.tmp" "$FAKE_OUT"
EOF
# A check that takes a Windows lock, says so, and then stays until stopped.
cat >"$work/hang-check.sh" <<'EOF'
#!/bin/sh
scripts/windows-lock.sh acquire --owner-pid $$ >"$FAKE_OUT.host"
echo "hung=$$" >"$FAKE_OUT.hung"
sleep 600
EOF
chmod +x "$work/fake-check.sh" "$work/hang-check.sh"

export NVRVIEW_WINDOWS_LOCK="$work/lock/windows.lock" NVRVIEW_LOCK_POLL=0.05 NVRVIEW_WINDOWS_HOST=win-test-1
export FAKE_OUT="$work/fake.out"

# --- a background check of HEAD -------------------------------------------------
r="$work/repo1"
new_repo "$r"
cd "$r"
. scripts/stamps-lib.sh
export NVRVIEW_FULLCHECK_CMD="$work/fake-check.sh"
sh scripts/full-check-bg.sh start >"$work/start1.out" 2>&1 && pass "start works" || { cat "$work/start1.out"; fail "start works"; }
wait_for grep -q '^pass$' "$(stamp_dir)/fullcheck/result" && pass "the check ran and passed" || fail "the check ran and passed"
check "it ran in its own worktree, not in the main checkout" grep -q "^dir=.*/.worktrees/fullcheck$" "$FAKE_OUT"
check "...at HEAD" grep -q "^head=$(git rev-parse HEAD)$" "$FAKE_OUT"
check "...at the background Windows lock priority" grep -q '^priority=background$' "$FAKE_OUT"
check "...with its own Windows worker" grep -q '^worker=fullcheck$' "$FAKE_OUT"
check "the main checkout was not touched" test -z "$(git status --porcelain)"
sh scripts/full-check-bg.sh status | grep -q 'not running; last:.*pass' && pass "status reports the last result" || fail "status reports the last result"

# --- a newer start supersedes an older one -----------------------------------------
rm -f "$FAKE_OUT" "$FAKE_OUT.host" "$FAKE_OUT.hung"
commit_file b.txt two
c2=$(git rev-parse HEAD)
NVRVIEW_FULLCHECK_CMD="$work/hang-check.sh" sh scripts/full-check-bg.sh start >"$work/start2.out" 2>&1
wait_for nonempty "$FAKE_OUT.hung" && pass "an older check is running" || fail "an older check is running"
old_pid=$(cat "$(stamp_dir)/fullcheck/pid")
kill -0 "$old_pid" 2>/dev/null && pass "...and its process is alive" || fail "...and its process is alive"
check "...holding a Windows computer" test -d "$NVRVIEW_WINDOWS_LOCK-win-test-1"
commit_file c.txt three
c3=$(git rev-parse HEAD)
sh scripts/full-check-bg.sh start >"$work/start3.out" 2>&1
grep -q 'stopping the older check' "$work/start3.out" && pass "a newer start says it stops the older check" || { cat "$work/start3.out"; fail "a newer start says it stops the older check"; }
kill -0 "$old_pid" 2>/dev/null && fail "the older check is stopped" || pass "the older check is stopped"
[ -z "$(pgrep -g "$old_pid" 2>/dev/null || true)" ] && pass "...with everything it started" || fail "...with everything it started"
wait_for grep -q '^pass$' "$(stamp_dir)/fullcheck/result" && pass "the newer check ran and passed" || fail "the newer check ran and passed"
check "...for the newer commit" grep -q "^head=$c3$" "$FAKE_OUT"
check "the older check's Windows lock is given back" test ! -d "$NVRVIEW_WINDOWS_LOCK-win-test-1"

# --- nothing starts for a tree that has full stamps ----------------------------------
rm -f "$FAKE_OUT"
commit_file a.txt four
scripts/checked.sh --record mac 1 fake >/dev/null
scripts/checked.sh --record windows 1 fake >/dev/null
sh scripts/full-check-bg.sh start >"$work/start4.out" 2>&1
grep -q 'already has full Mac and Windows stamps' "$work/start4.out" && pass "a tree with full stamps is not checked again" || fail "a tree with full stamps is not checked again"
check "...and no check ran" test ! -e "$FAKE_OUT"
# A stamp with cached suites is not a full one.
commit_file a.txt five
scripts/checked.sh --record mac 1 fake >/dev/null
scripts/checked.sh --record windows 1 fake "clippy" >/dev/null
sh scripts/full-check-bg.sh start >"$work/start5.out" 2>&1
wait_for nonempty "$FAKE_OUT" && pass "a tree whose Windows stamp used cached suites is checked" || fail "a tree whose Windows stamp used cached suites is checked"
wait_for grep -q '^pass$' "$(stamp_dir)/fullcheck/result" || true

# --- the background priority ---------------------------------------------------------
lock="$here/windows-lock.sh"
export NVRVIEW_WINDOWS_LOCK="$work/l2/windows.lock" NVRVIEW_WINDOWS_HOSTS="win-test-1 win-test-2"
unset NVRVIEW_WINDOWS_HOST
mkdir -p "$NVRVIEW_WINDOWS_LOCK.waiters"
printf 'pid=%s\nmachine=%s\nhosts=win-test-1 win-test-2\n' "$$" "$(hostname)" >"$NVRVIEW_WINDOWS_LOCK.waiters/agent-1-1"
got=$(sh "$lock" acquire --try --priority background --owner-pid "$$" 2>/dev/null || echo none)
[ "$got" = none ] && pass "a background try does not take a host a waiting agent wants" || fail "a background try does not take a host a waiting agent wants ($got)"
rm -f "$NVRVIEW_WINDOWS_LOCK.waiters/agent-1-1"
printf 'pid=%s\nmachine=%s\nhosts=win-test-1 win-test-2\n' "$$" "$(hostname)" >"$NVRVIEW_WINDOWS_LOCK.waiters/merge-1-1"
got=$(sh "$lock" acquire --try --priority background --owner-pid "$$" 2>/dev/null || echo none)
[ "$got" = none ] && pass "nor one a waiting merge wants" || fail "nor one a waiting merge wants ($got)"
rm -f "$NVRVIEW_WINDOWS_LOCK.waiters/merge-1-1"
got=$(sh "$lock" acquire --try --priority background --owner-pid "$$" 2>/dev/null || echo none)
[ "$got" = win-test-1 ] && pass "with nobody waiting a background try takes a free host" || fail "with nobody waiting a background try takes a free host ($got)"
sh "$lock" release --owner-pid "$$"
sh "$lock" acquire --try --priority nonsense --owner-pid "$$" >/dev/null 2>&1 && fail "an unknown priority is refused" || pass "an unknown priority is refused"

# --- the merge scripts start it -------------------------------------------------------
export NVRVIEW_MERGE_MAC_CMD="true" NVRVIEW_MERGE_WINDOWS_CMD="scripts/checked.sh --record windows 1 fake clippy"
export NVRVIEW_FULLCHECK_CMD="$work/fake-check.sh"
train() { # NAME HOSTS FLAGS...: a fresh repo with one branch, then a train over it
  name="$1"; hosts="$2"; shift 2
  r="$work/$name"
  new_repo "$r"
  cd "$r"
  git checkout -q -b "br-$name" main
  commit_file "f-$name.txt" one
  git checkout -q main
  rm -f "$FAKE_OUT"
  export NVRVIEW_WINDOWS_LOCK="$work/lock-$name/windows.lock" NVRVIEW_WINDOWS_HOSTS="$hosts"
  sh scripts/merge-train.sh "$@" "br-$name" >"$work/$name.out" 2>&1 || { cat "$work/$name.out"; return 1; }
}
train two "win-test-1 win-test-2" && pass "a train on two hosts passes" || fail "a train on two hosts passes"
. scripts/stamps-lib.sh
wait_for grep -q "^head=$(git rev-parse HEAD)$" "$FAKE_OUT" && pass "...and starts a full check of the new main by default" || { cat "$work/two.out"; fail "...and starts a full check of the new main by default"; }
check "...at background priority" grep -q '^priority=background$' "$FAKE_OUT"
train one "win-test-1" && pass "a train on one host passes" || fail "a train on one host passes"
[ ! -e "$(stamp_dir)/fullcheck/commit" ] && pass "...and starts no full check by default" || fail "...and starts no full check by default"
train off "win-test-1 win-test-2" --no-full-check-after && pass "a train with --no-full-check-after passes" || fail "a train with --no-full-check-after passes"
[ ! -e "$(stamp_dir)/fullcheck/commit" ] && pass "...and starts none on two hosts either" || fail "...and starts none on two hosts either"
train on "win-test-1" --full-check-after && pass "a train with --full-check-after passes" || fail "a train with --full-check-after passes"
. scripts/stamps-lib.sh
wait_for grep -q "^head=$(git rev-parse HEAD)$" "$FAKE_OUT" && pass "...and starts one on a single host" || fail "...and starts one on a single host"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all full check tests passed"
