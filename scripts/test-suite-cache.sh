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
for c in a b c d viewer-ffi mock-nvr sign-update updates; do
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
 {"name": "d", "manifest_path": "$r/crates/d/Cargo.toml", "targets": [{"kind": ["lib"]}], "dependencies": []},
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
grep -q "cached suites: test (8 of 8 crates), swift-test" "$work/table" && pass "the table names the cached suites" || fail "the table names the cached suites"
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

# The Windows app self-test is cached by area: the areas whose inputs changed
# rerun (with the startup smoke, which takes every change under apps/windows),
# and a change to shared code reruns them all. The C# suites cover the
# projects they build; the trimmed build (app-publish.AREA) is deferred except
# for the daily run and full runs.
for d in App Shell Demo Interop Tiles Video Smoke Shell.Tests Tiles.Tests; do
  mkdir -p "apps/windows/NvrView.$d"
done
mkdir -p apps/windows/layout
for f in apps/windows/NvrView.App/SelfTest.cs apps/windows/NvrView.App/SelfTest.Recordings.cs \
  apps/windows/NvrView.App/SelfTest.Ptz.cs apps/windows/NvrView.App/RecordingsWindow.cs \
  apps/windows/NvrView.App/PtzControls.cs apps/windows/NvrView.App/StageView.cs \
  apps/windows/NvrView.App/Updater.cs apps/windows/NvrView.App/Program.cs \
  apps/windows/NvrView.Shell/Recordings.cs apps/windows/NvrView.Demo/Program.cs \
  apps/windows/NvrView.Interop/Core.cs apps/windows/NvrView.Tiles/Zoom.cs \
  apps/windows/NvrView.Video/Mf.cs apps/windows/NvrView.Smoke/Program.cs \
  apps/windows/NvrView.Shell.Tests/Tests.cs apps/windows/NvrView.Tiles.Tests/Tests.cs \
  apps/windows/layout/nvlayout.c apps/windows/Directory.Build.props \
  scripts/windows-app.sh scripts/windows-selftest-areas.txt scripts/csharp-smoke.sh; do
  echo "// $f" >"$f"
