#!/usr/bin/env python3
"""Per-suite test result cache for scripts/check.sh and scripts/windows-check.sh.

A suite (one crate's tests, the C smoke test, the Swift tests, a Windows app
self-test, ...) is skipped when a clean pass is cached for the same platform
and the same fingerprint within NVRVIEW_CACHE_MAX_AGE_H hours (default 72).

Fingerprint: sha256 over the suite's input files as git records them in the
index (path, mode, blob id; committed plus staged content, so unrelated
uncommitted files do not matter), the tool versions, and the suite name. A
suite with an input that differs between the index and the working tree, or
an untracked input file, is "dirty": it always runs and is never recorded.

Inputs: a Rust crate's suites cover the crate and every workspace crate it
depends on transitively (all dependency kinds for the crate itself, normal and
build dependencies below it; from `cargo metadata`), the workspace files
(Cargo.toml, Cargo.lock, rust-toolchain, .cargo/config.toml, the nextest and
lint settings) and fixtures/synthetic/. Other suites list their inputs in
SUITES below.

Full runs (nothing cached) happen with NVRVIEW_FULL_CHECK=1 or --full (the
release scripts), when no full run on the platform passed within
NVRVIEW_FULL_MAX_AGE_H hours (default 72), and, when NVRVIEW_FULL_EVERY=N is
set, on every Nth run. A full run refreshes every entry it passes.

The store is <git common dir>/nvrview-checks/suite-cache/ (shared by all
worktrees): entries/<platform>/<suite>/<key>.json per clean pass (a pass in a
run retried after a known flake is kept as <key>.retried.json and never
skips anything), state-<platform>.json (last full pass, runs since), and
runs/<id>.json for a run in progress. Entries older than 14 days are pruned,
and at most 20000 are kept. Failures are never recorded.

Commands (RUN is the path that `plan` prints):
  plan PLATFORM [--full] [--retried] [--tools-file F]
                        start a run and decide every suite once; prints RUN.
                        Writes RUN.json (the decisions and fingerprints),
                        RUN.plan (the same decisions as tab-separated lines for
                        the shell: full, areas, want, crates, cmsg) and an
                        empty RUN.log
  finish RUN [--ok [--partial]] [--failed LABEL]
                        folds RUN.log into the store, prints the table, ends
                        the run (removes the three files); a full run counts
                        as the platform's full pass only with --ok and without
                        --partial (steps were skipped)
  full-due PLATFORM     exit 0 when the next run on PLATFORM would be full
  areas windows         the Windows app self-test areas (suites app-debug.AREA;
                        see WINDOWS_OWNED below)
  check-projects        fails when DOTNET_REFS differs from the C# project files

The scripts call Python twice per run (plan, finish), not once per suite. The
shell reads its answers from RUN.plan and appends one tab-separated line per
event to RUN.log (plain printf, no Python):
  want SUITE            SUITE was reported as cached or deferred (a row)
  crates FAMILY         the crates of a Rust family were selected
  pass SECS NAME...     a suite, a family, or a plain step name (shown only in
                        the table) passed
finish replays the log in order, so a run that was interrupted still records
the suites that had completed. In RUN.plan, `want SUITE run` means run it,
`want SUITE cached|deferred LINE` carries the line to print, and `crates FAMILY
ARGS LIST` the cargo arguments and the --list form (`cmsg` lines are the
cached-crate messages for stderr). A suite not in the plan runs.

The trimmed publish build and what runs on it (app-publish: the plain start and
the short self-test, `--smoke`, startup and one wiring check per area) are deferred
in ordinary runs ("deferred ...", counted like cached suites: a stamp with them
serves merges, not a release); they run in full runs and when the last passing
trimmed run on the platform is older than NVRVIEW_TRIMMED_MAX_AGE_H (24). app-update
(the update test, also on the trimmed build) runs then too, and otherwise
when its own inputs change.

For tests: NVRVIEW_CACHE_NOW (epoch seconds), NVRVIEW_CACHE_METADATA (a
`cargo metadata` JSON file), NVRVIEW_CACHE_TOOLS (tool versions, one string).
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import time

HARNESS = "suite-cache 2"  # bump to invalidate every entry
KEEP_DAYS = 14
KEEP_MAX = 20000
FAMILIES = ("clippy", "test", "doctest", "wincheck", "linuxcheck")
RUST_SHARED = [
    "Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml",
    ".cargo/config.toml", ".config/nextest.toml", "clippy.toml", ".clippy.toml",
    "rustfmt.toml", ".rustfmt.toml", "fixtures/synthetic/",
]
FIXTURES = ["fixtures/synthetic/"]

# The C# projects under apps/windows and the projects each references (kept in
# step with the project files: `suite-cache.py check-projects`, run by
# scripts/test_suite_cache.py).
W = "apps/windows/"
DOTNET_REFS = {
    "Interop": [], "Tiles": [], "Shell": ["Tiles", "Interop"],
    "Video": ["Interop", "Tiles", "Shell"], "Demo": ["Interop", "Video"],
    "Smoke": ["Interop", "Shell"], "Shell.Tests": ["Shell"], "Tiles.Tests": ["Tiles"],
    "App": ["Interop", "Video", "Tiles", "Shell"],
}


def dotnet_inputs(project):
    """The directories of PROJECT and every project it references, and the shared build settings."""
    seen, todo = set(), [project]
    while todo:
        p = todo.pop()
        if p not in seen:
            seen.add(p)
            todo += DOTNET_REFS[p]
    return sorted(W + "NvrView.%s/" % p for p in seen) + [W + "Directory.Build.props"]


# What the side-by-side update test exercises: the updater and the start-up
# handling, the app project (what is packaged), the updates model.
UPDATE_FILES = [W + "NvrView.App/" + f for f in (
    "Updater.cs", "Program.cs", "StartupTest.cs", "App.xaml.cs", "NvrView.App.csproj", "app.manifest",
    "UpdateSettingsPanel.cs")] + [W + "NvrView.Shell/Updates.cs", W + "Directory.Build.props"]

# Non-Rust suites: platform -> name -> (input paths, crates whose closure is
# an input, tool kinds). A path ending in "/" is a directory.
SUITES = {
    "mac": {
        "script-tests": (["scripts/"], [], []),
        "c-header": (["scripts/gen-header.sh"], ["viewer-ffi"], ["rust", "cbindgen"]),
        "c-smoke": (["scripts/test-c-ffi.sh"] + FIXTURES, ["viewer-ffi", "mock-nvr"], ["rust", "cc"]),
        "c-smoke-asan": (["scripts/test-c-ffi.sh"] + FIXTURES, ["viewer-ffi", "mock-nvr"], ["rust", "cc"]),
        "linux-tests": (["scripts/linux-check.sh", "scripts/test-c-ffi.sh"], ["*"], ["linux-image"]),
        "csharp-docker": (["apps/windows/", "scripts/csharp-check.sh", "VERSION"] + FIXTURES,
                          ["viewer-ffi", "mock-nvr"], ["linux-image", "dotnet-image"]),
        # The Swift package compiles against a header-only copy of the C
        # interface (docs/nvrview.h) and never links the Rust library by
        # default, so the Rust crates are not its inputs; the header is.
        "swift-test": (["apps/macos/", "docs/nvrview.h", "crates/viewer-ffi/include/", "VERSION"] + FIXTURES, [], ["swift"]),
        "macos-demo-metal": (["apps/macos/", "docs/nvrview.h", "crates/viewer-ffi/include/", "VERSION"] + FIXTURES, [], ["swift"]),
        "macos-demo-layer": (["apps/macos/", "docs/nvrview.h", "crates/viewer-ffi/include/", "VERSION"] + FIXTURES, [], ["swift"]),
        "browser": ([""], [], ["rust"]),  # the whole tree
    },
    "windows": {
        # Each C# suite covers the projects it builds (their project
        # references, transitively), the shared build settings, and, for those
        # that load the core, the core crates and the fixtures.
        "csharp-smoke": (dotnet_inputs("Smoke") + [W + "layout/", "scripts/csharp-smoke.sh", "VERSION"] + FIXTURES,
                         ["viewer-ffi", "mock-nvr"], ["rust"]),
        # The tile features' logic on simulated time (managed code only).
        "tiles-tests": (dotnet_inputs("Tiles.Tests") + ["scripts/csharp-smoke.sh", "VERSION"], [], ["rust"]),
        "video-selftest": (dotnet_inputs("Demo") + ["scripts/csharp-video-selftest.sh", "VERSION"] + FIXTURES,
                           ["viewer-ffi", "mock-nvr"], ["rust"]),
        # The shell tests are managed code only (no native library).
        "shell-tests": (dotnet_inputs("Shell.Tests") + ["VERSION"] + FIXTURES, [], ["rust"]),
        # The side-by-side update end to end, on the trimmed build: the
        # updater and start-up handling, the packaging script, and the
        # signing tool's crates. The rest of the app is the self-test areas'.
        "app-update": (UPDATE_FILES + ["scripts/windows-app.sh", "VERSION"], ["viewer-ffi", "sign-update"], ["rust"]),
        # app-debug.AREA and app-publish are added below (WINDOWS_AREAS).
    },
}

# The Windows app self-test runs by area (scripts/windows-selftest-areas.txt;
# SelfTest.AreaNames in the app). Each area of the debug build is cached on its
# own (app-debug.AREA). The trimmed publish build runs only a short smoke
# (app-publish: startup and one wiring check per area, SelfTest.Smoke.cs; the
# trimming itself is checked statically), so it is one suite over the whole app.
# An area's inputs are all of apps/windows/ except the projects that are not
# part of the app and the files that only some other area exercises
# (WINDOWS_OWNED), so a file nobody owns and every shared one (App.xaml.cs,
# CoreHost, StageView, the renderer, SelfTest.cs, the interop, ...) is an
# input of every area. "startup", the smoke every run includes, takes
# everything under apps/windows/, owned files too: any change reruns it.
# Every area also takes the core crates the app loads, the area table, and
# the fixtures.
WINDOWS_AREAS = ["startup", "viewer", "tiles", "layouts", "header", "settings",
                 "calls", "recordings", "capture", "ptz"]
WINDOWS_NOT_APP = ["apps/windows/NvrView.Demo/", "apps/windows/NvrView.Smoke/",
                   "apps/windows/NvrView.Shell.Tests/", "apps/windows/NvrView.Tiles.Tests/",
                   "apps/windows/layout/", "apps/windows/README.md"]
_A, _S, _T, _I, _V = ("apps/windows/NvrView.App/", "apps/windows/NvrView.Shell/", "apps/windows/NvrView.Tiles/",
                      "apps/windows/NvrView.Interop/", "apps/windows/NvrView.Video/")
WINDOWS_OWNED = {
    "viewer": [_A + "SelfTest.Viewer.cs", _T + "Zoom.cs"],
    "tiles": [_A + "SelfTest.Tiles.cs", _A + "IndicatorStrip.cs", _A + "QualityMenu.cs",
              _T + "Alerts.cs", _T + "Detections.cs", _T + "Fisheye.cs", _T + "Indicators.cs", _T + "Quality.cs"],
    "layouts": [_A + "SelfTest.Layouts.cs", _S + "SidebarReveal.cs"],
    "header": [_A + "SelfTest.Header.cs", _A + "HeaderBar.cs", _A + "LayoutIcon.cs", _S + "HeaderChrome.cs"],
    "settings": [_A + "SelfTest.Settings.cs", _A + "SettingsPages.cs", _A + "SettingsWindow.cs",
                 _A + "UpdateSettingsPanel.cs", _A + "UpdateDefaultsSelfTest.cs", _A + "UpdateNotesSelfTest.cs",
                 _A + "UpdateNotesView.cs", _A + "Updater.cs", _S + "Discovery.cs", _S + "Updates.cs"],
    "calls": [_A + "SelfTest.Calls.cs", _A + "CallPanel.cs", _A + "Calls.cs", _A + "CallServices.cs",
              _A + "CallWindows.cs", _A + "TranscriptStore.cs", _T + "CallWindow.cs", _T + "Calls.cs",
              _T + "CaptionEcho.cs", _T + "EchoCancellation.cs", _T + "Insets.cs", _T + "TranscriptFiles.cs"],
    "recordings": [_A + "SelfTest.Recordings.cs", _A + "SelfTest.Export.cs", _A + "EditAudio.cs", _A + "EditStage.cs",
                   _A + "ExportSynthetic.cs", _A + "ExportWriters.cs", _A + "RecordingsHost.cs",
                   _A + "RecordingsTimeline.cs", _A + "RecordingsWindow.cs", _A + "RecordingsWindow.Export.cs",
                   _A + "RecordingsWindow.Inspector.cs", _A + "RecordingsWindow.Player.cs",
                   _A + "RecordingsWindow.Sidebar.cs",
                   _S + "EditPlan.cs", _S + "EditPlayer.cs", _S + "ExportController.cs", _S + "ExportPlan.cs",
                   _S + "FakePlayback.cs", _S + "FakeRecordings.Export.cs", _S + "FakeRecordings.Sound.cs",
                   _S + "FakeRecordings.cs", _S + "Mp4Index.cs", _S + "Mp4Inspector.cs", _S + "Mp4ShareSafe.cs",
                   _S + "Playback.cs", _S + "PlayerRules.cs", _S + "Recordings.cs", _S + "RecordingsBrowse.cs",
                   _S + "RecordingsBrowser.cs", _S + "RecordingsPlayer.cs", _S + "RecordingsService.Export.cs",
                   _S + "RecordingsService.cs", _S + "RecordingsSound.cs", _S + "SoundAnalysis.cs",
                   _S + "ThumbnailDecoding.cs", _S + "ToneCheck.cs",
                   _I + "NvCore.EditSound.cs", _I + "NvCore.Export.cs", _I + "NvCore.Playback.cs",
                   _I + "NvCore.Recordings.cs",
                   _V + "AacRoundTrip.cs", _V + "EditFiles.cs", _V + "EditFrameRenderer.cs", _V + "KeyframeImage.cs",
                   _V + "MfExport.cs", _V + "RecordedFile.cs", _V + "SyntheticRecording.cs"],
    "capture": [_A + "SelfTest.Capture.cs", _A + "SelfTest.CaptureRecord.cs", _A + "CaptureController.cs",
                _A + "CaptureSettingsPage.cs", _A + "PictureSaver.cs", _A + "StageView.Capture.cs",
                _S + "Capture.cs", _S + "CaptureRecordings.cs", _S + "FakeCapture.cs", _T + "Pictures.cs",
                _I + "NvCore.Capture.cs", _V + "AacCaptureEncoder.cs", _V + "PictureRender.cs", _V + "TilePicture.cs"],
    "ptz": [_A + "SelfTest.Ptz.cs", _A + "PtzControls.cs", _A + "StageView.Ptz.cs", _A + "Viewer.Ptz.cs",
            _S + "FakePtz.cs", _S + "Ptz.cs", _S + "PtzModel.cs"],
}
WINDOWS_APP_EXTRA = ["scripts/windows-app.sh", "scripts/windows-selftest-areas.txt", "VERSION"] + FIXTURES


def windows_area_excludes(area):
    out = list(WINDOWS_NOT_APP)
    if area != "startup":
        for other, paths in WINDOWS_OWNED.items():
            if other != area:
                out += paths
    return out


for _area in WINDOWS_AREAS:
    SUITES["windows"]["app-debug.%s" % _area] = (
        ["apps/windows/"] + WINDOWS_APP_EXTRA, ["viewer-ffi", "mock-nvr"], ["rust"], windows_area_excludes(_area))
SUITES["windows"]["app-publish"] = (
    ["apps/windows/"] + WINDOWS_APP_EXTRA, ["viewer-ffi", "mock-nvr"], ["rust"], windows_area_excludes("startup"))

RUST_FAMILIES = {"mac": FAMILIES, "windows": ("clippy", "test", "doctest")}


def now():
    return int(os.environ.get("NVRVIEW_CACHE_NOW") or time.time())


def hours(name, default):
    v = os.environ.get(name, "")
    return float(v) if v else default


def git(*args):
    return subprocess.run(("git",) + args, check=True, capture_output=True).stdout


def store():
    common = git("rev-parse", "--git-common-dir").decode().strip()
    return os.path.join(os.path.abspath(common), "nvrview-checks", "suite-cache")


def write_json(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = "%s.%d.tmp" % (path, os.getpid())
    with open(tmp, "w") as f:
        json.dump(data, f, indent=1, sort_keys=True)
    os.replace(tmp, path)


def read_json(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def age(secs):
    secs = max(0, int(secs))
    if secs < 60:
        return "%ds" % secs
    if secs < 3600:
        return "%dm" % (secs // 60)
    return "%dh%02dm" % (secs // 3600, secs % 3600 // 60)


# --- inputs ------------------------------------------------------------------

def tool(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, timeout=60)
        return (r.stdout + r.stderr).decode(errors="replace").strip()
    except (OSError, subprocess.TimeoutExpired):
        return "missing"


def mac_tools(kind):
    env = os.environ.get
    return {
        "rust": lambda: "\n".join([tool(["rustc", "-vV"]), tool(["cargo", "nextest", "--version"]),
                                   tool(["cargo", "clippy", "--version"]),
                                   "RUSTFLAGS=" + env("RUSTFLAGS", "")]),
        "cbindgen": lambda: tool([os.path.expanduser("~/.cargo/bin/cbindgen"), "--version"])
        if os.path.exists(os.path.expanduser("~/.cargo/bin/cbindgen")) else tool(["cbindgen", "--version"]),
        "cc": lambda: tool(["cc", "--version"]),
        "swift": lambda: tool(["swift", "--version"]),
        "linux-image": lambda: env("NVRVIEW_LINUX_IMAGE", "rust:1-bookworm"),
        "dotnet-image": lambda: env("NVRVIEW_DOTNET_IMAGE", "default"),
    }[kind]()


class Tree:
    """The index, the dirty paths, and the workspace crate graph."""

    def __init__(self, platform, tools):
        self.platform = platform
        self.tools = tools  # kind -> version string (filled lazily on the Mac)
        self.files = []
        for rec in git("ls-files", "-s", "-z").split(b"\0"):
            if rec:
                meta, path = rec.split(b"\t", 1)
                mode, blob, _stage = meta.split(b" ")
                self.files.append((path.decode(), mode.decode(), blob.decode()))
        self.files.sort()
        self.dirty = set()
        for rec in git("status", "--porcelain=v1", "-z", "--untracked-files=all").split(b"\0"):
            if len(rec) > 3:
                xy, path = rec[:2].decode(), rec[3:].decode()
                if xy[1] != " " or xy == "??":
                    self.dirty.add(path)
        self.crates = self._crates()

    def _crates(self):
        meta_file = os.environ.get("NVRVIEW_CACHE_METADATA")
        if meta_file:
            meta = read_json(meta_file)
        else:
            cargo = shutil.which("cargo") or next(
                (p for p in (os.path.expanduser("~/.cargo/bin/cargo"), "/opt/homebrew/opt/rustup/bin/cargo")
                 if os.path.exists(p)), "cargo")
            out = subprocess.run([cargo, "metadata", "--format-version", "1", "--no-deps", "--offline"],
                                 check=True, capture_output=True).stdout
            meta = json.loads(out)
        root = os.path.abspath(meta.get("workspace_root", "."))
        crates = {}
        for p in meta["packages"]:
            d = os.path.relpath(os.path.dirname(p["manifest_path"]), root).replace(os.sep, "/")
            kinds = {k for t in p.get("targets", []) for k in t["kind"]}
            crates[p["name"]] = {
                "dir": d + "/",
                "lib": bool(kinds & {"lib", "rlib", "dylib", "proc-macro"}),
                "deps": [(x["name"], x.get("kind")) for x in p["dependencies"] if x.get("path")],
            }
        return crates

    def closure(self, name):
        if name == "*":
            return sorted(self.crates)
        seen = {name}
        todo = [d for d, _ in self.crates[name]["deps"] if d in self.crates]
        while todo:
            c = todo.pop()
            if c in seen:
                continue
            seen.add(c)
            todo += [d for d, k in self.crates[c]["deps"] if k != "dev" and d in self.crates]
        return sorted(seen)

    def tool(self, kind):
        if kind not in self.tools:
            self.tools[kind] = mac_tools(kind)
        return self.tools[kind]

    def key(self, suite, paths, tool_kinds, excludes=()):
        """(key, dirty input or None). EXCLUDES are left out (a trailing / is a directory)."""
        ex_dirs = tuple(e for e in excludes if e.endswith("/"))
        ex_files = set(e for e in excludes if not e.endswith("/"))

        def skip(path):
            return path in ex_files or path.startswith(ex_dirs)

        prefixes = tuple(sorted(set(p for p in paths if p.endswith("/") or p == "")))
        exact = set(p for p in paths if not (p.endswith("/") or p == ""))
        h = hashlib.sha256()
        h.update(("%s\n%s\n%s\n" % (HARNESS, self.platform, suite)).encode())
        for k in sorted(set(tool_kinds)):
            h.update(("tool %s %s\n" % (k, self.tool(k))).encode())
        for path, mode, blob in self.files:
            if (path in exact or path.startswith(prefixes)) and not skip(path):
                h.update(("%s %s %s\n" % (mode, blob, path)).encode())
        dirty = sorted(p for p in self.dirty if (p in exact or p.startswith(prefixes)) and not skip(p))
        return h.hexdigest(), (dirty[0] if dirty else None)

    def crate_inputs(self, roots):
        dirs = []
        for r in roots:
            dirs += [self.crates[c]["dir"] for c in self.closure(r)]
        return sorted(set(dirs)) + RUST_SHARED

    def suite_keys(self):
        """suite -> {key, dirty}; Rust families as FAMILY.CRATE."""
        out = {}
        for c in self.crates:
            paths = self.crate_inputs([c])
            for fam in RUST_FAMILIES[self.platform]:
                if fam == "doctest" and not self.crates[c]["lib"]:
                    continue
                k, d = self.key("%s.%s" % (fam, c), paths, ["rust"])
                out["%s.%s" % (fam, c)] = {"key": k, "dirty": d}
        for name, (paths, roots, kinds, *rest) in SUITES[self.platform].items():
            roots = [r for r in roots if r == "*" or r in self.crates]
            all_paths = list(paths) + (self.crate_inputs(roots) if roots else [])
            k, d = self.key(name, all_paths, kinds, rest[0] if rest else ())
            out[name] = {"key": k, "dirty": d}
        return out


# --- state and entries -------------------------------------------------------

def entry_path(base, platform, suite, key, retried=False):
    return os.path.join(base, "entries", platform, suite, key + (".retried.json" if retried else ".json"))


def is_trimmed(suite):
    """The suites that need the trimmed publish build."""
    return suite in ("app-publish", "app-update")


def trimmed_reason(base, platform, full):
    """Why this run must run the trimmed-build suites, or None (they are deferred).

    The trimmed publish and its self-test take minutes and fail only when
    trimming breaks something, so ordinary runs defer them; they run in full
    runs (releases) and when the last passing trimmed run on the platform is
    older than NVRVIEW_TRIMMED_MAX_AGE_H hours (default 24).
    """
    if platform != "windows":
        return None
    if full:
        return "full run"
    last = read_json(os.path.join(base, "state-%s.json" % platform), {}).get("last_trimmed")
    max_age = hours("NVRVIEW_TRIMMED_MAX_AGE_H", 24)
    if not last:
        return "no trimmed-build run has passed on %s yet" % platform
    if now() - last > max_age * 3600:
        return "the last trimmed-build run on %s was %s ago (every %gh)" % (platform, age(now() - last), max_age)
    return None


def full_reason(base, platform):
    """Why the next run must be full, or None."""
    if os.environ.get("NVRVIEW_FULL_CHECK", "0") == "1":
        return "NVRVIEW_FULL_CHECK=1"
    state = read_json(os.path.join(base, "state-%s.json" % platform), {})
    last = state.get("last_full")
    max_age = hours("NVRVIEW_FULL_MAX_AGE_H", 72)
    if not last:
        return "no full run has passed on %s yet" % platform
    if now() - last > max_age * 3600:
        return "the last full pass on %s was %s ago (every %gh)" % (platform, age(now() - last), max_age)
    every = os.environ.get("NVRVIEW_FULL_EVERY", "")
    if every and int(every) > 0 and state.get("runs_since_full", 0) + 1 >= int(every):
        return "NVRVIEW_FULL_EVERY=%s" % every
    return None


def prune(base):
    root = os.path.join(base, "entries")
    found = []
    for d, _, files in os.walk(root):
        for f in files:
            p = os.path.join(d, f)
            found.append((os.path.getmtime(p), p))
    found.sort()
    cutoff = time.time() - KEEP_DAYS * 86400
    excess = len(found) - KEEP_MAX
    for i, (mtime, p) in enumerate(found):
        if mtime < cutoff or i < excess:
            try:
                os.remove(p)
            except OSError:
                pass


# --- commands ----------------------------------------------------------------
#
# `plan` decides every suite once, from the tree, the platform, the tools and
# the store as they are at the start of the run, and writes the decisions to
# RUN.json and a tab-separated RUN.plan that the shell reads (no Python per
# suite). The shell appends what happened to RUN.log; `finish` replays the log
# in order, so the table is the same as when each step called Python itself.

def want_decision(base, run, suite):
    """Whether SUITE runs: {"status": run|cached|deferred, "line": text printed when it does not}."""
    if is_trimmed(suite) and run["platform"] == "windows" and not run["full"]:
        if run.get("trimmed"):
            return {"status": "run", "line": ""}  # the daily trimmed-build run is due
        if suite != "app-update":
            last = read_json(os.path.join(base, "state-windows.json"), {}).get("last_trimmed", now())
            return {"status": "deferred", "line": "deferred %s (trimmed build; last passed %s ago, runs daily and for releases)"
                    % (suite, age(now() - last))}
    e = cached_entry(base, run, suite)
    if e is None:
        return {"status": "run", "line": ""}
    return {"status": "cached", "line": "cached pass %s (%s, key %s)" % (
        suite, age(now() - e["time"]), run["suites"][suite]["key"][:10])}


def crates_decision(base, run, fam):
    """The crates of FAM to run: cargo arguments, the --list form, messages, and the table label."""
    names = sorted(s.split(".", 1)[1] for s in run["suites"] if s.startswith(fam + "."))
    todo, cached = [], []
    for c in names:
        e = cached_entry(base, run, "%s.%s" % (fam, c))
        (todo if e is None else cached).append((c, e))
    if len(cached) <= 3:
        msgs = ["cached pass %s.%s (%s, key %s)" % (fam, c, age(now() - e["time"]),
                                                    run["suites"]["%s.%s" % (fam, c)]["key"][:10]) for c, e in cached]
    else:
        ages = sorted(now() - e["time"] for _, e in cached)
        msgs = ["cached pass %s for %d crates: %s (%s to %s old)" % (
            fam, len(cached), ", ".join(c for c, _ in cached), age(ages[0]), age(ages[-1]))]
    # Doctest suites exist for library crates only, which is what
    # `cargo test --doc --workspace` covers.
    all_crates = len(todo) == len(names)
    return {
        "todo": [c for c, _ in todo], "msgs": msgs,
        "label": "cached %d of %d crates" % (len(cached), len(names)),
        "cached_label": "%s (%d of %d crates)" % (fam, len(cached), len(names)) if cached else "",
        "args": ("--workspace" if all_crates else " ".join("-p %s" % c for c, _ in todo)) if todo else "",
        "list": "all" if todo and all_crates else ",".join(c for c, _ in todo) or "none",
    }


def plan_text(run):
    """The decisions as tab-separated lines for the shell."""
    lines = ["full\t%s" % run["reason"], "areas\t%s" % " ".join(WINDOWS_AREAS)]
    for suite, d in sorted(run["wants"].items()):
        lines.append("want\t%s\t%s\t%s" % (suite, d["status"], d["line"]))
    for fam, d in run["crate_plans"].items():
        for m in d["msgs"]:
            lines.append("cmsg\t%s\t%s" % (fam, m))
        lines.append("crates\t%s\t%s\t%s" % (fam, d["args"], d["list"]))
    return "\n".join(lines) + "\n"


def run_stem(path):
    return path[:-len(".json")]


def plan(platform, full=False, retried=False, tools_file=None):
    """Starts a run. Returns (RUN path, the one-line message for stderr)."""
    tools = {}
    if tools_file:
        with open(tools_file) as f:
            text = f.read()
        tools = {"rust": text, "cbindgen": text, "cc": text, "swift": text}
    if os.environ.get("NVRVIEW_CACHE_TOOLS") is not None:
        t = os.environ["NVRVIEW_CACHE_TOOLS"]
        tools = {k: t for k in ("rust", "cbindgen", "cc", "swift", "linux-image", "dotnet-image")}
    base = store()
    reason = "--full" if full else full_reason(base, platform)
    tree = Tree(platform, tools)
    run = {
        "platform": platform, "full": bool(reason), "reason": reason or "", "retried": retried,
        "started": now(), "tools": tree.tools, "suites": tree.suite_keys(),
        "trimmed": trimmed_reason(base, platform, bool(reason)) or "",
    }
    run["tools"] = tree.tools  # filled while computing the keys
    run["wants"] = {s: want_decision(base, run, s) for s in run["suites"]}
    run["crate_plans"] = {f: crates_decision(base, run, f) for f in FAMILIES}
    state_file = os.path.join(base, "state-%s.json" % platform)
    state = read_json(state_file, {})
    if reason:
        message = "full run: %s" % reason
    else:
        state["runs_since_full"] = state.get("runs_since_full", 0) + 1
        write_json(state_file, state)
        message = ("cached run: suites with a clean pass in the last %gh are skipped (last full pass %s ago; NVRVIEW_FULL_CHECK=1 runs everything)"
                   % (hours("NVRVIEW_CACHE_MAX_AGE_H", 72), age(now() - state.get("last_full", now()))))
    path = os.path.join(base, "runs", "%s-%d-%d.json" % (platform, now(), os.getpid()))
    write_json(path, run)
    with open(run_stem(path) + ".plan", "w") as f:
        f.write(plan_text(run))
    open(run_stem(path) + ".log", "w").close()
    return path, message


def cached_entry(base, run, suite):
    """The cached clean pass that lets SUITE be skipped, or None."""
    if run["full"]:
        return None
    s = run["suites"].get(suite)
    if not s or s["dirty"]:
        return None
    e = read_json(entry_path(base, run["platform"], suite, s["key"]))
    if not e or e.get("retried") or now() - e["time"] > hours("NVRVIEW_CACHE_MAX_AGE_H", 72) * 3600:
        return None
    return e


def replay(run, log_path):
    """Folds the shell's event log into the run: rows, cached suites, passed suites."""
    run["rows"], run["cached"], run["passed"], run["selected"] = [], [], {}, {}
    try:
        with open(log_path) as f:
            events = [line.rstrip("\n").split("\t") for line in f if line.strip()]
    except OSError:
        events = []
    for ev in events:
        if ev[0] == "want" and len(ev) == 2:
            d = run["wants"].get(ev[1])
            if d and d["status"] == "cached":
                run["rows"].append([ev[1], "cached", 0])
                run["cached"].append(ev[1])
            elif d and d["status"] == "deferred":
                run["rows"].append([ev[1], "deferred", 0])
                run["cached"].append(ev[1] + " (deferred)")
        elif ev[0] == "crates" and len(ev) == 2:
            d = run["crate_plans"].get(ev[1])
            if d:
                run["selected"][ev[1]] = d["todo"]
                run["rows"].append([ev[1], d["label"], 0])
                if d["cached_label"]:
                    run["cached"].append(d["cached_label"])
        elif ev[0] == "pass" and len(ev) >= 3 and ev[1].isdigit():
            pass_event(run, int(ev[1]), ev[2:])


