#!/usr/bin/env python3
"""Checks the app self-tests' area tables, for Windows and for the Mac.

Windows: scripts/windows-selftest-areas.txt against the `// parity: <id>`
steps in apps/windows/NvrView.App/SelfTest*.cs. Mac:
scripts/macos-selftest-areas.txt against the steps in
apps/macos/Sources/NVRView/SelfTest*.swift.

Every step must belong to exactly one area: the table's longest matching
pattern decides, and two patterns of different areas with the same length
that match one step is an error. Also checked, per platform:

- every pattern matches a step (no stale entries);
- the area list is the same in the table, the app's own list (Windows
  SelfTest.AreaNames, Mac SelfTest.areaNames: what --areas accepts), and
  WINDOWS_AREAS or MAC_AREAS in scripts/suite-cache.py;
- every area name used in a Runs(...)/RunsAny(...) call (Windows) or a
  runs(...) call (Mac) is a known area;
- the files that scripts/suite-cache.py lists as owned by an area
  (WINDOWS_OWNED, MAC_OWNED) exist and are owned by one area only.

Run with `python3 -I scripts/check-selftest-areas.py`; `--list` prints the
steps of each area.
"""

import importlib.util
import re
import sys

sys.dont_write_bytecode = True
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MARKER = re.compile(r"^\s*//+\s*parity:\s*(\S+)\s*$")

PLATFORMS = {
    "windows": {
        "sources": lambda: sorted((ROOT / "apps/windows/NvrView.App").glob("SelfTest*.cs")),
        "table": ROOT / "scripts/windows-selftest-areas.txt",
        "names_file": ROOT / "apps/windows/NvrView.App/SelfTest.cs",
        "names": re.compile(r"AreaNames\s*=\s*\[([^\]]*)\]"),
        "names_label": "SelfTest.AreaNames",
        "calls": re.compile(r"\bRuns(?:Any)?\(([^)]*)\)"),
        "areas": "WINDOWS_AREAS",
        "owned": "WINDOWS_OWNED",
    },
    "mac": {
        "sources": lambda: sorted((ROOT / "apps/macos/Sources/NVRView").glob("SelfTest*.swift")),
        "table": ROOT / "scripts/macos-selftest-areas.txt",
        "names_file": ROOT / "apps/macos/Sources/NVRView/SelfTest.swift",
        "names": re.compile(r"static let areaNames\s*=\s*\[([^\]]*)\]"),
        "names_label": "SelfTest.areaNames",
        "calls": re.compile(r"\bruns\(([^)]*)\)"),
        "areas": "MAC_AREAS",
        "owned": "MAC_OWNED",
    },
}


def load_cache():
    spec = importlib.util.spec_from_file_location("suite_cache", ROOT / "scripts/suite-cache.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def check(platform, cfg, cache, errors):
    """Checks one platform; returns (areas in order, step -> area, step count)."""
    def err(text):
        errors.append(f"{platform}: {text}")

    ids = {}
    sources = cfg["sources"]()
    for path in sources:
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            m = MARKER.match(line)
            if m:
                ids[m.group(1)] = f"{path.name}:{number}"

    table = []  # (area, pattern, line number)
    table_rel = cfg["table"].relative_to(ROOT)
    for number, raw in enumerate(cfg["table"].read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 2:
            err(f"{table_rel}:{number}: want `area  id` or `area  prefix*`")
            continue
        table.append((parts[0], parts[1], number))

    def matches(pattern, step):
        return step.startswith(pattern[:-1]) if pattern.endswith("*") else step == pattern

    areas = {}  # step -> area
    for step, where in sorted(ids.items()):
        best = []
        for area, pattern, number in table:
            if matches(pattern, step):
                best.append((len(pattern.rstrip("*")) + (0 if pattern.endswith("*") else 1), area, pattern, number))
        if not best:
            err(f"{where}: step `{step}` belongs to no area (add it to {table_rel})")
            continue
        top = max(b[0] for b in best)
        winners = [b for b in best if b[0] == top]
        if len({b[1] for b in winners}) > 1:
            err(f"{where}: step `{step}` matches patterns of several areas equally: "
                + ", ".join(f"{b[1]} {b[2]}" for b in winners))
            continue
        areas[step] = winners[0][1]
    for area, pattern, number in table:
        if not any(matches(pattern, step) for step in ids):
            err(f"{table_rel}:{number}: `{pattern}` matches no step")

    table_areas = {a for a, _, _ in table}
    cache_areas = list(getattr(cache, cfg["areas"]))
    text = cfg["names_file"].read_text(encoding="utf-8")
    m = cfg["names"].search(text)
    app_areas = re.findall(r'"([a-z]+)"', m.group(1)) if m else []
    if not m:
        err(f"{cfg['names_file'].name}: {cfg['names_label']} not found")
    if set(app_areas) != set(cache_areas) or len(app_areas) != len(cache_areas):
        err(f"area lists differ: {cfg['names_label']} {app_areas}, suite-cache.py {cache_areas}")
    if table_areas != set(cache_areas):
        err(f"{table_rel}: areas {sorted(table_areas)} differ from suite-cache.py {sorted(cache_areas)}")

    for path in sources:
        for call in cfg["calls"].finditer(path.read_text(encoding="utf-8")):
            for name in re.findall(r'"([^"]*)"', call.group(1)):
                if name not in cache_areas:
                    err(f"{path.name}: unknown area `{name}` in {call.group(0)}")

    seen = {}
    for area, paths in getattr(cache, cfg["owned"]).items():
        if area not in cache_areas or area == "startup":
            err(f"suite-cache.py {cfg['owned']}: `{area}` is not an owning area")
        for p in paths:
            if not (ROOT / p).exists():
                err(f"suite-cache.py {cfg['owned']}: {p} does not exist")
            if p in seen:
                err(f"suite-cache.py {cfg['owned']}: {p} is owned by {seen[p]} and {area}")
            seen[p] = area
    return cache_areas, areas, len(ids)


def main():
    errors = []
    cache = load_cache()
    results = {p: check(p, cfg, cache, errors) for p, cfg in PLATFORMS.items()}
    if errors:
        print("self-test area check failed:", file=sys.stderr)
        for e in errors:
            print("  " + e, file=sys.stderr)
        return 1
    for platform, (area_list, areas, count) in results.items():
        if "--list" in sys.argv:
            print(f"== {platform}")
            for area in area_list:
                print(f"{area}: " + " ".join(sorted(s for s, a in areas.items() if a == area)))
        else:
            print(f"self-test areas ok ({platform}): {count} steps in {len(area_list)} areas")
    return 0


if __name__ == "__main__":
    sys.exit(main())