done
git add -A
git commit -q -m "windows app files"
# The areas to run, in the debug build and in the publish build, and whether
# the update test runs ("areas <debug> | <publish> | <update or ->"); all pass.
wcheck() {
  tick 60
  wrun=$(sc plan windows 2>/dev/null)
  out=""
  for prefix in app-debug app-publish; do
    todo=""
    for a in $(sc areas windows); do
      if sc want "$wrun" "$prefix.$a" >/dev/null; then todo="$todo${todo:+,}$a"; sc pass "$wrun" 1 "$prefix.$a"; fi
    done
    out="$out${out:+ | }${todo:--}"
  done
  upd=-
  if sc want "$wrun" app-update >/dev/null; then upd=update; sc pass "$wrun" 1 app-update; fi
  if [ "${WFAIL:-}" = trimmed ]; then
    sc finish "$wrun" --failed app-update >"$work/wtable"
  else
    sc finish "$wrun" --ok >"$work/wtable"
  fi
  echo "$out | $upd"
}
all_areas="startup,viewer,tiles,layouts,header,settings,calls,recordings,capture,ptz"
same "app areas: the first run runs every area, debug and publish, and the update test" "$(wcheck)" "$all_areas | $all_areas | update"
same "app areas: unchanged, all cached" "$(wcheck)" "- | - | -"
echo "// recordings window 2" >>apps/windows/NvrView.App/RecordingsWindow.cs
git add -A
git commit -q -m "recordings window"
same "app areas: a recordings view file reruns recordings and startup in the debug build only" "$(wcheck)" "startup,recordings | - | -"
grep -q "app-publish.recordings *deferred" "$work/wtable" && pass "the table shows the deferred trimmed suites" || fail "the table shows the deferred trimmed suites"
grep -q "cached suites:.*app-publish.ptz (deferred)" "$work/wtable" && pass "...and counts them like cached ones (the stamp is not a full one)" || fail "...and counts them like cached ones"
commit_file apps/windows/NvrView.Shell/Recordings.cs "// shell recordings 2"
same "app areas: so does a recordings shell file" "$(wcheck)" "startup,recordings | - | -"
commit_file apps/windows/NvrView.App/SelfTest.Recordings.cs "// self-test recordings 2"
same "app areas: so does the recordings self-test file" "$(wcheck)" "startup,recordings | - | -"
commit_file apps/windows/NvrView.App/PtzControls.cs "// ptz 2"
same "app areas: a PTZ file reruns ptz and startup" "$(wcheck)" "startup,ptz | - | -"
commit_file apps/windows/NvrView.Demo/Program.cs "// demo 2"
same "app areas: a project that is not part of the app reruns no area" "$(wcheck)" "- | - | -"
commit_file apps/windows/NvrView.App/StageView.cs "// shared stage 2"
same "app areas: a shared app file reruns every area (debug only)" "$(wcheck)" "$all_areas | - | -"
commit_file apps/windows/NvrView.App/SelfTest.cs "// shared self-test 2"
same "app areas: the shared self-test file reruns every area" "$(wcheck)" "$all_areas | - | -"
commit_file scripts/windows-selftest-areas.txt "# table 2"
same "app areas: the area table reruns every area" "$(wcheck)" "$all_areas | - | -"
commit_file fixtures/synthetic/clip.h264 "frame 2"
same "app areas: the fixtures rerun every area (and the update test, which loads the core)" "$(wcheck)" "$all_areas | - | update"
echo "// local edit" >>apps/windows/NvrView.App/RecordingsWindow.cs
same "app areas: an unstaged edit of a recordings file runs recordings and startup" "$(wcheck)" "startup,recordings | - | -"
same "app areas: ...every time, as it is never recorded" "$(wcheck)" "startup,recordings | - | -"
git checkout -q apps/windows/NvrView.App/RecordingsWindow.cs
same "app areas: an area can pass alone: the others stay cached" "$(wcheck)" "- | - | -"
# One area failing leaves its entry unrecorded while the others are kept.
echo "// ptz 3" >>apps/windows/NvrView.App/PtzControls.cs
git add -A
git commit -q -m "ptz 3"
wrun=$(sc plan windows 2>/dev/null)
sc want "$wrun" app-debug.ptz >/dev/null && sc want "$wrun" app-debug.startup >/dev/null && sc pass "$wrun" 1 app-debug.startup
sc finish "$wrun" --failed app-debug.ptz >/dev/null
same "app areas: a failed area is run again, a passed one is not" \
  "$(tick 60; wrun=$(sc plan windows 2>/dev/null); o=""; for a in $(sc areas windows); do sc want "$wrun" "app-debug.$a" >/dev/null && o="$o${o:+,}$a"; done; sc finish "$wrun" --ok >/dev/null; echo "$o")" "ptz"

# The trimmed build runs once a day: 24 hours after the last passing trimmed
# run the next check runs every publish area (and the update test), whatever
# the cache holds; a failed trimmed run does not count; a release (full run)
# always runs them.
tick $((23 * 3600))
same "trimmed: 23 hours after the first run the publish areas are still deferred" "$(wcheck)" "ptz | - | -"
tick $((2 * 3600))
WFAIL=trimmed
same "trimmed: after 24 hours every publish area and the update test are due" "$(wcheck)" "- | $all_areas | update"
grep -q "app-update.*FAILED" "$work/wtable" && pass "trimmed: ...the failing update test shows in the table" || fail "trimmed: the failing update test shows in the table"
unset WFAIL
same "trimmed: a failed trimmed run does not count: they are due again" "$(wcheck)" "- | $all_areas | update"
same "trimmed: after a passing trimmed run they are deferred again" "$(wcheck)" "- | - | -"
case "$(sc full-due windows || echo no)" in no) pass "trimmed: a passing trimmed run is no full run" ;; *) fail "trimmed: a passing trimmed run is no full run" ;; esac
wrun=$(sc plan windows --full 2>/dev/null)
n=0
for a in $(sc areas windows); do sc want "$wrun" "app-publish.$a" >/dev/null && n=$((n + 1)); done
sc want "$wrun" app-update >/dev/null && n=$((n + 1))
sc finish "$wrun" --failed x >/dev/null
same "trimmed: a full run (a release) runs every publish area and the update test" "$n" "11"
NVRVIEW_TRIMMED_MAX_AGE_H=1000
export NVRVIEW_TRIMMED_MAX_AGE_H
tick $((30 * 3600))
same "trimmed: NVRVIEW_TRIMMED_MAX_AGE_H moves the limit" "$(wcheck)" "- | - | -"
unset NVRVIEW_TRIMMED_MAX_AGE_H
wcheck >/dev/null # the due trimmed run, so the C# checks below start from a settled state

