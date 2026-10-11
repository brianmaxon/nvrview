#!/usr/bin/env python3
"""Compares the two apps' self-tests step by step (docs/PARITY.md).

Every step of a self-test carries a stable ID in a comment directly above the
code it describes:

    // parity: fisheye-circle

IDs are short kebab-case strings, shared by the steps that check the same
thing on the two apps. This script collects the IDs from both self-tests and
fails when an ID exists in one app only, unless scripts/parity-allow.txt lists
it. Run with `python3 -I scripts/check-parity.py`; `--list` prints the matrix
instead of checking.

The allowlist has one entry per line, `id  platform  reason`, where platform
is the app that HAS the step (`macos` or `windows`) and the other lacks it.
The reason starts with `n/a:` (the step makes no sense on the other platform)
or `gap:` (the other app should get the check or the feature). An entry whose
step exists on both apps, or on neither, is stale and fails too, so the list
cannot rot. Lines starting with # and blank lines are ignored.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = {
    "macos": [ROOT / "apps/macos/Sources/NVRView/SelfTest.swift"],
    "windows": [ROOT / "apps/windows/NvrView.App/SelfTest.cs"],
}
ALLOW = ROOT / "scripts/parity-allow.txt"
MARKER = re.compile(r"^\s*//+\s*parity:\s*(\S+)\s*$")
ID_FORM = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
PLATFORMS = tuple(SOURCES)


def collect(errors):
    """Returns {platform: {id: "relative/path:line"}}."""
    found = {}
    for platform, paths in SOURCES.items():
        ids = found[platform] = {}
        for path in paths:
            rel = path.relative_to(ROOT)
            if not path.is_file():
                errors.append(f"{rel}: self-test source not found")
                continue
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                if "parity:" not in line:
                    continue
                m = MARKER.match(line)
                if not m:
                    errors.append(f"{rel}:{number}: malformed marker (want `// parity: some-id`)")
                    continue
                step = m.group(1)
                if not ID_FORM.match(step):
                    errors.append(f"{rel}:{number}: `{step}` is not a kebab-case ID")
                elif step in ids:
                    errors.append(f"{rel}:{number}: `{step}` is already used at {ids[step]}")
                else:
                    ids[step] = f"{rel}:{number}"
    return found


def allowlist(errors):
    """Returns {id: (platform, reason)}."""
    entries = {}
    if not ALLOW.is_file():
        return entries
    rel = ALLOW.relative_to(ROOT)
    for number, raw in enumerate(ALLOW.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 3 or parts[1] not in PLATFORMS:
            errors.append(f"{rel}:{number}: want `id  macos|windows  reason`")
        elif parts[0] in entries:
            errors.append(f"{rel}:{number}: `{parts[0]}` is listed twice")
        else:
            entries[parts[0]] = (parts[1], parts[2])
    return entries


def other(platform):
    return PLATFORMS[1] if platform == PLATFORMS[0] else PLATFORMS[0]


def main(argv):
    if any(a not in ("--list", "-h", "--help") for a in argv):
        print(__doc__)
        return 2
    if "-h" in argv or "--help" in argv:
        print(__doc__)
        return 0

    errors = []
    found = collect(errors)
    allowed = allowlist(errors)
    everything = sorted(set().union(*(set(ids) for ids in found.values())))

    if "--list" in argv:
        width = max([len(i) for i in everything] + [2])
        print(f"{'id':<{width}}  macos  windows  allowlist")
        for step in everything:
            mac, win = step in found["macos"], step in found["windows"]
            note = ""
            if step in allowed:
                note = f"{allowed[step][0]} only: {allowed[step][1]}"
            elif mac != win:
                note = "NOT ALLOWED"
            print(f"{step:<{width}}  {'yes' if mac else '-':<5}  {'yes' if win else '-':<7}  {note}")
        both = sum(1 for s in everything if all(s in found[p] for p in PLATFORMS))
        print(f"\n{len(everything)} steps, {both} on both apps, "
              f"{len(everything) - both} on one ({len(allowed)} allowlisted)")
        return 1 if errors else 0

    for step in everything:
        have = [p for p in PLATFORMS if step in found[p]]
        if len(have) == 2:
            if step in allowed:
                errors.append(f"parity-allow.txt: `{step}` is on both apps now; remove its entry")
        elif step not in allowed:
            lacking = other(have[0])
            errors.append(
                f"`{step}` is in the {have[0]} self-test only ({found[have[0]][step]}); "
                f"add it to the {lacking} self-test or list it in scripts/parity-allow.txt")
        elif allowed[step][0] != have[0]:
            errors.append(f"parity-allow.txt: `{step}` says {allowed[step][0]} but only {have[0]} has it")
    for step in allowed:
        if step not in everything:
            errors.append(f"parity-allow.txt: `{step}` is in no self-test; remove its entry")

    if errors:
        print("self-test parity check failed:")
        for e in errors:
            print(f"  {e}")
        return 1
    both = sum(1 for s in everything if all(s in found[p] for p in PLATFORMS))
    print(f"self-test parity: {len(everything)} steps, {both} on both apps, {len(allowed)} allowlisted")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
