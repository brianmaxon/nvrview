#!/usr/bin/env python3
"""Names a failed check and keeps a record of the flaky ones.

  flakes.py explain LOG            prints which suite or area failed (the last
                                   "== " step header in the log) and the failing
                                   step's lines (the self-test's problem lines
                                   before "selftest: FAIL", else the lines that
                                   mention a failure)
  flakes.py record [--tree T] [--platform P] LOG
                                   appends one JSON line for a flaky failure to
                                   <git common dir>/flakes.jsonl (last 500 lines)
  flakes.py summary [N]            one line: the N most frequent flaky steps

A line looks like {"time": EPOCH, "date": "YYYY-MM-DD", "tree": "...",
"platform": "windows", "suite": "...", "step": "...", "line": "..."}.
NVRVIEW_FLAKE_LOG overrides the file, NVRVIEW_FLAKE_NOW the clock and
NVRVIEW_FLAKE_KEEP the line limit (for the tests).
"""

import collections
import json
import os
import re
import subprocess
import sys
import time

KEEP = 500
FAILURE = re.compile(r"FAIL|panicked|^error|\berror\[|did not finish|microphone|software.*h\.?264", re.I)


def read_lines(path):
    try:
        with open(path, errors="replace") as f:
            return [l.rstrip("\r\n").replace("\r", "") for l in f]
    except OSError:
        return []


def analyze(path):
    """Returns (suite, step lines) of a failed check log."""
    lines = read_lines(path)
    suite = ""
    for l in lines:
        if l.startswith("== "):
            suite = l[3:].strip()
    steps = []
    verdicts = [i for i, l in enumerate(lines) if l.strip() == "selftest: FAIL"]
    if verdicts:
        i = verdicts[-1] - 1
        while i >= 0 and lines[i].startswith("  ") and lines[i].strip():
            steps.insert(0, lines[i].strip())
            i -= 1
    if not steps:
        steps = [l.strip() for l in lines if l.strip() and not l.startswith("== ") and FAILURE.search(l)][-4:]
    if not steps:
        steps = [l.strip() for l in lines if l.strip()][-2:]
    return suite, steps[:6]


def step_name(line):
    """The name of a failing step: the text before the first colon."""
    return line.split(":", 1)[0].strip()[:80]


def cmd_explain(args):
    suite, steps = analyze(args[0])
    print("failed in: %s" % (suite or "(no step header in the log)"))
    for s in steps:
        print("failing step: %s" % s[:300])
    return 0


def log_path():
    p = os.environ.get("NVRVIEW_FLAKE_LOG")
    if p:
        return p
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], capture_output=True, check=True).stdout.decode().strip()
    return os.path.join(os.path.abspath(common), "flakes.jsonl")


def cmd_record(args):
    tree, platform = "", ""
    while args[:1] and args[0].startswith("--"):
        if args[0] == "--tree":
            tree = args[1]
        elif args[0] == "--platform":
            platform = args[1]
        args = args[2:]
    suite, steps = analyze(args[0])
    now = int(os.environ.get("NVRVIEW_FLAKE_NOW") or time.time())
    first = steps[0] if steps else ""
    rec = {"time": now, "date": time.strftime("%Y-%m-%d", time.gmtime(now)), "tree": tree, "platform": platform,
           "suite": suite, "step": step_name(first), "line": first[:300]}
    path = log_path()
    lines = [l for l in read_lines(path) if l.strip()]
    lines.append(json.dumps(rec, separators=(",", ":")))
    lines = lines[-int(os.environ.get("NVRVIEW_FLAKE_KEEP") or KEEP):]
    tmp = "%s.%d.tmp" % (path, os.getpid())
    with open(tmp, "w") as f:
        f.write("\n".join(lines) + "\n")
    os.replace(tmp, path)
    return 0


def cmd_summary(args):
    top = int(args[0]) if args else 10
    count, last = collections.Counter(), {}
    total = 0
    for l in read_lines(log_path()):
        try:
            r = json.loads(l)
        except ValueError:
            continue
        total += 1
        key = r.get("step") or r.get("suite") or "(unknown)"
        count[key] += 1
        last[key] = r.get("date", "")
    if not total:
        print("no flaky failures recorded")
        return 0
    print("%d flaky failure(s) recorded; most frequent: %s" % (
        total, "; ".join("%s x%d (last %s)" % (k, n, last[k]) for k, n in count.most_common(top))))
    return 0


def main():
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "explain" and args:
        return cmd_explain(args)
    if cmd == "record" and args:
        return cmd_record(args)
    if cmd == "summary":
        return cmd_summary(args)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
