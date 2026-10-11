#!/usr/bin/env bash
# Runs on a Windows computer under Git Bash, in a copy of the tree that
# scripts/windows-check.sh unpacked: the Windows part of CI (clippy, tests,
# the C interface library, the C# smoke test), the video self-test on the
# real GPU, the C# shell tests, and a build of the Windows app. Stops at the
# first failure. Each step's seconds and the total are printed. The release
# library and the C# projects build in the background while clippy and the
# Rust tests run; NVRVIEW_WORKER_DEBUGINFO=1 keeps debug information in the
# test builds (off by default: smaller and about twice as fast to build).
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

# Each step prints how long the previous one took, and the end prints the total.
check_start=$(date +%s)
step_start=$check_start
step_name=""
step() {
  local now
  now=$(date +%s)
  if [ -n "$step_name" ]; then printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"; fi
  step_start=$now
  step_name="$*"
  printf '\n== %s\n' "$*"
}

step "toolchain"
rustc --version
dotnet --version

only="${NVRVIEW_WINDOWS_ONLY:-}"

# Worker-only build settings (CI and shipping builds are unchanged). No
# debug information in the test builds: measured on this worker, a clean
# `cargo test --no-run` took about half as long and the target folder shrank
# from 5 GB to 1.7 GB. NVRVIEW_WORKER_DEBUGINFO=1 keeps it (symbolized
# backtraces from a crash).
if [ "${NVRVIEW_WORKER_DEBUGINFO:-0}" != 1 ]; then
  export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
fi
# One cargo invocation for the library and the mock: separate invocations
# unify features differently and compile shared dependencies twice.
export NVRVIEW_RELEASE_PACKAGES="-p viewer-ffi -p mock-nvr"

# The release library, the mock, and every C# project build in the
# background while clippy and the debug tests run: cargo keeps a separate
# build lock per profile, and the .NET builds need no cargo output. The
# steps that run the results come after, when these builds are already done.
prebuild_log=target/prebuild.log
prebuild() {
  # shellcheck disable=SC2086 # a list of cargo arguments
  cargo build --release $NVRVIEW_RELEASE_PACKAGES
  if [ "$only" != app ]; then
    NVRVIEW_BUILD_ONLY=1 scripts/csharp-smoke.sh
    NVRVIEW_BUILD_ONLY=1 scripts/csharp-video-selftest.sh
  fi
  dotnet build apps/windows/NvrView.Shell.Tests -c Release --artifacts-path target/dotnet -v quiet -clp:ErrorsOnly -nologo
  scripts/windows-app.sh build
}
prebuild >"$prebuild_log" 2>&1 &
prebuild_pid=$!
trap 'kill "$prebuild_pid" 2>/dev/null || true' EXIT

if [ -z "$only" ]; then
  step "clippy (release library and C# builds run in the background)"
  cargo clippy --workspace --all-targets -- -D warnings

  step "tests (build)"
  cargo test --workspace --no-run

  step "tests (run)"
  cargo test --workspace
fi

step "wait for the background builds"
if ! wait "$prebuild_pid"; then
  cat "$prebuild_log"
  echo "the background builds failed (log above)" >&2
  exit 1
fi
trap - EXIT
cat "$prebuild_log"
ls -l target/release/nvrview.dll target/release/nvrview.dll.lib

if [ "$only" != app ]; then
  step "C# smoke test"
  scripts/csharp-smoke.sh

  step "Windows video self-test (GPU, then WARP)"
  scripts/csharp-video-selftest.sh
fi

step "C# shell tests (layouts, leases, sidebar reveal, window restore)"
dotnet target/dotnet/bin/NvrView.Shell.Tests/release/NvrView.Shell.Tests.dll

if [ "${NVRVIEW_APP_SELFTEST:-0}" = 1 ]; then
  step "Windows app plain start, first run (interactive session)"
  scripts/windows-app.sh startup

  step "Windows app self-test (interactive session)"
  scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test --mic-test
fi

now=$(date +%s)
printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
printf '\nWindows checks passed in %ss.\n' "$((now - check_start))"