# The C# suites cover the projects they build, and only those.
csuites() {
  tick 60
  wrun=$(sc plan windows 2>/dev/null)
  out=""
  for s in csharp-smoke tiles-tests video-selftest shell-tests app-update; do
    if sc want "$wrun" "$s" >/dev/null; then out="$out${out:+,}$s"; sc pass "$wrun" 1 "$s"; fi
  done
  sc finish "$wrun" --ok >/dev/null
  echo "${out:--}"
}
csuites >/dev/null
same "C# suites: unchanged, all cached" "$(csuites)" "-"
commit_file apps/windows/NvrView.Video/Mf.cs "// video 2"
same "C# suites: a video file reruns the video self-test only" "$(csuites)" "video-selftest"
commit_file apps/windows/NvrView.Demo/Program.cs "// demo 3"
same "C# suites: so does the demo project it builds" "$(csuites)" "video-selftest"
commit_file apps/windows/NvrView.Shell.Tests/Tests.cs "// shell tests 2"
same "C# suites: a shell test file reruns the shell tests only" "$(csuites)" "shell-tests"
commit_file apps/windows/NvrView.Tiles.Tests/Tests.cs "// tiles tests 2"
same "C# suites: a tile test file reruns the tile tests only" "$(csuites)" "tiles-tests"
commit_file apps/windows/NvrView.Smoke/Program.cs "// smoke 2"
same "C# suites: a smoke file reruns the smoke test only" "$(csuites)" "csharp-smoke"
commit_file apps/windows/NvrView.Interop/Core.cs "// interop 2"
same "C# suites: interop reruns smoke, video, and the shell tests" "$(csuites)" "csharp-smoke,video-selftest,shell-tests"
commit_file apps/windows/NvrView.Tiles/Zoom.cs "// tiles 2"
same "C# suites: the tiles project reruns every suite built on it" "$(csuites)" "csharp-smoke,tiles-tests,video-selftest,shell-tests"
commit_file apps/windows/NvrView.Shell/Recordings.cs "// shell recordings 3"
same "C# suites: a shell file reruns smoke, video, and the shell tests" "$(csuites)" "csharp-smoke,video-selftest,shell-tests"
commit_file apps/windows/NvrView.App/RecordingsWindow.cs "// recordings window 3"
same "C# suites: a recordings-only app file reruns none of them" "$(csuites)" "-"
commit_file apps/windows/NvrView.App/Updater.cs "// updater 2"
same "C# suites: an updater file reruns the update test only" "$(csuites)" "app-update"
commit_file apps/windows/NvrView.App/Program.cs "// program 2"
same "C# suites: so does the app's start-up handling" "$(csuites)" "app-update"
commit_file scripts/windows-app.sh "# windows app 2"
same "C# suites: so does the packaging script" "$(csuites)" "app-update"
commit_file crates/updates/src/lib.rs "// updates 2"
same "C# suites: the updates crate (signing tool) reruns the update test only" "$(csuites)" "app-update"
commit_file scripts/csharp-smoke.sh "# smoke script 2"
same "C# suites: the smoke script reruns smoke and the tile tests" "$(csuites)" "csharp-smoke,tiles-tests"
commit_file fixtures/synthetic/clip.h264 "frame 3"
same "C# suites: the fixtures rerun the suites that play them (not the tile tests)" "$(csuites)" "csharp-smoke,video-selftest,shell-tests,app-update"
commit_file apps/windows/Directory.Build.props "<!-- props 2 -->"
same "C# suites: the shared build settings rerun every suite" "$(csuites)" "csharp-smoke,tiles-tests,video-selftest,shell-tests,app-update"
commit_file crates/viewer-ffi/src/lib.rs "// ffi 2"
same "C# suites: the core library reruns the ones that load it" "$(csuites)" "csharp-smoke,video-selftest,app-update"
(cd "$here/.." && python3 -I scripts/suite-cache.py check-projects) && pass "the project table matches the C# project files" || fail "the project table matches the C# project files"

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
