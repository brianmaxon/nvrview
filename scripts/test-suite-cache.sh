#!/bin/sh
# Tests the shell side of the test result cache: scripts/suite-cache-lib.sh
# (the plan file, the run log, finish, and finish from an exit trap) with the
# real scripts/suite-cache.py in a throwaway repository with a fake crate
# graph, a fake clock, and fake tool versions. The decision logic itself
# (fingerprints, invalidation, expiry, areas, ...) is in test_suite_cache.py,
# which calls the Python functions in one process. No real build or test runs.
#
#   scripts/test-suite-cache.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
same() { # NAME GOT WANT
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi
}

real_py=$(python3 -I -c 'import sys; print(sys.executable)')
r="$work/repo"
mkdir -p "$r/scripts" "$r/apps/macos" "$r/fixtures/synthetic"
cp "$here/suite-cache.py" "$here/suite-cache-lib.sh" "$r/scripts/"
cd "$r"
git init -q -b main .
git config user.email test@example.com
git config user.name test
git config commit.gpgsign false
for c in a b viewer-ffi mock-nvr sign-update updates; do
  mkdir -p "crates/$c/src"
  echo "// $c" >"crates/$c/src/lib.rs"
  echo "[package]" >"crates/$c/Cargo.toml"
done
echo "[workspace]" >Cargo.toml
echo "# lock 1" >Cargo.lock
echo "// app" >apps/macos/App.swift
cat >"$work/metadata.json" <<EOF
{"workspace_root": "$r", "packages": [
 {"name": "a", "manifest_path": "$r/crates/a/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []},
 {"name": "b", "manifest_path": "$r/crates/b/Cargo.toml", "targets": [{"kind": ["lib"]}],
  "dependencies": [{"name": "a", "kind": null, "path": "$r/crates/a"}]},
 {"name": "viewer-ffi", "manifest_path": "$r/crates/viewer-ffi/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []},
 {"name": "mock-nvr", "manifest_path": "$r/crates/mock-nvr/Cargo.toml", "targets": [{"kind": ["bin"]}], "dependencies": []},
 {"name": "updates", "manifest_path": "$r/crates/updates/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []},
 {"name": "sign-update", "manifest_path": "$r/crates/sign-update/Cargo.toml", "targets": [{"kind": ["bin"]}],
  "dependencies": [{"name": "updates", "kind": null, "path": "$r/crates/updates"}]}
]}
EOF
git add -A
git commit -q -m init

export NVRVIEW_CACHE_METADATA="$work/metadata.json"
export NVRVIEW_CACHE_TOOLS="toolchain 1"
export NVRVIEW_CACHE_NOW=1000000000
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H

# An interpreter that counts how often it is called, then runs the real one.
cat >"$work/counting-python" <<EOF
#!/bin/sh
echo call >>"$work/calls"
exec "$real_py" "\$@"
EOF
chmod +x "$work/counting-python"

# A fake check wired like scripts/check.sh: the plan is read from the files,
# results are appended to the log, and finish runs from the exit trap. STOP_AFTER
# names a step after which the run is interrupted (the exit trap still runs);
# FAIL_AT makes that step fail. Prints what ran and what was cached.
cat >"$work/fake-check.sh" <<'EOF'
set -eu
cd "$FAKE_REPO"
py="$COUNTING_PY"
. scripts/suite-cache-lib.sh
cache_start mac ${PLAN_ARGS:-}
ok=0
step_name=""
on_exit() {
  rc=$?
  if [ "$ok" = 0 ]; then sc finish "$run_file" --failed "$step_name" >"$FAKE_TABLE" || true; fi
  exit "$rc"
}
trap on_exit EXIT
args=$(cache_crates test args)
echo "crates: ${args:-none}"
for s in c-header c-smoke c-smoke-asan; do
  if cache_want "$s"; then
    step_name=$s
    echo "run $s"
    if [ "${FAIL_AT:-}" = "$s" ]; then exit 1; fi
    cache_pass 2 "$s"
    if [ "${STOP_AFTER:-}" = "$s" ]; then exit 1; fi
  fi
done
if [ -n "$args" ]; then cache_pass 3 test; fi
ok=1
sc finish "$run_file" --ok >"$FAKE_TABLE"
EOF
export FAKE_REPO="$r" COUNTING_PY="$work/counting-python" FAKE_TABLE="$work/table"
fake() { # prints "crates=<..> ran=<steps> cached=<steps>"
  out=$(sh "$work/fake-check.sh" 2>/dev/null) || true
  ran=$(printf '%s\n' "$out" | sed -n 's/^run //p' | tr '\n' ',')
  cached=$(printf '%s\n' "$out" | sed -n 's/^cached pass \([a-z-]*\) .*/\1/p' | tr '\n' ',')
  echo "$(printf '%s\n' "$out" | sed -n 's/^crates: //p') ran=${ran:--} cached=${cached:--}"
}
tick() { NVRVIEW_CACHE_NOW=$((NVRVIEW_CACHE_NOW + $1)); export NVRVIEW_CACHE_NOW; }

tick 60
rm -f "$work/calls"
same "wiring: the first run runs everything" "$(fake)" "--workspace ran=c-header,c-smoke,c-smoke-asan, cached=-"
same "wiring: Python runs twice in a run (plan, finish), not once per suite" "$(wc -l <"$work/calls" | tr -d ' ')" "2"
grep -q "c-smoke-asan  *ran" "$work/table" && pass "wiring: the table lists the passes the shell logged" || fail "wiring: the table lists the passes the shell logged"
grep -q "test  *ran 6 crates" "$work/table" && pass "wiring: ...and the crates family" || fail "wiring: ...and the crates family"
tick 60
same "wiring: the next run reads everything as cached" "$(fake)" "none ran=- cached=c-header,c-smoke,c-smoke-asan,"
grep -q "cached suites: test (6 of 6 crates), c-header, c-smoke, c-smoke-asan" "$work/table" && pass "wiring: the table names the cached suites" || fail "wiring: the table names the cached suites"
[ -z "$(ls "$(git rev-parse --git-common-dir)/nvrview-checks/suite-cache/runs")" ] && pass "wiring: finish removes the run files" || fail "wiring: finish removes the run files"

# The plan files: tab-separated, one want line per suite, and the shell
# helpers read them.
tick 60
run=$(python3 -I scripts/suite-cache.py plan mac 2>/dev/null)
plan_file="${run%.json}.plan"
grep -q "^want	swift-test	cached	cached pass swift-test " "$plan_file" && fail "plan: swift-test was never recorded here" || pass "plan: swift-test was never recorded here, so it runs"
grep -q "^crates	test		none$" "$plan_file" && pass "plan: the crates line carries the arguments and the list" || fail "plan: the crates line carries the arguments and the list"
[ -f "${run%.json}.log" ] && pass "plan: the run log starts empty" || fail "plan: the run log starts empty"
python3 -I scripts/suite-cache.py finish "$run" --ok >/dev/null
[ ! -e "$run" ] && [ ! -e "$plan_file" ] && [ ! -e "${run%.json}.log" ] && pass "plan: finish removes RUN.json, RUN.plan and RUN.log" || fail "plan: finish removes RUN.json, RUN.plan and RUN.log"

# An interrupted run keeps the passes of the suites that completed.
echo "// app 2" >>apps/macos/App.swift
git add -A
git commit -q -m "swift change"
echo "# lock 2" >Cargo.lock
git add -A
git commit -q -m "lock change"
tick 60
same "interrupted: the run stops after c-header" "$(STOP_AFTER=c-header fake)" "--workspace ran=c-header, cached=-"
grep -q "c-header  *ran" "$work/table" && grep -q "c-header.*" "$work/table" && pass "interrupted: the exit trap's finish prints the table" || fail "interrupted: the exit trap's finish prints the table"
grep -q "FAILED" "$work/table" && pass "interrupted: ...with the step that was running" || fail "interrupted: ...with the step that was running"
tick 60
same "interrupted: the next run has c-header cached and the crates unrecorded" "$(fake)" "--workspace ran=c-smoke,c-smoke-asan, cached=c-header,"
# Lock change reran every crate; step results do not depend on files here.

# A failing step is not recorded.
echo "# lock 3" >Cargo.lock
git add -A
git commit -q -m "lock change 2"
tick 60
same "failure: c-smoke fails" "$(FAIL_AT=c-smoke fake)" "--workspace ran=c-header,c-smoke, cached=-"
tick 60
same "failure: c-header passed before it and stays recorded" "$(fake)" "--workspace ran=c-smoke,c-smoke-asan, cached=c-header,"

# Retried passes are recorded as retried and never skip anything.
tick 60
fake_retried() { PLAN_ARGS=--retried fake; }
echo "# lock 4" >Cargo.lock
git add -A
git commit -q -m "lock change 3"
same "retried: the retried run runs everything" "$(fake_retried)" "--workspace ran=c-header,c-smoke,c-smoke-asan, cached=-"
ls "$(git rev-parse --git-common-dir)/nvrview-checks/suite-cache/entries/mac/c-header" | grep -q '\.retried\.json$' && pass "retried: the pass is recorded as retried" || fail "retried: the pass is recorded as retried"
tick 60
same "retried: ...and skips nothing in the next run" "$(fake)" "--workspace ran=c-header,c-smoke,c-smoke-asan, cached=-"

# The scripts use the plan, not Python per suite.
for f in check.sh windows-check.sh; do
  if grep -Eq 'sc (want|crates|pass|areas)|python3 -I scripts/suite-cache.py (want|crates|pass)' "$here/$f"; then
    fail "$f asks Python per suite"
  else
    grep -q 'cache_start' "$here/$f" && grep -q 'cache_want' "$here/$f" && grep -q 'cache_pass' "$here/$f" && pass "$f reads the plan and logs events" || fail "$f reads the plan and logs events"
  fi
done

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all suite cache wiring tests passed"
