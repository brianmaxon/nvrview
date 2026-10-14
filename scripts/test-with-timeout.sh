#!/bin/sh
# Tests scripts/with-timeout.py (a timeout stops the whole process tree, also a
# child in its own session, and so do a stop request and a killed wrapper) and
# scripts/swiftpm-holders.sh (stale and live holders from a fake process table).
# Fake hanging children only; no swift, no device, no network. The one short
# time limit is the thing under test; every other wait is a condition bounded
# by a count.
#
#   scripts/test-with-timeout.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
check() { # NAME CMD...
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}
# Every pid a test wrote down is killed on the way out, passing or not.
cleanup() {
  for f in "$work"/*.pid; do
    [ -f "$f" ] || continue
    kill -9 "$(cat "$f")" 2>/dev/null || true
  done
  rm -rf "$work"
}
trap cleanup EXIT
py=$(python3 -I -c 'import sys; print(sys.executable)')
wt="$here/with-timeout.py"

wait_for() { # CMD...: until it succeeds, at most 10 s
  n=0
  until "$@" >/dev/null 2>&1; do
    n=$((n + 1))
    [ "$n" -lt 200 ] || return 1
    sleep 0.05
  done
}
has_pid() { [ -s "$1" ]; }
dead() { ! kill -0 "$(cat "$1")" 2>/dev/null; }
alive_pid() { kill -0 "$(cat "$1")" 2>/dev/null; }

# 1. A hung child with a grandchild: exit 124, the named last test, both gone.
rc=0
out=$("$py" -I "$wt" 1 "fake hang" sh -c "echo 'Test Case fake started.'; sleep 300 & echo \$! >'$work/grand1.pid'; echo \$\$ >'$work/child1.pid'; wait" 2>&1) || rc=$?
[ "$rc" = 124 ] && pass "timeout exits 124" || fail "timeout exits 124 (got $rc)"
case "$out" in *"TIMEOUT: fake hang"*"last test started: Test Case fake started."*) pass "timeout names the label and the last test" ;; *) fail "timeout names the label and the last test: $out" ;; esac
check "timeout stops the child" wait_for dead "$work/child1.pid"
check "timeout stops the grandchild" wait_for dead "$work/grand1.pid"

# 2. A grandchild in its own session (not in the process group) is found by parent links.
rc=0
"$py" -I "$wt" 1 "own session" "$py" -I -c "
import subprocess, time
p = subprocess.Popen(['sleep', '300'], start_new_session=True)
open('$work/grand2.pid', 'w').write(str(p.pid))
time.sleep(300)
" >/dev/null 2>&1 || rc=$?
[ "$rc" = 124 ] && pass "own-session case exits 124" || fail "own-session case exits 124 (got $rc)"
check "timeout stops a grandchild in its own session" wait_for dead "$work/grand2.pid"

# 3. The child's own status passes through; a leftover background process is stopped.
rc=0
"$py" -I "$wt" 30 "status" sh -c "sleep 300 & echo \$! >'$work/grand3.pid'; exit 7" >/dev/null 2>&1 || rc=$?
[ "$rc" = 7 ] && pass "the command's status passes through" || fail "the command's status passes through (got $rc)"
check "a process left behind by a finished command is stopped" wait_for dead "$work/grand3.pid"

# 4. A stop request (TERM) to the wrapper stops the tree.
"$py" -I "$wt" 300 "term" sh -c "sleep 300 & echo \$! >'$work/grand4.pid'; wait" >/dev/null 2>&1 &
wrapper=$!
echo "$wrapper" >"$work/wrapper4.pid"
wait_for has_pid "$work/grand4.pid" || fail "term case started"
kill -TERM "$wrapper"
check "a stop request stops the grandchild" wait_for dead "$work/grand4.pid"
wait "$wrapper" 2>/dev/null

# 5. A wrapper killed without warning: the watcher stops the tree.
"$py" -I "$wt" 300 "kill" sh -c "sleep 300 & echo \$! >'$work/grand5.pid'; wait" >/dev/null 2>&1 &
wrapper=$!
wait_for has_pid "$work/grand5.pid" || fail "kill case started"
kill -KILL "$wrapper"
check "a killed wrapper leaves no grandchild" wait_for dead "$work/grand5.pid"
wait "$wrapper" 2>/dev/null

# 6. SwiftPM lock holders, from a fake process table (PID PPID AGE COMMAND).
holders="$here/swiftpm-holders.sh"
: >"$work/none.txt"
printf '4242 1 09:00:00 /usr/bin/swiftpm-testing-helper --test-bundle-path x\n' >"$work/stale.txt"
printf '4243 700 00:10 /usr/bin/swift-build --build-tests\n' >"$work/live.txt"
printf '4244 1 09:00:00 /bin/zsh -l\n' >"$work/other.txt"
rc=0; NVRVIEW_FAKE_PS="$work/none.txt" sh "$holders" "$here/.." >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] && pass "no holder: status 0" || fail "no holder: status 0 (got $rc)"
rc=0; out=$(NVRVIEW_FAKE_PS="$work/stale.txt" sh "$holders" "$here/.." 2>&1) || rc=$?
[ "$rc" = 3 ] && pass "an orphaned test helper is stale: status 3" || fail "an orphaned test helper is stale: status 3 (got $rc)"
case "$out" in "stale 4242 1 "*) pass "the stale holder is named" ;; *) fail "the stale holder is named: $out" ;; esac
rc=0; NVRVIEW_FAKE_PS="$work/live.txt" sh "$holders" "$here/.." >/dev/null 2>&1 || rc=$?
[ "$rc" = 4 ] && pass "a holder with a live parent: status 4" || fail "a holder with a live parent: status 4 (got $rc)"
rc=0; NVRVIEW_FAKE_PS="$work/other.txt" sh "$holders" "$here/.." >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] && pass "an unrelated process is not a holder" || fail "an unrelated process is not a holder (got $rc)"

[ "$failures" = 0 ] || { echo "$failures failure(s)" >&2; exit 1; }
echo "all passed"
