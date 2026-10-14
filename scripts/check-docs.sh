#!/bin/sh
# The checks that read markdown files, on the working tree. They are what a
# change to *.md files (and VERSION) still has to pass when the stamps of a
# checked tree are carried over to it (scripts/stamps-lib.sh,
# carry_version_stamps; docs/DEVELOPMENT.md, "Docs-only changes"):
#
#   sanitization of the whole tree (docs/SANITIZATION.md)
#   self-test parity (docs/PARITY.md) and the self-test area tables
#   the test-module check
#   the secret scan
#
# Keep this list in step with every check that reads a *.md file. Stops at the
# first failure. scripts/check.sh runs the same checks on its docs-only fast
# path.
set -eu
cd "$(git rev-parse --show-toplevel)"
echo "== markdown checks: sanitization, self-test parity, test modules, self-test areas, secret scan"
python3 -I scripts/check-sanitized.py --all
python3 -I scripts/check-parity.py
python3 -I scripts/check-test-modules.py
python3 -I scripts/check-selftest-areas.py
if [ "${NVRVIEW_SECRET_SCAN:-1}" = "0" ]; then
  echo "secret scan skipped (NVRVIEW_SECRET_SCAN=0)"
else
  rc=0
  scripts/secret-scan.sh || rc=$?
  # 3: no scanner available; nothing was scanned (check.sh reports the same).
  if [ "$rc" != 0 ] && [ "$rc" != 3 ]; then exit "$rc"; fi
fi
echo "markdown checks passed"
