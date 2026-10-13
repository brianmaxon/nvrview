#!/bin/sh
# Tests splitting one Windows check over two computers (scripts/windows-check.sh,
# scripts/windows-split.py) in throwaway repositories with a fake ssh: no real
# Windows computer, check, or network, and no waiting on the clock (the fake
# halves wait for each other's start marker, which proves they ran at the same
# time, and for nothing else).
#
#   scripts/test-windows-split.sh
set -eu
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H NVRVIEW_FULL_WINDOWS_EVERY \
  NVRVIEW_WINDOWS_HOSTS NVRVIEW_WINDOWS_HOST NVRVIEW_WINDOWS_PRIORITY NVRVIEW_WINDOWS_LOCK_HELD NVRVIEW_WINDOWS_LOCK_OWNER \
  NVRVIEW_WINDOWS_ONLY NVRVIEW_APP_AREAS NVRVIEW_RETRIED NVRVIEW_WINDOWS_SPLIT NVRVIEW_WINDOWS_SPEEDS NVRVIEW_SPLIT_MIN_SECONDS
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
check() { # NAME CONDITION-COMMAND...
  name="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}

# --- the planner (windows-split.py) ------------------------------------------
plan() { python3 -I "$here/windows-split.py" plan --timings "$work/timings.jsonl" "$@"; }
cat >"$work/timings.jsonl" <<'EOF'
{"time":1,"kind":"windows","total":600,"tree":"t","steps":[["clippy (--workspace)",150],["tests (build: --workspace)",170],["tests (run, per-test time limit)",60],["doctests (--workspace)",10],["build: release library and mock",100],["build: C# app",50],["wait for the background builds",5],["Windows app plain start, first run (interactive session)",9],["Windows app self-test, areas: all (interactive session)",80]]}
EOF
out=$(plan --hosts fast slow --rust clippy,test,doctest --suites app-debug.startup,app-debug.viewer)
case "$out" in "split fast=rust slow=rest" | "split slow=rust fast=rest") pass "the planner splits a check with work in both halves" ;; *) fail "the planner splits a check with work in both halves ($out)" ;; esac
out=$(plan --hosts fast slow --speeds "fast=2 slow=1" --rust clippy,test,doctest --suites app-debug.startup,app-debug.viewer)
case "$out" in "split fast=rust slow=rest") pass "the faster computer takes the longer half" ;; *) fail "the faster computer takes the longer half ($out)" ;; esac
out=$(plan --hosts fast slow --rust none --suites app-debug.startup)
case "$out" in single*) pass "no Rust work: no split" ;; *) fail "no Rust work: no split ($out)" ;; esac
out=$(plan --hosts fast slow --rust clippy --suites none)
case "$out" in single*) pass "only Rust work: no split" ;; *) fail "only Rust work: no split ($out)" ;; esac
out=$(plan --hosts fast slow --rust doctest --suites app-debug.startup --min-seconds 30)
case "$out" in single*) pass "a tiny half is not worth a second computer" ;; *) fail "a tiny half is not worth a second computer ($out)" ;; esac
out=$(plan --hosts fast slow --speeds "fast=10 slow=1" --rust clippy,test,doctest --suites app-debug.startup)
case "$out" in single*) pass "a computer too slow to help is not used" ;; *) fail "a computer too slow to help is not used ($out)" ;; esac
out=$(python3 -I "$here/windows-split.py" plan --timings "$work/none.jsonl" --hosts a b --rust clippy --suites csharp-smoke)
case "$out" in split*) pass "without history the check splits" ;; *) fail "without history the check splits ($out)" ;; esac

