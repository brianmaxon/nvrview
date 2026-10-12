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
  plan PLATFORM [--full] [--retried] [--tools-file F]   start a run; prints RUN
  want RUN SUITE        exit 0: run it; exit 1: cached (prints the cached line)
  crates RUN FAMILY [--list]
                        Rust family (clippy, test, doctest, wincheck,
                        linuxcheck): prints cargo arguments for the crates to
                        run (--workspace, -p a -p b, or nothing); with --list
                        all, none, or a,b
  pass RUN SECS NAME... record passes (a suite, a family, or a plain step name
                        that is only shown in the table)
  finish RUN [--ok [--partial]] [--failed LABEL]
                        records the passes, prints the table, ends the run;
                        a full run counts as the platform's full pass only
                        with --ok and without --partial (steps were skipped)
  full-due PLATFORM     exit 0 when the next run on PLATFORM would be full

For tests: NVRVIEW_CACHE_NOW (epoch seconds), NVRVIEW_CACHE_METADATA (a
`cargo metadata` JSON file), NVRVIEW_CACHE_TOOLS (tool versions, one string).
"""

import hashlib
import json
import os
import subprocess
import sys
import time

HARNESS = "suite-cache 1"  # bump to invalidate every entry
KEEP_DAYS = 14
KEEP_MAX = 20000
FAMILIES = ("clippy", "test", "doctest", "wincheck", "linuxcheck")
RUST_SHARED = [
    "Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml",
    ".cargo/config.toml", ".config/nextest.toml", "clippy.toml", ".clippy.toml",
    "rustfmt.toml", ".rustfmt.toml", "fixtures/synthetic/",
]
FIXTURES = ["fixtures/synthetic/"]

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
        "csharp-smoke": (["apps/windows/", "scripts/csharp-smoke.sh", "VERSION"] + FIXTURES,
                         ["viewer-ffi", "mock-nvr"], ["rust"]),
        "video-selftest": (["apps/windows/", "scripts/csharp-video-selftest.sh", "VERSION"] + FIXTURES,
                           ["viewer-ffi", "mock-nvr"], ["rust"]),
        # The shell tests are managed code only (no native library).
        "shell-tests": (["apps/windows/", "VERSION"] + FIXTURES, [], ["rust"]),
        "app-debug": (["apps/windows/", "scripts/windows-app.sh", "VERSION"] + FIXTURES,
                      ["viewer-ffi", "mock-nvr"], ["rust"]),
        "app-publish": (["apps/windows/", "scripts/windows-app.sh", "VERSION"] + FIXTURES,
                        ["viewer-ffi", "mock-nvr"], ["rust"]),
        "app-update": (["apps/windows/", "scripts/windows-app.sh", "VERSION"] + FIXTURES,
                       ["viewer-ffi", "mock-nvr", "sign-update"], ["rust"]),
    },
}
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
            out = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
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

    def key(self, suite, paths, tool_kinds):
        """(key, dirty input or None)."""
        prefixes = tuple(sorted(set(p for p in paths if p.endswith("/") or p == "")))
        exact = set(p for p in paths if not (p.endswith("/") or p == ""))
        h = hashlib.sha256()
        h.update(("%s\n%s\n%s\n" % (HARNESS, self.platform, suite)).encode())
        for k in sorted(set(tool_kinds)):
            h.update(("tool %s %s\n" % (k, self.tool(k))).encode())
        for path, mode, blob in self.files:
            if path in exact or path.startswith(prefixes):
                h.update(("%s %s %s\n" % (mode, blob, path)).encode())
        dirty = sorted(p for p in self.dirty if p in exact or p.startswith(prefixes))
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
        for name, (paths, roots, kinds) in SUITES[self.platform].items():
            roots = [r for r in roots if r == "*" or r in self.crates]
            all_paths = list(paths) + (self.crate_inputs(roots) if roots else [])
            k, d = self.key(name, all_paths, kinds)
            out[name] = {"key": k, "dirty": d}
        return out


# --- state and entries -------------------------------------------------------

def entry_path(base, platform, suite, key, retried=False):
    return os.path.join(base, "entries", platform, suite, key + (".retried.json" if retried else ".json"))


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

def cmd_plan(args):
    platform = args[0]
    full = "--full" in args
    retried = "--retried" in args
    tools = {}
    if "--tools-file" in args:
        with open(args[args.index("--tools-file") + 1]) as f:
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
        "started": now(), "tools": tree.tools, "suites": tree.suite_keys(), "selected": {},
        "passed": {}, "rows": [], "cached": [], "crates": {c: v["lib"] for c, v in tree.crates.items()},
    }
    run["tools"] = tree.tools  # filled while computing the keys
    state_file = os.path.join(base, "state-%s.json" % platform)
    state = read_json(state_file, {})
    if reason:
        print("full run: %s" % reason, file=sys.stderr)
    else:
        state["runs_since_full"] = state.get("runs_since_full", 0) + 1
        write_json(state_file, state)
        print("cached run: suites with a clean pass in the last %gh are skipped (last full pass %s ago; NVRVIEW_FULL_CHECK=1 runs everything)"
              % (hours("NVRVIEW_CACHE_MAX_AGE_H", 72), age(now() - state.get("last_full", now()))), file=sys.stderr)
    path = os.path.join(base, "runs", "%s-%d-%d.json" % (platform, now(), os.getpid()))
    write_json(path, run)
    print(path)


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


def cmd_want(args):
    path, suite = args
    run = read_json(path)
    base = store()
    e = cached_entry(base, run, suite)
    if e is None:
        return 0
    print("cached pass %s (%s, key %s)" % (suite, age(now() - e["time"]), run["suites"][suite]["key"][:10]))
    run["rows"].append([suite, "cached", 0])
    run["cached"].append(suite)
    write_json(path, run)
    return 1


def cmd_crates(args):
    path, fam = args[0], args[1]
    as_list = "--list" in args
    run = read_json(path)
    base = store()
    names = sorted(s.split(".", 1)[1] for s in run["suites"] if s.startswith(fam + "."))
    todo, cached = [], []
    for c in names:
        e = cached_entry(base, run, "%s.%s" % (fam, c))
        (todo if e is None else cached).append((c, e))
    for c, e in cached:
        print("cached pass %s.%s (%s, key %s)" % (fam, c, age(now() - e["time"]),
                                                 run["suites"]["%s.%s" % (fam, c)]["key"][:10]), file=sys.stderr)
    run["selected"][fam] = [c for c, _ in todo]
    run["rows"].append([fam, "cached %d of %d crates" % (len(cached), len(names)), 0])
    write_json(path, run)
    if cached:
        run["cached"].append("%s (%d of %d crates)" % (fam, len(cached), len(names)))
        write_json(path, run)
    # Doctest suites exist for library crates only, which is what
    # `cargo test --doc --workspace` covers.
    all_crates = len(todo) == len(names)
    if as_list:
        print("all" if todo and all_crates else ",".join(c for c, _ in todo) or "none")
    elif todo:
        print("--workspace" if all_crates else " ".join("-p %s" % c for c, _ in todo))
    return 0


def cmd_pass(args):
    path, secs, names = args[0], int(args[1]), args[2:]
    run = read_json(path)
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
    write_json(path, run)
    return 0


def cmd_finish(args):
    path = args[0]
    ok = "--ok" in args
    failed = args[args.index("--failed") + 1] if "--failed" in args else ""
    run = read_json(path)
    if run is None:
        return 0
    base = store()
    # Record only suites whose inputs are what they were at the start.
    tree = Tree(run["platform"], run["tools"])
    keys = tree.suite_keys() if run["passed"] else {}
    recorded = changed = 0
    for suite in run["passed"]:
        s = run["suites"].get(suite)
        k = keys.get(suite)
        if not s or s["dirty"] or not k or k["dirty"] or k["key"] != s["key"]:
            changed += bool(s and not s["dirty"])
            continue
        write_json(entry_path(base, run["platform"], suite, s["key"], run["retried"]),
                   {"time": now(), "run": os.path.basename(path), "retried": run["retried"],
                    "duration": dict((r[0], r[2]) for r in run["rows"]).get(suite.split(".")[0], 0)})
        recorded += 1
    state_file = os.path.join(base, "state-%s.json" % run["platform"])
    if ok and run["full"] and not run["retried"] and "--partial" not in args:
        state = read_json(state_file, {})
        state.update(last_full=now(), runs_since_full=0)
        write_json(state_file, state)
    prune(base)
    try:
        os.remove(path)
    except OSError:
        pass
    # The table.
    rows = run["rows"]
    if failed:
        rows.append([failed, "FAILED", 0])
    width = max([len(r[0]) for r in rows] + [5])
    print("\n%-*s  %-34s %s" % (width, "suite", "result", "time"))
    for name, status, secs in rows:
        print("%-*s  %-34s %s" % (width, name, status, "-" if status.startswith("cached") else "%ss" % secs))
    print("%s run on %s; cached suites: %s" % ("full" if run["full"] else "cached", run["platform"],
                                                ", ".join(run["cached"]) or "none"))
    if changed:
        print("%d passed suite(s) not recorded: their inputs changed during the run" % changed)
    return 0


def cmd_full_due(args):
    reason = full_reason(store(), args[0])
    if reason:
        print(reason)
        return 0
    return 1


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    cmd, args = sys.argv[1], sys.argv[2:]
    return {"plan": cmd_plan, "want": cmd_want, "crates": cmd_crates, "pass": cmd_pass,
            "finish": cmd_finish, "full-due": cmd_full_due}[cmd](args) or 0


if __name__ == "__main__":
    sys.exit(main())
