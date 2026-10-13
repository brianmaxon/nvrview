#!/usr/bin/env python3
"""Tests the decision logic of the test result cache (scripts/suite-cache.py).

One interpreter imports suite-cache.py and calls its functions directly, in
throwaway repositories with fake suites, a fake crate graph, a fake clock, and
fake tool versions. No real build or test runs. The wiring (check.sh reading
the plan, the run log, finish from the exit trap) is in test-suite-cache.sh.

  python3 -I scripts/test_suite_cache.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("suite_cache", os.path.join(HERE, "suite-cache.py"))
sc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sc)

failures = 0


def ok(name):
    print("ok   " + name)


def fail(name):
    global failures
    print("FAIL " + name, file=sys.stderr)
    failures += 1


def same(name, got, want):
    if got == want:
        ok(name)
    else:
        fail("%s (got %r, want %r)" % (name, got, want))


def check(name, cond):
    ok(name) if cond else fail(name)


def sh(*cmd, cwd=None):
    subprocess.run(cmd, check=True, capture_output=True, cwd=cwd)


def write(path, text):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


work = tempfile.mkdtemp()
r = os.path.join(work, "repo")
os.makedirs(r)
os.chdir(r)
sh("git", "init", "-q", "-b", "main", ".")
for k, v in (("user.email", "test@example.com"), ("user.name", "test"), ("commit.gpgsign", "false")):
    sh("git", "config", k, v)

# Crates: b depends on a; c has b as a dev-dependency; a has d as a
# dev-dependency (so d's changes rerun a's tests, but not b's or c's).
for c in ("a", "b", "c", "d", "viewer-ffi", "mock-nvr", "sign-update", "updates"):
    write("crates/%s/src/lib.rs" % c, "// %s\n" % c)
    write("crates/%s/Cargo.toml" % c, "[package]\n")
write("Cargo.toml", "[workspace]\n")
write("Cargo.lock", "# lock 1\n")
write("apps/macos/App.swift", "// app\n")
write("fixtures/synthetic/clip.h264", "frame\n")
write("VERSION", "0.1.0\n")
import json  # noqa: E402


def pkg(name, kind, deps):
    return {"name": name, "manifest_path": "%s/crates/%s/Cargo.toml" % (r, name), "targets": [{"kind": [kind]}],
            "dependencies": deps}


def dep(name, kind=None):
    return {"name": name, "kind": kind, "path": "%s/crates/%s" % (r, name)}


write(os.path.join(work, "metadata.json"), json.dumps({"workspace_root": r, "packages": [
    pkg("a", "lib", [dep("d", "dev")]),
    pkg("b", "lib", [dep("a"), {"name": "serde", "kind": None}]),
    pkg("c", "bin", [dep("b", "dev")]),
    pkg("d", "lib", []),
    pkg("viewer-ffi", "lib", []),
    pkg("mock-nvr", "bin", []),
    pkg("updates", "lib", []),
    pkg("sign-update", "bin", [dep("updates")]),
]}))
sh("git", "add", "-A")
sh("git", "commit", "-q", "-m", "init")

os.environ["NVRVIEW_CACHE_METADATA"] = os.path.join(work, "metadata.json")
os.environ["NVRVIEW_CACHE_TOOLS"] = "toolchain 1"
os.environ["NVRVIEW_CACHE_NOW"] = "1000000000"
for v in ("NVRVIEW_FULL_CHECK", "NVRVIEW_FULL_EVERY", "NVRVIEW_FULL_MAX_AGE_H", "NVRVIEW_CACHE_MAX_AGE_H",
          "NVRVIEW_TRIMMED_MAX_AGE_H"):
    os.environ.pop(v, None)


def tick(secs):
    os.environ["NVRVIEW_CACHE_NOW"] = str(int(os.environ["NVRVIEW_CACHE_NOW"]) + secs)


def commit_file(path, text):
    write(path, text + "\n")
    sh("git", "add", "-A")
    sh("git", "commit", "-q", "-m", "change " + path)


# What the shell does with the plan (scripts/suite-cache-lib.sh), in process:
# read a decision, and append the event to the run's log.
def event(path, *fields):
    with open(sc.run_stem(path) + ".log", "a") as f:
        f.write("\t".join(str(x) for x in fields) + "\n")


def run_data(path):
    return sc.read_json(path)


def want(path, suite):
    """True when SUITE runs; a cached or deferred suite logs its event."""
    d = run_data(path)["wants"].get(suite)
    if d is None or d["status"] == "run":
        return True
    event(path, "want", suite)
    return False


def crates(path, fam):
    d = run_data(path)["crate_plans"][fam]
    event(path, "crates", fam)
    return d["list"]


def passed(path, secs, name):
    event(path, "pass", secs, name)


def plan(platform="mac", **kw):
    path, message = sc.plan(platform, **kw)
    return path, message.startswith("full run")


# One fake check: the Rust test family and the Swift suite. Returns
# "<mode> test=<crates to run> swift=<run|cached>"; both pass unless fail
# names swift. The table of the last check is in last_table.
last_table = ""


def fake_check(fail_swift=False, **plan_kw):
    global last_table
    tick(60)
    path, full = plan("mac", **plan_kw)
    mode = "full" if full else "cached"
    cr = crates(path, "test")
    swift = "run" if want(path, "swift-test") else "cached"
    if cr != "none":
        passed(path, 3, "test")
    if swift == "run" and not fail_swift:
        passed(path, 2, "swift-test")
    if fail_swift:
        last_table = sc.finish(path, failed="swift-test")
    else:
        last_table = sc.finish(path, ok=True)
    return "%s test=%s swift=%s" % (mode, cr, swift)


same("first run is full", fake_check(), "full test=all swift=run")
same("unchanged: everything cached", fake_check(), "cached test=none swift=cached")
check("the table names the cached suites", "cached suites: test (8 of 8 crates), swift-test" in last_table)
commit_file("crates/d/src/lib.rs", "// d2")
same("a dev-dependency's change reruns its dependents only", fake_check(), "cached test=a,d swift=cached")
commit_file("crates/a/src/lib.rs", "// a2")
same("a dependency's change reruns every crate above it", fake_check(), "cached test=a,b,c swift=cached")
commit_file("crates/c/src/lib.rs", "// c2")
same("a leaf change reruns the leaf only", fake_check(), "cached test=c swift=cached")
commit_file("Cargo.lock", "# lock 2")
same("a Cargo.lock change reruns all Rust", fake_check(), "cached test=all swift=cached")
os.environ["NVRVIEW_CACHE_TOOLS"] = "toolchain 2"
same("a toolchain change reruns everything", fake_check(), "cached test=all swift=run")
same("...and is cached after", fake_check(), "cached test=none swift=cached")
commit_file("apps/macos/App.swift", "// app 2")
same("a Swift change reruns the Swift suite only", fake_check(), "cached test=none swift=run")

# Failures are never cached.
commit_file("apps/macos/App.swift", "// app 3")
same("a failing suite runs", fake_check(fail_swift=True), "cached test=none swift=run")
check("the table shows the failure", "swift-test" in last_table and "FAILED" in last_table)
same("a failure is not cached", fake_check(), "cached test=none swift=run")

# A pass in a run retried after a flake is recorded but never skips a suite.
commit_file("apps/macos/App.swift", "// app 4")
same("a retried run runs the suite", fake_check(retried=True), "cached test=none swift=run")
same("a retried pass does not skip the suite", fake_check(), "cached test=none swift=run")
same("the later clean pass does", fake_check(), "cached test=none swift=cached")
# ...and the retried pass is kept as a .retried.json entry, never a plain one.
commit_file("apps/macos/App.swift", "// app 5")
tick(60)
rp, _ = plan("mac", retried=True)
want(rp, "swift-test")
passed(rp, 1, "swift-test")
rk = run_data(rp)["suites"]["swift-test"]["key"]
sc.finish(rp, ok=True)
ent = os.path.join(sc.store(), "entries", "mac", "swift-test")
check("a retried pass is recorded as retried", os.path.exists(os.path.join(ent, rk + ".retried.json"))
      and not os.path.exists(os.path.join(ent, rk + ".json")))
same("...and does not skip the suite", fake_check(), "cached test=none swift=run")

# A retry (NVRVIEW_RETRY_SINCE = start of the first try) takes the passes of
# the earlier tries of its own sequence, also those recorded as retried, also
# in a full run, and nothing recorded before that.
commit_file("apps/macos/App.swift", "// app 6")
tick(60)
since = int(os.environ["NVRVIEW_CACHE_NOW"])
tick(5)
p1, _ = plan("mac")
passed(p1, 1, "swift-test")
sc.finish(p1, ok=False, failed="other")
tick(5)
os.environ["NVRVIEW_RETRY_SINCE"] = str(since)
p2, _ = plan("mac", retried=True)
check("a retry skips a suite the first try passed", not want(p2, "swift-test"))
sc.finish(p2, ok=True)
commit_file("apps/macos/App.swift", "// app 7")
tick(60)
since = int(os.environ["NVRVIEW_CACHE_NOW"])
os.environ["NVRVIEW_RETRY_SINCE"] = str(since)
tick(5)
p3, _ = plan("mac", full=True, retried=True)
check("a retried full run runs a suite with no pass in its sequence", want(p3, "swift-test"))
passed(p3, 1, "swift-test")
sc.finish(p3, ok=False, failed="other")
tick(5)
p4, _ = plan("mac", full=True, retried=True)
check("...and skips it once a try of the sequence passed it (retried entry)", not want(p4, "swift-test"))
sc.finish(p4, ok=True)
os.environ["NVRVIEW_RETRY_SINCE"] = str(int(os.environ["NVRVIEW_CACHE_NOW"]) + 1)
tick(5)
p5, _ = plan("mac", retried=True)
check("a pass from before the sequence started is not taken", want(p5, "swift-test"))
sc.finish(p5, ok=True)
del os.environ["NVRVIEW_RETRY_SINCE"]
fake_check()  # a clean run records the plain pass again

# Uncommitted changes to a suite's inputs: it runs and is never recorded;
# staged changes count as content.
with open("crates/d/src/lib.rs", "a") as f:
    f.write("// local edit\n")
same("an unstaged input change reruns its suites", fake_check(), "cached test=a,d swift=cached")
same("...and is never recorded", fake_check(), "cached test=a,d swift=cached")
sh("git", "add", "crates/d/src/lib.rs")
same("staged, it is recorded", fake_check(), "cached test=a,d swift=cached")
same("...under the staged content", fake_check(), "cached test=none swift=cached")
write("untracked-notes.txt", "notes\n")
same("an untracked file outside every input changes nothing", fake_check(), "cached test=none swift=cached")
sh("git", "commit", "-q", "-m", "d edit")

# Forced full runs.
os.environ["NVRVIEW_FULL_CHECK"] = "1"
same("NVRVIEW_FULL_CHECK=1 runs everything", fake_check(), "full test=all swift=run")
del os.environ["NVRVIEW_FULL_CHECK"]
same("so does --full (the release scripts)", fake_check(full=True), "full test=all swift=run")
for script, needle in (("release-check.sh", "export NVRVIEW_FULL_CHECK=1"), ("release.sh", "NVRVIEW_FULL_CHECK=1 scripts/check.sh")):
    with open(os.path.join(HERE, script)) as f:
        check("%s forces full runs" % script, needle in f.read())
same("every Nth run is off by default", [fake_check() for _ in range(4)], ["cached test=none swift=cached"] * 4)
os.environ["NVRVIEW_FULL_EVERY"] = "3"
same("NVRVIEW_FULL_EVERY=3: the next run is the Nth since the full one", fake_check(), "full test=all swift=run")
same("...then two cached runs", [fake_check() for _ in range(2)], ["cached test=none swift=cached"] * 2)
same("...then a full one", fake_check(), "full test=all swift=run")
del os.environ["NVRVIEW_FULL_EVERY"]
tick(71 * 3600)
same("71 hours later passes are still cached", fake_check(), "cached test=none swift=cached")
tick(2 * 3600)
same("no full pass in 72 hours: full run", fake_check(), "full test=all swift=run")
same("...after which runs are cached again", fake_check(), "cached test=none swift=cached")
same("full-due is quiet when no full run is due", sc.full_reason(sc.store(), "mac"), None)
tick(73 * 3600)
check("full-due names the 72-hour rule", "72h" in (sc.full_reason(sc.store(), "mac") or ""))
os.environ["NVRVIEW_FULL_MAX_AGE_H"] = "200"
os.environ["NVRVIEW_CACHE_MAX_AGE_H"] = "200"
same("NVRVIEW_FULL_MAX_AGE_H and NVRVIEW_CACHE_MAX_AGE_H move both limits", fake_check(), "cached test=none swift=cached")
del os.environ["NVRVIEW_FULL_MAX_AGE_H"], os.environ["NVRVIEW_CACHE_MAX_AGE_H"]
same("a failed full run does not count as the full pass", fake_check(fail_swift=True), "full test=all swift=run")
same("...so the next run is full again", fake_check(), "full test=all swift=run")

# Platforms are separate; worktrees share the store.
check("another platform has its own entries", sc.full_reason(sc.store(), "windows") is not None)
sh("git", "worktree", "add", "-q", os.path.join(work, "wt"), "-b", "other")
os.chdir(os.path.join(work, "wt"))
same("a worktree of the same content reuses the main checkout's passes", fake_check(), "cached test=none swift=cached")
commit_file("crates/b/src/lib.rs", "// b in the worktree")
same("a worktree change reruns its dependents", fake_check(), "cached test=b,c swift=cached")
os.chdir(r)
sh("git", "merge", "-q", "other")
same("...and the main checkout then reuses the worktree's passes", fake_check(), "cached test=none swift=cached")

# The Windows app self-test is cached by area: the areas whose inputs changed
# rerun (with the startup smoke, which takes every change under apps/windows),
# and a change to shared code reruns them all. The C# suites cover the
# projects they build; the trimmed build's smoke (app-publish) is deferred except
# for the daily run and full runs.
for f in ("apps/windows/NvrView.App/SelfTest.cs", "apps/windows/NvrView.App/SelfTest.Recordings.cs",
          "apps/windows/NvrView.App/SelfTest.Ptz.cs", "apps/windows/NvrView.App/RecordingsWindow.cs",
          "apps/windows/NvrView.App/PtzControls.cs", "apps/windows/NvrView.App/StageView.cs",
          "apps/windows/NvrView.App/Updater.cs", "apps/windows/NvrView.App/Program.cs",
          "apps/windows/NvrView.Shell/Recordings.cs", "apps/windows/NvrView.Demo/Program.cs",
          "apps/windows/NvrView.Interop/Core.cs", "apps/windows/NvrView.Tiles/Zoom.cs",
          "apps/windows/NvrView.Video/Mf.cs", "apps/windows/NvrView.Smoke/Program.cs",
          "apps/windows/NvrView.Shell.Tests/Tests.cs", "apps/windows/NvrView.Tiles.Tests/Tests.cs",
          "apps/windows/layout/nvlayout.c", "apps/windows/Directory.Build.props",
          "scripts/windows-app.sh", "scripts/windows-selftest-areas.txt", "scripts/csharp-smoke.sh"):
    write(f, "// %s\n" % f)
sh("git", "add", "-A")
sh("git", "commit", "-q", "-m", "windows app files")
AREAS = sc.WINDOWS_AREAS
all_areas = ",".join(AREAS)
last_wtable = ""


def wcheck(fail_trimmed=False):
    """The areas to run, "<debug> | <publish or -> | <update or ->"; all pass."""
    global last_wtable
    tick(60)
    wrun, _ = plan("windows")
    out = []
    todo = []
    for a in AREAS:
        if want(wrun, "app-debug.%s" % a):
            todo.append(a)
            passed(wrun, 1, "app-debug.%s" % a)
    out.append(",".join(todo) or "-")
    if want(wrun, "app-publish"):
        out.append("publish")
        passed(wrun, 1, "app-publish")
    else:
        out.append("-")
    upd = "-"
    if want(wrun, "app-update"):
        upd = "update"
        passed(wrun, 1, "app-update")
    last_wtable = sc.finish(wrun, failed="app-update") if fail_trimmed else sc.finish(wrun, ok=True)
    return "%s | %s" % (" | ".join(out), upd)


same("the area list is what `areas` prints", AREAS, ["startup", "viewer", "tiles", "layouts", "header", "settings",
                                                       "calls", "recordings", "capture", "ptz"])
same("app areas: the first run runs every debug area, the trimmed smoke, and the update test", wcheck(),
     "%s | publish | update" % all_areas)
same("app areas: unchanged, all cached", wcheck(), "- | - | -")
with open("apps/windows/NvrView.App/RecordingsWindow.cs", "a") as f:
    f.write("// recordings window 2\n")
sh("git", "add", "-A")
sh("git", "commit", "-q", "-m", "recordings window")
same("app areas: a recordings view file reruns recordings and startup in the debug build only", wcheck(),
     "startup,recordings | - | -")
check("the table shows the deferred trimmed suite",
      any(l.startswith("app-publish") and "deferred" in l for l in last_wtable.splitlines()))
check("...and counts it like cached ones (the stamp is not a full one)",
      "cached suites:" in last_wtable and "app-publish (deferred)" in last_wtable)
commit_file("apps/windows/NvrView.Shell/Recordings.cs", "// shell recordings 2")
same("app areas: so does a recordings shell file", wcheck(), "startup,recordings | - | -")
commit_file("apps/windows/NvrView.App/SelfTest.Recordings.cs", "// self-test recordings 2")
same("app areas: so does the recordings self-test file", wcheck(), "startup,recordings | - | -")
commit_file("apps/windows/NvrView.App/PtzControls.cs", "// ptz 2")
same("app areas: a PTZ file reruns ptz and startup", wcheck(), "startup,ptz | - | -")
commit_file("apps/windows/NvrView.Demo/Program.cs", "// demo 2")
same("app areas: a project that is not part of the app reruns no area", wcheck(), "- | - | -")
commit_file("apps/windows/NvrView.App/StageView.cs", "// shared stage 2")
same("app areas: a shared app file reruns every area (debug only)", wcheck(), "%s | - | -" % all_areas)
commit_file("apps/windows/NvrView.App/SelfTest.cs", "// shared self-test 2")
same("app areas: the shared self-test file reruns every area", wcheck(), "%s | - | -" % all_areas)
commit_file("scripts/windows-selftest-areas.txt", "# table 2")
same("app areas: the area table reruns every area", wcheck(), "%s | - | -" % all_areas)
commit_file("fixtures/synthetic/clip.h264", "frame 2")
same("app areas: the fixtures rerun every area (and the update test, which loads the core)", wcheck(),
     "%s | - | update" % all_areas)
with open("apps/windows/NvrView.App/RecordingsWindow.cs", "a") as f:
    f.write("// local edit\n")
same("app areas: an unstaged edit of a recordings file runs recordings and startup", wcheck(), "startup,recordings | - | -")
same("app areas: ...every time, as it is never recorded", wcheck(), "startup,recordings | - | -")
sh("git", "checkout", "-q", "apps/windows/NvrView.App/RecordingsWindow.cs")
same("app areas: an area can pass alone: the others stay cached", wcheck(), "- | - | -")
# One area failing leaves its entry unrecorded while the others are kept.
with open("apps/windows/NvrView.App/PtzControls.cs", "a") as f:
    f.write("// ptz 3\n")
sh("git", "add", "-A")
sh("git", "commit", "-q", "-m", "ptz 3")
wrun, _ = plan("windows")
want(wrun, "app-debug.ptz")
want(wrun, "app-debug.startup")
passed(wrun, 1, "app-debug.startup")
sc.finish(wrun, failed="app-debug.ptz")
tick(60)
wrun, _ = plan("windows")
again = ",".join(a for a in AREAS if want(wrun, "app-debug." + a))
sc.finish(wrun, ok=True)
same("app areas: a failed area is run again, a passed one is not", again, "ptz")

# The trimmed build runs once a day: 24 hours after the last passing trimmed
# run the next check runs its smoke (and the update test), whatever
# the cache holds; a failed trimmed run does not count; a release (full run)
# always runs them.
tick(23 * 3600)
same("trimmed: 23 hours after the first run the trimmed smoke is still deferred", wcheck(), "ptz | - | -")
tick(2 * 3600)
same("trimmed: after 24 hours the trimmed smoke and the update test are due", wcheck(fail_trimmed=True),
     "- | publish | update")
check("trimmed: ...the failing update test shows in the table",
      any(l.startswith("app-update") and "FAILED" in l for l in last_wtable.splitlines()))
same("trimmed: a failed trimmed run does not count: they are due again", wcheck(), "- | publish | update")
same("trimmed: after a passing trimmed run they are deferred again", wcheck(), "- | - | -")
check("trimmed: a passing trimmed run is no full run", sc.full_reason(sc.store(), "windows") is None)
wrun, _ = plan("windows", full=True)
n = (1 if want(wrun, "app-publish") else 0) + (1 if want(wrun, "app-update") else 0)
sc.finish(wrun, failed="x")
same("trimmed: a full run (a release) runs the trimmed smoke and the update test", n, 2)
os.environ["NVRVIEW_TRIMMED_MAX_AGE_H"] = "1000"
tick(30 * 3600)
same("trimmed: NVRVIEW_TRIMMED_MAX_AGE_H moves the limit", wcheck(), "- | - | -")
del os.environ["NVRVIEW_TRIMMED_MAX_AGE_H"]
wcheck()  # the due trimmed run, so the C# checks below start from a settled state


# The C# suites cover the projects they build, and only those.
def csuites():
    tick(60)
    wrun, _ = plan("windows")
    out = []
    for s in ("csharp-smoke", "tiles-tests", "video-selftest", "shell-tests", "app-update"):
        if want(wrun, s):
            out.append(s)
            passed(wrun, 1, s)
    sc.finish(wrun, ok=True)
    return ",".join(out) or "-"


csuites()
same("C# suites: unchanged, all cached", csuites(), "-")
commit_file("apps/windows/NvrView.Video/Mf.cs", "// video 2")
same("C# suites: a video file reruns the video self-test only", csuites(), "video-selftest")
commit_file("apps/windows/NvrView.Demo/Program.cs", "// demo 3")
same("C# suites: so does the demo project it builds", csuites(), "video-selftest")
commit_file("apps/windows/NvrView.Shell.Tests/Tests.cs", "// shell tests 2")
same("C# suites: a shell test file reruns the shell tests only", csuites(), "shell-tests")
commit_file("apps/windows/NvrView.Tiles.Tests/Tests.cs", "// tiles tests 2")
same("C# suites: a tile test file reruns the tile tests only", csuites(), "tiles-tests")
commit_file("apps/windows/NvrView.Smoke/Program.cs", "// smoke 2")
same("C# suites: a smoke file reruns the smoke test only", csuites(), "csharp-smoke")
commit_file("apps/windows/NvrView.Interop/Core.cs", "// interop 2")
same("C# suites: interop reruns smoke, video, and the shell tests", csuites(), "csharp-smoke,video-selftest,shell-tests")
commit_file("apps/windows/NvrView.Tiles/Zoom.cs", "// tiles 2")
same("C# suites: the tiles project reruns every suite built on it", csuites(),
     "csharp-smoke,tiles-tests,video-selftest,shell-tests")
commit_file("apps/windows/NvrView.Shell/Recordings.cs", "// shell recordings 3")
same("C# suites: a shell file reruns smoke, video, and the shell tests", csuites(),
     "csharp-smoke,video-selftest,shell-tests")
commit_file("apps/windows/NvrView.App/RecordingsWindow.cs", "// recordings window 3")
same("C# suites: a recordings-only app file reruns none of them", csuites(), "-")
commit_file("apps/windows/NvrView.App/Updater.cs", "// updater 2")
same("C# suites: an updater file reruns the update test only", csuites(), "app-update")
commit_file("apps/windows/NvrView.App/Program.cs", "// program 2")
same("C# suites: so does the app's start-up handling", csuites(), "app-update")
commit_file("scripts/windows-app.sh", "# windows app 2")
same("C# suites: so does the packaging script", csuites(), "app-update")
commit_file("crates/updates/src/lib.rs", "// updates 2")
same("C# suites: the updates crate (signing tool) reruns the update test only", csuites(), "app-update")
commit_file("scripts/csharp-smoke.sh", "# smoke script 2")
same("C# suites: the smoke script reruns smoke and the tile tests", csuites(), "csharp-smoke,tiles-tests")
commit_file("fixtures/synthetic/clip.h264", "frame 3")
same("C# suites: the fixtures rerun the suites that play them (not the tile tests)", csuites(),
     "csharp-smoke,video-selftest,shell-tests,app-update")
commit_file("apps/windows/Directory.Build.props", "<!-- props 2 -->")
same("C# suites: the shared build settings rerun every suite", csuites(),
     "csharp-smoke,tiles-tests,video-selftest,shell-tests,app-update")
commit_file("crates/viewer-ffi/src/lib.rs", "// ffi 2")
same("C# suites: the core library reruns the ones that load it", csuites(), "csharp-smoke,video-selftest,app-update")

# The C# tests in Docker (Mac suites) are split the same way: one suite per
# project, and a change reruns only the ones built on it.
def dsuites():
    tick(60)
    mrun, _ = plan("mac")
    out = []
    for s in ("csharp-docker-smoke", "csharp-docker-shell", "csharp-docker-tiles"):
        if want(mrun, s):
            out.append(s)
            passed(mrun, 1, s)
    sc.finish(mrun, ok=True)
    return ",".join(out) or "-"


dsuites()
same("Docker C#: unchanged, all cached", dsuites(), "-")
commit_file("apps/windows/NvrView.Shell.Tests/Tests.cs", "// shell tests 3")
same("Docker C#: a shell test file reruns the shell tests only", dsuites(), "csharp-docker-shell")
commit_file("apps/windows/NvrView.Tiles.Tests/Tests.cs", "// tiles tests 3")
same("Docker C#: a tile test file reruns the tile tests only", dsuites(), "csharp-docker-tiles")
commit_file("apps/windows/NvrView.Smoke/Program.cs", "// smoke 3")
same("Docker C#: a smoke file reruns the smoke test only", dsuites(), "csharp-docker-smoke")
commit_file("crates/viewer-ffi/src/lib.rs", "// ffi 3")
same("Docker C#: the core library reruns the smoke test only", dsuites(), "csharp-docker-smoke")
commit_file("apps/windows/NvrView.Tiles/Zoom.cs", "// tiles 3")
same("Docker C#: the tiles project reruns the tile and shell tests and the smoke test", dsuites(),
     "csharp-docker-smoke,csharp-docker-shell,csharp-docker-tiles")
commit_file("scripts/csharp-check.sh", "# csharp check 2")
same("Docker C#: the Docker script reruns all three", dsuites(),
     "csharp-docker-smoke,csharp-docker-shell,csharp-docker-tiles")

# The Mac app self-test (fake core) is cached by area the same way
# (mac-app.AREA): the area files and the screens only one area exercises
# rerun that area and startup; shared code, the main self-test file, and the
# area table rerun every area; the demo and the unit tests rerun none.
for f in ("apps/macos/Sources/NVRView/SelfTest.swift", "apps/macos/Sources/NVRView/SelfTest+Recordings.swift",
          "apps/macos/Sources/NVRView/SelfTest+PTZ.swift", "apps/macos/Sources/NVRView/RecordingsWindow.swift",
          "apps/macos/Sources/NVRView/StageView.swift", "apps/macos/Sources/NVRCore/PTZ.swift",
          "apps/macos/Sources/AlertHost/AlertNotifier.swift", "apps/macos/Sources/NVRViewDemo/main.swift",
          "apps/macos/Tests/NVRCoreTests/PacingStatsTests.swift", "scripts/macos-selftest-areas.txt"):
    write(f, "// %s\n" % f)
sh("git", "add", "-A")
sh("git", "commit", "-q", "-m", "mac app files")
MAC_AREAS = sc.MAC_AREAS
all_mac = ",".join(MAC_AREAS)


def mcheck():
    """The Mac app self-test areas to run; all pass."""
    tick(60)
    mrun, _ = plan("mac")
    todo = [a for a in MAC_AREAS if want(mrun, "mac-app." + a)]
    for a in todo:
        passed(mrun, 1, "mac-app." + a)
    sc.finish(mrun, ok=True)
    return ",".join(todo) or "-"


same("mac areas: the first run runs every area", mcheck(), all_mac)
same("mac areas: unchanged, all cached", mcheck(), "-")
commit_file("apps/macos/Sources/NVRView/SelfTest+Recordings.swift", "// recordings steps 2")
same("mac areas: the recordings steps rerun recordings and startup", mcheck(), "startup,recordings")
commit_file("apps/macos/Sources/NVRView/RecordingsWindow.swift", "// recordings window 2")
same("mac areas: so does the recordings window", mcheck(), "startup,recordings")
commit_file("apps/macos/Sources/NVRCore/PTZ.swift", "// ptz 2")
same("mac areas: a PTZ file reruns ptz and startup", mcheck(), "startup,ptz")
commit_file("apps/macos/Sources/AlertHost/AlertNotifier.swift", "// alert host 2")
same("mac areas: a file of an owned directory reruns its area", mcheck(), "startup,alerts")
commit_file("apps/macos/Sources/NVRViewDemo/main.swift", "// demo 2")
commit_file("apps/macos/Tests/NVRCoreTests/PacingStatsTests.swift", "// tests 2")
same("mac areas: the demo and the unit tests rerun no area", mcheck(), "-")
commit_file("apps/macos/Sources/NVRView/StageView.swift", "// shared stage 2")
same("mac areas: a shared app file reruns every area", mcheck(), all_mac)
commit_file("apps/macos/Sources/NVRView/SelfTest.swift", "// shared self-test 2")
same("mac areas: the main self-test file reruns every area", mcheck(), all_mac)
commit_file("scripts/macos-selftest-areas.txt", "# table 2")
same("mac areas: the area table reruns every area", mcheck(), all_mac)
check("mac areas: the plan lists them for the shell",
      "macareas\t%s" % " ".join(MAC_AREAS) in sc.plan_text({"reason": "", "wants": {}, "crate_plans": {}}))

# DOTNET_REFS against the real C# project files.
here_cwd = os.getcwd()
os.chdir(os.path.join(HERE, ".."))
check("the project table matches the C# project files", sc.cmd_check_projects([]) == 0)
os.chdir(here_cwd)

# The store stays bounded: entries older than 14 days are pruned.
entries = os.path.join(sc.store(), "entries")
old = os.path.join(entries, "mac", "test.zz", "0000.json")
write(old, '{"time": 0}')
os.utime(old, (946684800, 946684800))
fake_check()
check("old entries are pruned", not os.path.exists(old))
runs = os.path.join(sc.store(), "runs")
check("finished runs leave no run file", not os.listdir(runs))

print()
if failures:
    print("%d test(s) failed" % failures, file=sys.stderr)
    sys.exit(1)
print("all suite cache decision tests passed")
