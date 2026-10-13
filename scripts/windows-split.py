#!/usr/bin/env python3
"""Decides whether one Windows check is worth splitting over two computers, and
which half runs where (scripts/windows-check.sh).

    windows-split.py plan --hosts PRIMARY SECOND --rust clippy,test,doctest|none
                          --suites a,b,c|none [--speeds "host=weight ..."]
                          [--timings FILE] [--min-seconds N]

The check has two halves that need nothing from each other: "rust" (clippy,
the tests and the doctests) and "rest" (the C# builds and tests, the video
self-test and the app self-test, the trimmed publish and the update test).
Prints one line:

    split HOST=rust HOST=rest      (either order) run both at the same time
    single REASON                  run everything on the primary host

The times of each half come from the step times of past Windows checks
(check-timings.jsonl, written by scripts/timing.py): for every step the half
would run, the median over the last runs that ran it. A computer's weight
(NVRVIEW_WINDOWS_SPEEDS="host=2 other=1": the slower one takes about twice as
long, so give it half the weight; default 1 each) scales those times. The
split happens when both halves have work, the smaller one is at least
--min-seconds (default NVRVIEW_SPLIT_MIN_SECONDS, else 30) and the longer
half's time on its computer is under 90 % of the whole check on the primary
computer. Without history the times are equal guesses, so the split happens
and the primary computer takes the rest half.
"""
import json
import os
import statistics
import subprocess
import sys

# Step-name prefixes (as printed by scripts/windows-worker.sh) of each half.
RUST_STEPS = {
    "clippy": ["clippy"],
    "test": ["tests ("],
    "doctest": ["doctests"],
}
# Steps of every suite of the other half; the builds are counted once.
REST_STEPS = {
    "csharp-smoke": ["build: C# smoke", "C# smoke test"],
    "tiles-tests": ["build: C# tile tests", "C# tile tests"],
    "video-selftest": ["build: C# video demo", "Windows video self-test"],
    "shell-tests": ["build: C# shell tests", "C# shell tests"],
    "app-debug": ["build: C# app", "Windows app plain start, first run", "Windows app self-test, areas"],
    "app-publish": ["Windows app, trimmed release build", "Windows app plain start, trimmed", "Windows app self-test, trimmed"],
    "app-update": ["Windows side-by-side update"],
}
REST_ALWAYS = ["build: release library and mock", "wait for the background builds"]
GUESS = 300.0


def timings_path():
    p = os.environ.get("NVRVIEW_TIMING_LOG")
    if p:
        return p
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], capture_output=True, check=True).stdout.decode().strip()
    return os.path.join(os.path.abspath(common), "check-timings.jsonl")


def load_runs(path, keep=30):
    runs = []
    try:
        with open(path) as f:
            for line in f:
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                if rec.get("kind") == "windows":
                    runs.append(rec.get("steps") or [])
    except OSError:
        pass
    return runs[-keep:]


def estimate(runs, prefixes):
    """Median seconds of the steps starting with any prefix, over the runs that ran one; None without history."""
    sums = []
    for steps in runs:
        total = 0
        seen = False
        for name, secs in steps:
            if any(name.startswith(p) for p in prefixes):
                total += secs
                seen = True
        if seen:
            sums.append(total)
    return statistics.median(sums) if sums else None


def group_estimate(runs, groups):
    """Sum of the per-step-prefix medians; (seconds, whether any history was found)."""
    total, known = 0.0, False
    for prefixes in groups:
        for p in prefixes:
            e = estimate(runs, [p])
            if e is not None:
                total += e
                known = True
    return total, known


def parse_speeds(text):
    out = {}
    for item in (text or "").split():
        host, _, w = item.partition("=")
        try:
            out[host] = float(w)
        except ValueError:
            pass
    return out


def main(argv):
    if not argv or argv[0] != "plan":
        print(__doc__, file=sys.stderr)
        return 2
    opts = {"--hosts": None, "--rust": "none", "--suites": "none", "--speeds": os.environ.get("NVRVIEW_WINDOWS_SPEEDS", ""),
            "--timings": None, "--min-seconds": os.environ.get("NVRVIEW_SPLIT_MIN_SECONDS", "30")}
    args = argv[1:]
    i = 0
    hosts = []
    while i < len(args):
        a = args[i]
        if a == "--hosts":
            hosts = args[i + 1:i + 3]
            i += 3
        elif a in opts:
            opts[a] = args[i + 1]
            i += 2
        else:
            print("unknown option " + a, file=sys.stderr)
            return 2
    if len(hosts) != 2:
        print("--hosts needs the primary and the second host", file=sys.stderr)
        return 2
    rust = [r for r in opts["--rust"].split(",") if r in RUST_STEPS]
    suites = [s for s in opts["--suites"].split(",") if s and s != "none"]
    if not rust:
        print("single nothing to run in the Rust half")
        return 0
    if not suites:
        print("single nothing to run in the other half")
        return 0
    runs = load_runs(opts["--timings"] or timings_path())
    rust_t, rust_known = group_estimate(runs, [RUST_STEPS[r] for r in rust])
    groups = [REST_ALWAYS]
    for s in suites:
        key = s.split(".")[0] if s.startswith("app-debug") else s
        if key in REST_STEPS:
            groups.append(REST_STEPS[key])
    # The same prefix in two suites (app-debug areas) counts once.
    uniq = []
    for g in groups:
        g = [p for p in g if not any(p in u for u in uniq)]
        uniq.append(g)
    rest_t, rest_known = group_estimate(runs, uniq)
    if not rust_known:
        rust_t = GUESS
    if not rest_known:
        rest_t = GUESS
    speeds = parse_speeds(opts["--speeds"])
    sp = {h: speeds.get(h, 1.0) or 1.0 for h in hosts}
    prim, second = hosts
    single = (rust_t + rest_t) / sp[prim]
    options = [
        (max(rust_t / sp[prim], rest_t / sp[second]), prim, second),  # primary takes rust
        (max(rest_t / sp[prim], rust_t / sp[second]), second, prim),  # primary takes rest
    ]
    # Ties go to the primary computer taking the rest half (the app part, which
    # a later --package for the same worker reuses).
    best = min(options[::-1], key=lambda o: o[0])
    rust_host = best[1]
    rest_host = best[2]
    if min(rust_t, rest_t) < float(opts["--min-seconds"]):
        print("single the smaller half is about %ds, not worth a second computer" % min(rust_t, rest_t))
        return 0
    if best[0] >= 0.9 * single:
        print("single splitting would take about %ds against %ds on %s" % (best[0], single, prim))
        return 0
    print("split %s=rust %s=rest" % (rust_host, rest_host))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
