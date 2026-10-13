#!/usr/bin/env python3
"""Every file under a crate's tests/ folder must be a module of that crate's
one test executable (tests/main.rs), or it is never built or run.

Crates with `autotests = false` in Cargo.toml declare a single [[test]] target
(tests/main.rs) so the dependency graph is linked once, not once per file.
This lists the tests/*.rs files that main.rs does not name with `mod`.
Run with `python3 -I scripts/check-test-modules.py`.
"""
import glob
import os
import re
import sys

bad = []
for cargo in sorted(glob.glob("crates/*/Cargo.toml") + glob.glob("tools/*/Cargo.toml")):
    if "autotests = false" not in open(cargo).read():
        continue
    tests = os.path.join(os.path.dirname(cargo), "tests")
    main = os.path.join(tests, "main.rs")
    if not os.path.exists(main):
        bad.append(f"{main}: missing")
        continue
    mods = set(re.findall(r"^mod (\w+);", open(main).read(), re.M))
    for f in sorted(glob.glob(os.path.join(tests, "*.rs"))):
        stem = os.path.basename(f)[:-3]
        if stem != "main" and stem not in mods:
            bad.append(f"{f}: not a module of tests/main.rs (add it there)")
for b in bad:
    print(b, file=sys.stderr)
sys.exit(1 if bad else 0)