# --- windows-check.sh with a fake ssh ------------------------------------------
# A throwaway repository holding the scripts under test.
new_repo() { # DIR
  mkdir -p "$1/scripts" "$1.bin"
  for f in windows-check.sh stamps-lib.sh suite-cache-lib.sh suite-cache.py windows-selftest-areas.txt flakes.py checked.sh windows-lock.sh windows-split.py timing.py; do
    cp "$here/$f" "$1/scripts/"
  done
  cp "$work/fake-ssh" "$1.bin/ssh"
  cp "$work/fake-scp" "$1.bin/scp"
  # One crate, so the Rust half has work.
  cat >"$1.metadata.json" <<META
{"workspace_root": "$1", "packages": [{"name": "a", "manifest_path": "$1/crates/a/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []}]}
META
  (
    cd "$1"
    git init -q -b main .
    git config user.email test@example.com
    git config user.name test
    git config commit.gpgsign false
    echo 0.1.0 >VERSION
    echo a >a.txt
    mkdir -p crates/a/src
    echo "// a" >crates/a/src/lib.rs
    echo "[package]" >crates/a/Cargo.toml
    echo "[workspace]" >Cargo.toml
    echo "# lock" >Cargo.lock
    git add -A
    git commit -q -m init
  )
}
# The fake ssh answers like the Windows computer: connected, free disk space,
# tool versions, an unpacked tree, and the worker. The worker prints the steps
# and "suite-result" lines of the suites it was asked for; the two halves of a
# split run wait for each other's start marker (a condition, bounded by a count
# and not a time) so that a sequential run would fail. FS_FAIL_PART=rust|rest
# makes that half fail at a video self-test step.
cat >"$work/fake-ssh" <<'EOF'
#!/bin/sh
for last; do :; done
cmd="$last"
host=""
for a; do case "$a" in -*|BatchMode=*|ConnectTimeout=*) ;; *) if [ "$a" != "$cmd" ]; then host="$a"; fi ;; esac; done
echo "$host $(echo "$cmd" | cut -c1-40)" >>"$FS_DIR/ssh.log"
case "$cmd" in
  "echo connected") echo connected ;;
  *powershell*) echo 100 ;;
  *"rustc -vV"*) echo "rustc fake ${FS_TOOLS_SUFFIX:-}$( [ "$host" = "${FS_ODD_HOST:-none}" ] && echo odd )" ;;
  *"windows-worker.sh"*)
    vars=${cmd#*&&}
    part=all
    suites=all
    for kv in $vars; do
      case "$kv" in
        NVRVIEW_WINDOWS_PART=*) part=${kv#*=} ;;
        NVRVIEW_SUITES=*) suites=${kv#*=} ;;
        NVRVIEW_CLIPPY_CRATES=*) clippy=${kv#*=} ;;
      esac
    done
    echo "$part $host" >>"$FS_DIR/worker.log"
    # Which hosts are locked while this half runs.
    scripts/windows-lock.sh status 2>/dev/null | grep -c held >"$FS_DIR/held.$part" || true
    if [ "$part" != all ]; then
      : >"$FS_DIR/started.$part"
      other=rust; [ "$part" = rust ] && other=rest
      n=0
      until [ -f "$FS_DIR/started.$other" ]; do
        n=$((n + 1))
        if [ "$n" -gt 2000 ]; then echo "the other half never started" >&2; exit 3; fi
        sleep 0.01
      done
    fi
    printf '== toolchain\n   (toolchain: 0s)\n'
    if [ "$part" != rest ] && [ "${clippy:-none}" != none ]; then
      printf '== clippy\n   (clippy: 3s)\nsuite-result clippy 3\n== tests (run)\n   (tests: 4s)\nsuite-result test 4\nsuite-result doctest 1\n'
    fi
    if [ "$part" != rust ]; then
      for s in $(echo "$suites" | tr ',' ' '); do
        case "$s" in none | all) continue ;; esac
        printf '== suite %s\n   (suite %s: 2s)\nsuite-result %s 2\n' "$s" "$s" "$s"
      done
    fi
    if [ "${FS_FAIL_PART:-}" = "$part" ]; then
      printf '== Windows video self-test (GPU, then WARP)\nvideo self-test: decoded 0 frames\n'
      exit 1
    fi
    printf '\nWindows checks passed in 5s.\n'
    ;;
  *tar.exe*) cat >/dev/null; echo "synced" >>"$FS_DIR/sync.$host" ;;
  *) ;;