def pass_event(run, secs, names):
    for name in names:
        if name in run["selected"]:
            sel = run["selected"][name]
            for c in sel:
                run["passed"]["%s.%s" % (name, c)] = True
            label = "ran %d crate%s" % (len(sel), "" if len(sel) == 1 else "s")
            for row in run["rows"]:
                if row[0] == name and row[1].startswith("cached ") and " of " in row[1]:
                    if not row[1].startswith("cached 0 "):
                        label += ", " + row[1]
                    run["rows"].remove(row)
                    break
            run["rows"].append([name, label, secs])
        else:
            if name in run["suites"]:
                run["passed"][name] = True
            run["rows"].append([name, "ran", secs])


def finish(path, ok=False, partial=False, failed=""):
    """Records the passes, ends the run, and returns the table text."""
    run = read_json(path)
    if run is None:
        return ""
    stem = run_stem(path)
    replay(run, stem + ".log")
    base = store()
    # Record only suites whose inputs are what they were at the start.
    tree = Tree(run["platform"], run["tools"])
    keys = tree.suite_keys() if run["passed"] else {}
    changed = 0
    for suite in run["passed"]:
        s = run["suites"].get(suite)
        k = keys.get(suite)
        if not s or s["dirty"] or not k or k["dirty"] or k["key"] != s["key"]:
            changed += bool(s and not s["dirty"])
            continue
        write_json(entry_path(base, run["platform"], suite, s["key"], run["retried"]),
                   {"time": now(), "run": os.path.basename(path), "retried": run["retried"],
                    "duration": dict((r[0], r[2]) for r in run["rows"]).get(suite.split(".")[0], 0)})
    state_file = os.path.join(base, "state-%s.json" % run["platform"])
    if ok and run["full"] and not run["retried"] and not partial:
        state = read_json(state_file, {})
        state.update(last_full=now(), runs_since_full=0)
        write_json(state_file, state)
    trimmed = [x for x in run["suites"] if is_trimmed(x)]
    if ok and trimmed and not run["retried"] and all(x in run["passed"] for x in trimmed):
        state = read_json(state_file, {})
        state["last_trimmed"] = now()
        write_json(state_file, state)
    prune(base)
    for ext in (".json", ".plan", ".log"):
        try:
            os.remove(stem + ext)
        except OSError:
            pass
    # The table.
    rows = run["rows"]
    if failed:
        rows.append([failed, "FAILED", 0])
    width = max([len(r[0]) for r in rows] + [5])
    out = ["", "%-*s  %-34s %s" % (width, "suite", "result", "time")]
    for name, status, secs in rows:
        out.append("%-*s  %-34s %s" % (width, name, status, "-" if status.startswith("cached") else "%ss" % secs))
    out.append("%s run on %s; cached suites: %s" % ("full" if run["full"] else "cached", run["platform"],
                                                    ", ".join(run["cached"]) or "none"))
    if changed:
        out.append("%d passed suite(s) not recorded: their inputs changed during the run" % changed)
    return "\n".join(out) + "\n"


