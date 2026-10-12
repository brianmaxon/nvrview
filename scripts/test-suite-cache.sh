#!/bin/sh
# Tests the test result cache (scripts/suite-cache.py) in throwaway
# repositories with fake suites, a fake crate graph, a fake clock, and fake
# tool versions. No real build or test runs.
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

# Crates: b depends on a; c has b as a dev-dependency; a has d as a
# dev-dependency (so d's changes rerun a's tests, but not b's or c's).
r="$work/repo"
mkdir -p "$r/scripts" "$r/apps/macos" "$r/fixtures/synthetic"
cp "$here/suite-cache.py" "$r/scripts/"
cd "$r"
git init -q -b main .
git config user.email test@example.com
git config user.name test
git config commit.gpgsign false
for c in a b c d; do
  mkdir -p "crates/$c/src"
  echo "// $c" >"crates/$c/src/lib.rs"
  echo "[package]" >"crates/$c/Cargo.toml"
done
echo "[workspace]" >Cargo.toml
echo "# lock 1" >Cargo.lock
echo "// app" >apps/macos/App.swift
echo "frame" >fixtures/synthetic/clip.h264
echo 0.1.0 >VERSION
cat >"$work/metadata.json" <<EOF
{"workspace_root": "$r", "packages": [
 {"name": "a", "manifest_path": "$r/crates/a/Cargo.toml", "targets": [{"kind": ["lib"]}],
  "dependencies": [{"name": "d", "kind": "dev", "path": "$r/crates/d"}]},
 {"name": "b", "manifest_path": "$r/crates/b/Cargo.toml", "targets": [{"kind": ["lib"]}],
  "dependencies": [{"name": "a", "kind": null, "path": "$r/crates/a"}, {"name": "serde", "kind": null}]},
 {"name": "c", "manifest_path": "$r/crates/c/Cargo.toml", "targets": [{"kind": ["bin"]}],
  "dependencies": [{"name": "b", "kind": "dev", "path": "$r/crates/b"}]},
 {"name": "d", "manifest_path": "$r/crates/d/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []}
]}
EOF
git add -A
git commit -q -m init

export NVRVIEW_CACHE_METADATA="$work/metadata.json"
export NVRVIEW_CACHE_TOOLS="toolchain 1"
export NVRVIEW_CACHE_NOW=1000000000
unset NVRVIEW_FULL_CHECK NVRVIEW_FULL_EVERY NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H
sc() { python3 -I "$r/scripts/suite-cache.py" "$@"; }
tick() { NVRVIEW_CACHE_NOW=$((NVRVIEW_CACHE_NOW + $1)); }
commit_file() { # FILE CONTENT
  echo "$2" >"$1"
  git add -A
  git commit -q -m "change $1"
}

# One fake check: the Rust test family and the Swift suite. Prints
# "<mode> test=<crates to run> swift=<run|cached>"; both pass unless
# FAKE_FAIL names swift, and PLAN_ARGS go to plan.
check() {
  tick 60
  # shellcheck disable=SC2086 # PLAN_ARGS is a list of options
  run=$(sc plan mac ${PLAN_ARGS:-} 2>"$work/plan.err")
  mode=cached
  if grep -q '^full run' "$work/plan.err"; then mode=full; fi
  crates=$(sc crates "$run" test --list 2>/dev/null)
  if sc want "$run" swift-test >/dev/null; then swift=run; else swift=cached; fi
  [ "$crates" = none ] || sc pass "$run" 3 test
  if [ "$swift" = run ] && [ "${FAKE_FAIL:-}" != swift ]; then sc pass "$run" 2 swift-test; fi
  if [ "${FAKE_FAIL:-}" = swift ]; then
    sc finish "$run" --failed swift-test >"$work/table"
  else
    sc finish "$run" --ok >"$work/table"
  fi
  echo "$mode test=$crates swift=$swift"
}

same "first run is full" "$(check)" "full test=all swift=run"
same "unchanged: everything cached" "$(check)" "cached test=none swift=cached"
grep -q "cached suites: test (4 of 4 crates), swift-test" "$work/table" && pass "the table names the cached suites" || fail "the table names the cached suites"
commit_file crates/d/src/lib.rs "// d2"
same "a dev-dependency's change reruns its dependents only" "$(check)" "cached test=a,d swift=cached"
commit_file crates/a/src/lib.rs "// a2"
same "a dependency's change reruns every crate above it" "$(check)" "cached test=a,b,c swift=cached"
commit_file crates/c/src/lib.rs "// c2"
same "a leaf change reruns the leaf only" "$(check)" "cached test=c swift=cached"
commit_file Cargo.lock "# lock 2"
same "a Cargo.lock change reruns all Rust" "$(check)" "cached test=all swift=cached"
NVRVIEW_CACHE_TOOLS="toolchain 2"
same "a toolchain change reruns everything" "$(check)" "cached test=all swift=run"
same "...and is cached after" "$(check)" "cached test=none swift=cached"
commit_file apps/macos/App.swift "// app 2"
same "a Swift change reruns the Swift suite only" "$(check)" "cached test=none swift=run"

# Failures are never cached.
commit_file apps/macos/App.swift "// app 3"
same "a failing suite runs" "$(FAKE_FAIL=swift check)" "cached test=none swift=run"
grep -q "swift-test.*FAILED" "$work/table" && pass "the table shows the failure" || fail "the table shows the failure"
same "a failure is not cached" "$(check)" "cached test=none swift=run"

# A pass in a run retried after a flake is recorded but never skips a suite.
commit_file apps/macos/App.swift "// app 4"
same "a retried run runs the suite" "$(PLAN_ARGS=--retried check)" "cached test=none swift=run"
same "a retried pass does not skip the suite" "$(check)" "cached test=none swift=run"
same "the later clean pass does" "$(check)" "cached test=none swift=cached"

# Uncommitted changes to a suite's inputs: it runs and is never recorded;
# staged changes count as content.
echo "// local edit" >>crates/d/src/lib.rs
same "an unstaged input change reruns its suites" "$(check)" "cached test=a,d swift=cached"
same "...and is never recorded" "$(check)" "cached test=a,d swift=cached"
git add crates/d/src/lib.rs
same "staged, it is recorded" "$(check)" "cached test=a,d swift=cached"
same "...under the staged content" "$(check)" "cached test=none swift=cached"
echo "notes" >untracked-notes.txt
same "an untracked file outside every input changes nothing" "$(check)" "cached test=none swift=cached"
git commit -q -m "d edit"

# Forced full runs.
same "NVRVIEW_FULL_CHECK=1 runs everything" "$(NVRVIEW_FULL_CHECK=1 check)" "full test=all swift=run"
same "so does --full (the release scripts)" "$(PLAN_ARGS=--full check)" "full test=all swift=run"
grep -q 'export NVRVIEW_FULL_CHECK=1' "$here/release-check.sh" && pass "release-check.sh forces full runs" || fail "release-check.sh forces full runs"
grep -q 'NVRVIEW_FULL_CHECK=1 scripts/check.sh' "$here/release.sh" && pass "release.sh forces a full Mac run" || fail "release.sh forces a full Mac run"
same "every Nth run is off by default" "$(check)$(check)$(check)$(check)" \
  "cached test=none swift=cachedcached test=none swift=cachedcached test=none swift=cachedcached test=none swift=cached"
export NVRVIEW_FULL_EVERY=3
same "NVRVIEW_FULL_EVERY=3: the next run is the Nth since the full one" "$(check)" "full test=all swift=run"
same "...then two cached runs" "$(check)$(check)" "cached test=none swift=cachedcached test=none swift=cached"
same "...then a full one" "$(check)" "full test=all swift=run"
unset NVRVIEW_FULL_EVERY
tick $((71 * 3600))
same "71 hours later passes are still cached" "$(check)" "cached test=none swift=cached"
tick $((2 * 3600))
same "no full pass in 72 hours: full run" "$(check)" "full test=all swift=run"
same "...after which runs are cached again" "$(check)" "cached test=none swift=cached"
same "full-due is quiet when no full run is due" "$(sc full-due mac || echo not-due)" "not-due"
tick $((73 * 3600))
case "$(sc full-due mac)" in *"72h"*) pass "full-due names the 72-hour rule" ;; *) fail "full-due names the 72-hour rule" ;; esac
NVRVIEW_FULL_MAX_AGE_H=200
export NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H=200
same "NVRVIEW_FULL_MAX_AGE_H and NVRVIEW_CACHE_MAX_AGE_H move both limits" "$(check)" "cached test=none swift=cached"
unset NVRVIEW_FULL_MAX_AGE_H NVRVIEW_CACHE_MAX_AGE_H
same "a failed full run does not count as the full pass" "$(FAKE_FAIL=swift check)" "full test=all swift=run"
same "...so the next run is full again" "$(check)" "full test=all swift=run"

# Platforms are separate; worktrees share the store.
same "another platform has its own entries" "$(sc full-due windows >/dev/null && echo due)" "due"
git worktree add -q "$work/wt" -b other
cd "$work/wt"
same "a worktree of the same content reuses the main checkout's passes" "$(check)" "cached test=none swift=cached"
commit_file crates/b/src/lib.rs "// b in the worktree"
same "a worktree change reruns its dependents" "$(check)" "cached test=b,c swift=cached"
cd "$r"
git merge -q other
same "...and the main checkout then reuses the worktree's passes" "$(check)" "cached test=none swift=cached"

# The store stays bounded: entries older than 14 days are pruned.
entries=$(git rev-parse --git-common-dir)/nvrview-checks/suite-cache/entries
old="$entries/mac/test.zz/0000.json"
mkdir -p "$(dirname "$old")"
echo '{"time": 0}' >"$old"
touch -t 200001010000 "$old"
check >/dev/null
[ ! -f "$old" ] && pass "old entries are pruned" || fail "old entries are pruned"
[ -z "$(ls "$(dirname "$entries")/runs" 2>/dev/null)" ] && pass "finished runs leave no run file" || fail "finished runs leave no run file"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all suite cache tests passed"
