#!/usr/bin/env python3
"""Per-step wall times of the checks: the end table and the timing log.

The check scripts print "   (STEP: Ns)" after every step (scripts/check.sh,
scripts/windows-worker.sh, scripts/windows-check.sh). This reads those lines
from one or more logs.

  timing.py table LOG...       the steps in order with their seconds, then the
                               slowest three
  timing.py slowest LOG...     only the total of the steps and the slowest three
  timing.py log KIND TOTAL_SECONDS [--tree T] LOG...
                               appends one JSON line to <git common dir>/
                               check-timings.jsonl, keeping the last 500 lines

A line looks like {"time": EPOCH, "kind": "mac", "total": 123, "tree": "...",
"steps": [["clippy", 12], ...]}. NVRVIEW_TIMING_LOG overrides the file and
NVRVIEW_TIMING_NOW the clock and NVRVIEW_TIMING_KEEP the line limit (for the tests).
"""

import json
import os
import re
import subprocess
import sys
import time

KEEP = 500
STEP = re.compile(r"^\s{3}\((.*): (\d+)s\)\s*$")


def steps_of(paths):
    out = []
    for p in paths:
        try:
            with open(p, errors="replace") as f:
                for line in f:
                    m = STEP.match(line.replace("\r", ""))
                    if m:
                        out.append((m.group(1), int(m.group(2))))
        except OSError:
            pass
    return out


def slowest_line(steps):
    top = sorted(steps, key=lambda s: -s[1])[:3]
    return "slowest: " + "; ".join("%s %ds" % (n[:60], s) for n, s in top) if top else "slowest: (no step times)"


def cmd_table(paths, only_slowest=False):
    steps = steps_of(paths)
    if not only_slowest:
        print("\nstep times")
        width = min(70, max([len(n) for n, _ in steps] + [4]))
        for n, s in steps:
            print("  %-*s %5ds" % (width, n[:70], s))
    print("%d steps, %ds of step time; %s" % (len(steps), sum(s for _, s in steps), slowest_line(steps)))
    return 0


def log_path():
    p = os.environ.get("NVRVIEW_TIMING_LOG")
    if p:
        return p
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], capture_output=True, check=True).stdout.decode().strip()
    return os.path.join(os.path.abspath(common), "check-timings.jsonl")


def cmd_log(args):
    kind, total = args[0], int(args[1])
    rest = args[2:]
    tree = ""
    if rest[:1] == ["--tree"]:
        tree, rest = rest[1], rest[2:]
    now = int(os.environ.get("NVRVIEW_TIMING_NOW") or time.time())
    rec = {"time": now, "kind": kind, "total": total, "tree": tree, "steps": steps_of(rest)}
    path = log_path()
    try:
        with open(path) as f:
            lines = [l for l in f.read().splitlines() if l.strip()]
    except OSError:
        lines = []
    lines.append(json.dumps(rec, separators=(",", ":")))
    lines = lines[-int(os.environ.get("NVRVIEW_TIMING_KEEP") or KEEP):]
    tmp = "%s.%d.tmp" % (path, os.getpid())
    with open(tmp, "w") as f:
        f.write("\n".join(lines) + "\n")
    os.replace(tmp, path)
    return 0


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "table":
        return cmd_table(args)
    if cmd == "slowest":
        return cmd_table(args, True)
    if cmd == "log":
        return cmd_log(args)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
