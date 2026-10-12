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

# Compile cache shared by every worker on this computer (each worker has its
# own target folder; sccache keys on each crate's exact inputs, so a second
# worker on the same tree builds mostly from the cache). Without sccache the
# build is as before. No incremental caches: sccache cannot use them.
if command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"
  export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
  export CARGO_INCREMENTAL=0
  sccache --start-server >/dev/null 2>&1 || true
  sccache --zero-stats >/dev/null 2>&1 || true
fi

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
  cargo nextest run --workspace --no-run

  # cargo-nextest runs each test with the time limit in .config/nextest.toml
  # and names a test that hangs; it does not run doctests, cargo test does.
  step "tests (run, per-test time limit)"
  cargo nextest run --workspace
  step "doctests"
  cargo test --workspace --doc
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
if ! timeout "${NVRVIEW_CSHARP_TEST_TIMEOUT:-300}" dotnet target/dotnet/bin/NvrView.Shell.Tests/release/NvrView.Shell.Tests.dll; then
  echo "NvrView.Shell.Tests failed or did not finish within ${NVRVIEW_CSHARP_TEST_TIMEOUT:-300} s (timeout exits 124)" >&2
  exit 1
fi

# The real-microphone step needs a microphone that hears sound; an unattended
# test computer usually has none, so it runs only with NVRVIEW_MIC_TEST=1.
mic_test=
if [ "${NVRVIEW_MIC_TEST:-0}" = 1 ]; then mic_test=--mic-test; else echo "real-microphone step not run (NVRVIEW_MIC_TEST=1 runs it)"; fi

if [ "${NVRVIEW_APP_SELFTEST:-0}" = 1 ]; then
  step "Windows app plain start, first run (interactive session)"
  scripts/windows-app.sh startup

  step "Windows app self-test (interactive session)"
  scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test $mic_test

  step "Windows app, trimmed release build (publish)"
  scripts/windows-app.sh publish

  step "Windows app plain start, trimmed release build (interactive session)"
  NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh startup

  step "Windows app self-test, trimmed release build (interactive session)"
  NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test $mic_test

  step "Windows side-by-side update, end to end, trimmed release build (interactive session)"
  NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh update
fi

now=$(date +%s)
printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
if command -v sccache >/dev/null 2>&1; then
  sccache --show-stats 2>/dev/null | grep -E 'Compile requests executed|Cache hits  |Cache misses  |Cache size|Max cache size' || true
fi
printf '\nWindows checks passed in %ss.\n' "$((now - check_start))"
