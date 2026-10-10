#!/usr/bin/env bash
# Runs on a Windows computer under Git Bash, in a copy of the tree that
# scripts/windows-check.sh unpacked: the Windows part of CI (clippy, tests,
# the C interface library, the C# smoke test), the video self-test on the
# real GPU, the C# shell tests, and a build of the Windows app. Stops at the
# first failure.
#
#   NVRVIEW_APP_SELFTEST=1   also run the app's tests in the logged-in user's
#                            desktop session (scripts/windows-app.sh): the
#                            plain start on an empty data directory, then the
#                            self-test against mock-nvr
#   NVRVIEW_WINDOWS_ONLY=csharp  skips clippy and the Rust tests (for changes
#                            under apps/windows only)
#   NVRVIEW_WINDOWS_ONLY=app only the library and the app (also skips the
#                            C# smoke test and the video self-test)
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

step "toolchain"
rustc --version
dotnet --version

only="${NVRVIEW_WINDOWS_ONLY:-}"
if [ -z "$only" ]; then
  step "clippy"
  cargo clippy --workspace --all-targets -- -D warnings

  step "tests"
  cargo test --workspace
fi

step "C interface library (release)"
cargo build --release -p viewer-ffi
ls -l target/release/nvrview.dll target/release/nvrview.dll.lib

if [ "$only" != app ]; then
  step "C# smoke test"
  scripts/csharp-smoke.sh

  step "Windows video self-test (GPU, then WARP)"
  scripts/csharp-video-selftest.sh
fi

step "C# shell tests (layouts, leases, sidebar reveal, window restore)"
dotnet build apps/windows/NvrView.Shell.Tests -c Release --artifacts-path target/dotnet -v quiet -clp:ErrorsOnly -nologo
dotnet target/dotnet/bin/NvrView.Shell.Tests/release/NvrView.Shell.Tests.dll

step "Windows app (build)"
scripts/windows-app.sh build

if [ "${NVRVIEW_APP_SELFTEST:-0}" = 1 ]; then
  step "Windows app plain start, first run (interactive session)"
  scripts/windows-app.sh startup

  step "Windows app self-test (interactive session)"
  scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test
fi

printf '\nWindows checks passed.\n'