def cmd_plan(args):
    tools_file = args[args.index("--tools-file") + 1] if "--tools-file" in args else None
    path, message = plan(args[0], "--full" in args, "--retried" in args, tools_file)
    print(message, file=sys.stderr)
    print(path)
    return 0


def cmd_finish(args):
    failed = args[args.index("--failed") + 1] if "--failed" in args else ""
    sys.stdout.write(finish(args[0], "--ok" in args, "--partial" in args, failed))
    return 0


def cmd_full_due(args):
    reason = full_reason(store(), args[0])
    if reason:
        print(reason)
        return 0
    return 1


def cmd_check_projects(args):
    """Fails when DOTNET_REFS differs from the ProjectReference lines of the C# projects."""
    import re
    bad = 0
    for name, refs in DOTNET_REFS.items():
        path = os.path.join(W, "NvrView.%s" % name, "NvrView.%s.csproj" % name)
        with open(path) as f:
            found = sorted(re.findall(r'ProjectReference Include="\.\./NvrView\.([A-Za-z.]+)/', f.read()))
        if found != sorted(refs):
            print("%s references %s, but DOTNET_REFS says %s" % (name, found, sorted(refs)))
            bad = 1
    for d in os.listdir(W):
        if d.startswith("NvrView.") and d[len("NvrView."):] not in DOTNET_REFS:
            print("%s is not in DOTNET_REFS" % d)
            bad = 1
    return bad


def cmd_areas(args):
    """Prints the Windows app self-test areas, space separated."""
    print(" ".join(WINDOWS_AREAS))
    return 0


def main():
    if len(sys.argv) < 2 or (len(sys.argv) < 3 and sys.argv[1] != "check-projects"):
        print(__doc__, file=sys.stderr)
        return 2
    cmd, args = sys.argv[1], sys.argv[2:]
    return {"plan": cmd_plan, "finish": cmd_finish, "full-due": cmd_full_due, "areas": cmd_areas,
            "check-projects": cmd_check_projects}[cmd](args) or 0


if __name__ == "__main__":
    sys.exit(main())