esac
EOF
chmod +x "$work/fake-ssh"
# The fake scp copies nothing; it makes the destination file and logs the source.
cat >"$work/fake-scp" <<'EOF2'
#!/bin/sh
for last; do :; done
for first; do case "$first" in -*|BatchMode=*|ConnectTimeout=*) ;; *) break ;; esac; done
echo "$first" >>"$FS_DIR/scp.log"
echo zip >"$last"
EOF2
chmod +x "$work/fake-scp"

run_check() { # DIR LOGFILE  (env decides the rest)
  (
    cd "$1"
    PATH="$1.bin:$PATH" scripts/windows-check.sh
  ) >"$2" 2>&1
}
fresh() { # NAME: a repository, its fake-ssh directory, and the environment of a check
  r="$work/$1"
  new_repo "$r"
  mkdir -p "$r.fs"
  export FS_DIR="$r.fs" NVRVIEW_WINDOWS_LOCK="$r.lock" NVRVIEW_LOCK_POLL=0.05
  export NVRVIEW_CACHE_METADATA="$r.metadata.json" NVRVIEW_CACHE_TOOLS="fake tools"
  export NVRVIEW_TIMING_LOG="$r.timings.jsonl" NVRVIEW_APP_SELFTEST=1 NVRVIEW_FULL_CHECK=1
  export NVRVIEW_CLIPPY_CRATES=none
  unset NVRVIEW_WINDOWS_HOSTS NVRVIEW_WINDOWS_HOST FS_FAIL_PART FS_ODD_HOST NVRVIEW_WINDOWS_PRIORITY NVRVIEW_WINDOWS_LOCK_HELD
  # The check decides the crates itself (cache plan); with no cargo metadata it
  # runs "all", which is what the fake worker is told to expect below.
  unset NVRVIEW_CLIPPY_CRATES
}
hosts_free() { # DIR: both locks are gone
  [ ! -d "$1.lock-win-a" ] && [ ! -d "$1.lock-win-b" ]
}
stamp_of() { # DIR: the Windows stamp without its times
  f=$(ls "$1"/.git/nvrview-checks/*.windows)
  sed -E 's/^(.*)=[0-9]+$/\1/; s/[0-9]+s//g; s/; host .*$//' "$f" | grep -Ev "^(time|commit=|date=)" | sort
}
entries_of() { # DIR
  (cd "$1/.git/nvrview-checks/suite-cache/entries/windows" && find . -type f | sed -E 's/[0-9]{6,}/N/g' | LC_ALL=C sort)
}

# Two hosts free: the halves run at the same time on different hosts.
fresh two
export NVRVIEW_WINDOWS_HOSTS="win-a win-b"
# History in which the app half is the long one, and win-b the faster computer:
# win-b takes the app half although win-a is the first free host.
echo '{"time":1,"kind":"windows","total":400,"tree":"t","steps":[["clippy (--workspace)",100],["build: C# app",300]]}' >"$NVRVIEW_TIMING_LOG"
export NVRVIEW_WINDOWS_SPEEDS="win-a=1 win-b=2"
if run_check "$r" "$work/two.out"; then pass "a check on two free hosts passes"; else cat "$work/two.out"; fail "a check on two free hosts passes"; fi
[ "$(sort -u "$FS_DIR/worker.log" | wc -l | tr -d ' ')" = 2 ] && grep -q '^rust ' "$FS_DIR/worker.log" && grep -q '^rest ' "$FS_DIR/worker.log" && pass "...in two halves" || { cat "$FS_DIR/worker.log"; fail "...in two halves"; }
[ "$(awk '{print $2}' "$FS_DIR/worker.log" | sort -u | wc -l | tr -d ' ')" = 2 ] && pass "...on two different hosts" || fail "...on two different hosts"
[ "$(cat "$FS_DIR/held.rust")" = 2 ] && [ "$(cat "$FS_DIR/held.rest")" = 2 ] && pass "...with both hosts locked while they ran" || fail "...with both hosts locked while they ran ($(cat "$FS_DIR/held.rust") $(cat "$FS_DIR/held.rest"))"
hosts_free "$r" && pass "...and both locks given back" || fail "...and both locks given back"
ls "$FS_DIR"/sync.* | wc -l | grep -q 2 && pass "...each host got the tree" || fail "...each host got the tree"
grep -q 'splitting the check' "$work/two.out" && pass "...and the output says so" || fail "...and the output says so"
grep -q '^split decision: split (split .*plan inputs: Rust half about' "$work/two.out" && pass "...and logs the split decision with the plan's numbers" || fail "...and logs the split decision with the plan's numbers"
two_stamp=$(stamp_of "$r")
two_entries=$(entries_of "$r")
check "...and leaves a Windows stamp" ls "$r"/.git/nvrview-checks/*.windows
# The stamp names the host that ran the app half, and a package is built on
# that host (it holds the tested publish output), whichever host is free first.
app_host=$(sed -n 's/^steps=.*; host \([A-Za-z0-9_.-]*\)$/\1/p' "$r"/.git/nvrview-checks/*.windows)
rest_host=$(grep '^rest ' "$FS_DIR/worker.log" | awk '{print $2}')
[ "$app_host" = win-b ] && [ "$app_host" = "$rest_host" ] && pass "the stamp records the host that ran the app half" || fail "the stamp records the host that ran the app half ($app_host, $rest_host)"
: >"$FS_DIR/ssh.log"
rm -f "$FS_DIR/scp.log"
( cd "$r" && PATH="$r.bin:$PATH" scripts/windows-check.sh --package "$work/two-pkg" ) >"$work/two-pkg.out" 2>&1 || { cat "$work/two-pkg.out"; fail "packaging after a split run works"; }
grep -q "^packaging on $app_host, where the checks" "$work/two-pkg.out" && pass "a package is built on the stamp's host" || fail "a package is built on the stamp's host"
[ -s "$FS_DIR/ssh.log" ] && grep -q "^$app_host " "$FS_DIR/ssh.log" && ! grep -v "^$app_host " "$FS_DIR/ssh.log" | grep -q . && pass "...and nothing is sent to or run on the other host" || { cat "$FS_DIR/ssh.log"; fail "...and nothing is sent to or run on the other host"; }
hosts_free "$r" && pass "...and its lock given back" || fail "...and its lock given back"
unset NVRVIEW_WINDOWS_SPEEDS
# The second run takes everything from the cache.
if run_check "$r" "$work/two2.out"; then :; fi

# One host: exactly as before (one worker run, no split, no second lock).
fresh one
export NVRVIEW_WINDOWS_HOSTS="win-a"
if run_check "$r" "$work/one.out"; then pass "a check on one host passes"; else cat "$work/one.out"; fail "a check on one host passes"; fi
[ "$(wc -l <"$FS_DIR/worker.log" | tr -d ' ')" = 1 ] && grep -q '^all win-a$' "$FS_DIR/worker.log" && pass "...in one whole run on that host" || fail "...in one whole run on that host"
hosts_free "$r" && pass "...and its lock given back" || fail "...and its lock given back"
! grep -q 'splitting the check' "$work/one.out" && pass "...without a split" || fail "...without a split"
grep -q '^split decision: whole on win-a (only one Windows host is configured)' "$work/one.out" && pass "...and logs why it did not split" || fail "...and logs why it did not split"
[ "$(stamp_of "$r")" = "$two_stamp" ] && pass "the stamp is the same as the split run's" || { fail "the stamp is the same as the split run's"; echo "$two_stamp"; stamp_of "$r"; }
[ "$(entries_of "$r")" = "$two_entries" ] && pass "the test result cache entries are the same as the split run's" || { fail "the test result cache entries are the same as the split run's"; echo "$two_entries"; entries_of "$r"; }

# Two hosts listed, the second busy: no split, no wait.
fresh busy
export NVRVIEW_WINDOWS_HOSTS="win-a win-b"
mkdir -p "$r.lock-win-b"
printf 'pid=%s\nmachine=%s\nstart=1\npriority=agent\nhost=win-b\n' "$$" "$(hostname)" >"$r.lock-win-b/owner"
if run_check "$r" "$work/busy.out"; then pass "a check with the second host busy passes"; else cat "$work/busy.out"; fail "a check with the second host busy passes"; fi
grep -q '^all win-a$' "$FS_DIR/worker.log" && [ "$(wc -l <"$FS_DIR/worker.log" | tr -d ' ')" = 1 ] && pass "...whole, on the free host" || fail "...whole, on the free host"
check "...and the busy host's lock is not touched" test -f "$r.lock-win-b/owner"
grep -q '^split decision: whole on win-a (win-b is not free right now' "$work/busy.out" && pass "...and logs that the second host was busy" || fail "...and logs that the second host was busy"
rm -rf "$r.lock-win-b"

# Different tool versions on the second host: no split.
fresh odd
export NVRVIEW_WINDOWS_HOSTS="win-a win-b" FS_ODD_HOST=win-b
if run_check "$r" "$work/odd.out"; then pass "a check with differing toolchains passes"; else cat "$work/odd.out"; fail "a check with differing toolchains passes"; fi
grep -q '^all win-a$' "$FS_DIR/worker.log" && grep -q 'split decision: whole on win-a (win-b has other tool versions' "$work/odd.out" && pass "...whole on one host, and says why" || fail "...whole on one host, and says why"
hosts_free "$r" && pass "...with the second lock given back" || fail "...with the second lock given back"
unset FS_ODD_HOST

# NVRVIEW_WINDOWS_SPLIT=0 turns it off.
fresh off
export NVRVIEW_WINDOWS_HOSTS="win-a win-b" NVRVIEW_WINDOWS_SPLIT=0
run_check "$r" "$work/off.out" || true
grep -q '^all win-a$' "$FS_DIR/worker.log" && pass "NVRVIEW_WINDOWS_SPLIT=0 runs the check whole" || fail "NVRVIEW_WINDOWS_SPLIT=0 runs the check whole"
grep -q '^split decision: whole on win-a (NVRVIEW_WINDOWS_SPLIT=0)' "$work/off.out" && pass "...and logs that it was turned off" || fail "...and logs that it was turned off"
unset NVRVIEW_WINDOWS_SPLIT

# A failure in either half fails the check, names the half, the suite and the
# step, still records what the other half passed, and gives back both locks.
for failing in rust rest; do
  fresh "fail-$failing"
  export NVRVIEW_WINDOWS_HOSTS="win-a win-b" FS_FAIL_PART=$failing
  if run_check "$r" "$work/fail-$failing.out"; then fail "a failing $failing half fails the check"; else pass "a failing $failing half fails the check"; fi
  grep -q "Windows check: the $failing half failed on win-" "$work/fail-$failing.out" && pass "...and names the half" || fail "...and names the half"
  grep -q 'Windows check: failed in: Windows video self-test' "$work/fail-$failing.out" && grep -q 'Windows check: failing step: video self-test' "$work/fail-$failing.out" &&
    pass "...and the suite and the step" || { cat "$work/fail-$failing.out"; fail "...and the suite and the step"; }
  hosts_free "$r" && pass "...and both locks are given back" || fail "...and both locks are given back"
  check "...and no stamp is written" test -z "$(ls "$r"/.git/nvrview-checks/*.windows 2>/dev/null)"
  # The passing half's suites are in the cache for the retry.
  if [ "$failing" = rust ]; then want=app-debug.startup; else want=""; fi
  if [ -n "$want" ]; then
    check "...and the other half's passes are recorded" test -d "$r/.git/nvrview-checks/suite-cache/entries/windows/$want"
  fi
  unset FS_FAIL_PART
done

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all windows split tests passed"
