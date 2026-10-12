#!/usr/bin/env python3
"""Checks the Windows app self-test's area table (scripts/windows-selftest-areas.txt).

Every `// parity: <id>` step in apps/windows/NvrView.App/SelfTest*.cs must
belong to exactly one area: the table's longest matching pattern decides, and
two patterns of different areas with the same length that match one step is
an error. Also checked:

- every pattern matches a step (no stale entries);
- the area list is the same in the table, SelfTest.AreaNames (the app's
  --areas), and WINDOWS_AREAS in scripts/suite-cache.py;
- every area name used in a Runs(...) or RunsAny(...) call is a known area;
- the files that scripts/suite-cache.py lists as owned by an area exist and
  are owned by one area only.

Run with `python3 -I scripts/check-selftest-areas.py`; `--list` prints the
steps of each area.
"""

import importlib.util
import re
import sys

sys.dont_write_bytecode = True
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "apps/windows/NvrView.App"
TABLE = ROOT / "scripts/windows-selftest-areas.txt"
MARKER = re.compile(r"^\s*//+\s*parity:\s*(\S+)\s*$")


def load_cache():
    spec = importlib.util.spec_from_file_location("suite_cache", ROOT / "scripts/suite-cache.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def main():
    errors = []
    ids = {}
    sources = sorted(APP.glob("SelfTest*.cs"))
    for path in sources:
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            m = MARKER.match(line)
            if m:
                ids[m.group(1)] = f"{path.name}:{number}"

    table = []  # (area, pattern, line number)
    table_rel = TABLE.relative_to(ROOT)
    for number, raw in enumerate(TABLE.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 2:
            errors.append(f"{table_rel}:{number}: want `area  id` or `area  prefix*`")
            continue
        table.append((parts[0], parts[1], number))

    def matches(pattern, step):
        return step.startswith(pattern[:-1]) if pattern.endswith("*") else step == pattern

    areas = {}  # step -> area
    used = set()
    for step, where in sorted(ids.items()):
        best = []
        for area, pattern, number in table:
            if matches(pattern, step):
                best.append((len(pattern.rstrip("*")) + (0 if pattern.endswith("*") else 1), area, pattern, number))
        if not best:
            errors.append(f"{where}: step `{step}` belongs to no area (add it to {table_rel})")
            continue
        top = max(b[0] for b in best)
        winners = [b for b in best if b[0] == top]
        if len({b[1] for b in winners}) > 1:
            errors.append(f"{where}: step `{step}` matches patterns of several areas equally: "
                          + ", ".join(f"{b[1]} {b[2]}" for b in winners))
            continue
        areas[step] = winners[0][1]
        used.update((b[3]) for b in winners)
    for area, pattern, number in table:
        if not any(matches(pattern, step) for step in ids):
            errors.append(f"{table_rel}:{number}: `{pattern}` matches no step")

    table_areas = {a for a, _, _ in table}
    cache = load_cache()
    cache_areas = list(cache.WINDOWS_AREAS)
    cs = (APP / "SelfTest.cs").read_text(encoding="utf-8")
    m = re.search(r"AreaNames\s*=\s*\[([^\]]*)\]", cs)
    app_areas = re.findall(r'"([a-z]+)"', m.group(1)) if m else []
    if not m:
        errors.append("SelfTest.cs: AreaNames not found")
    if set(app_areas) != set(cache_areas) or len(app_areas) != len(cache_areas):
        errors.append(f"area lists differ: SelfTest.AreaNames {app_areas}, suite-cache.py {cache_areas}")
    if table_areas != set(cache_areas):
        errors.append(f"{table_rel}: areas {sorted(table_areas)} differ from suite-cache.py {sorted(cache_areas)}")

    for path in sources:
        for call in re.finditer(r"\bRuns(?:Any)?\(([^)]*)\)", path.read_text(encoding="utf-8")):
            for name in re.findall(r'"([^"]*)"', call.group(1)):
                if name not in cache_areas:
                    errors.append(f"{path.name}: unknown area `{name}` in {call.group(0)}")

    seen = {}
    for area, paths in cache.WINDOWS_OWNED.items():
        if area not in cache_areas or area == "startup":
            errors.append(f"suite-cache.py WINDOWS_OWNED: `{area}` is not an owning area")
        for p in paths:
            if not (ROOT / p).is_file():
                errors.append(f"suite-cache.py WINDOWS_OWNED: {p} does not exist")
            if p in seen:
                errors.append(f"suite-cache.py WINDOWS_OWNED: {p} is owned by {seen[p]} and {area}")
            seen[p] = area

    if errors:
        print("self-test area check failed:", file=sys.stderr)
        for e in errors:
            print("  " + e, file=sys.stderr)
        return 1
    if "--list" in sys.argv:
        for area in cache_areas:
            print(f"{area}: " + " ".join(sorted(s for s, a in areas.items() if a == area)))
    else:
        print(f"self-test areas ok: {len(ids)} steps in {len(cache_areas)} areas")
    return 0


if __name__ == "__main__":
    sys.exit(main())
