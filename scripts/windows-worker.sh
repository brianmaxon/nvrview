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

# Which suites to run, decided on the Mac by the test result cache
# (scripts/suite-cache.py; the cache lives there): NVRVIEW_SUITES is a comma
# list of suites (csharp-smoke, tiles-tests, video-selftest, shell-tests, app-update, and
# app-debug.AREA and app-publish.AREA for each app self-test area in
# scripts/windows-selftest-areas.txt; "none" for nothing), and NVRVIEW_CLIPPY_CRATES,
# NVRVIEW_TEST_CRATES, NVRVIEW_DOCTEST_CRATES say which crates the Rust steps
# cover (all, none, or a comma list). Unset means everything. Each passing
# suite prints "suite-result NAME SECONDS" for the Mac to record.
suites="${NVRVIEW_SUITES:-all}"
want() {
  [ "$suites" = all ] && return 0
  case ",$suites," in *",$1,"*) return 0 ;; esac
  return 1
}
crate_args() { # all | none | a,b
  case "${1:-all}" in
    all) echo --workspace ;;
    none) ;;
    *) echo "-p ${1//,/ -p }" ;;
  esac
}
report() { # SUITE START
  echo "suite-result $1 $(($(date +%s) - $2))"
}
app_tests=0
if [ "${NVRVIEW_APP_SELFTEST:-0}" = 1 ]; then app_tests=1; fi
# The app self-test areas to run for a build (app-debug or app-publish): a
# comma list, "all" when every area runs (then the app gets no --areas), or
# empty. The app starts once with the list; "startup" is part of every run.
all_areas=$(awk '!/^#/ && NF && !seen[$1]++ { print $1 }' scripts/windows-selftest-areas.txt)
app_areas() { # PREFIX
  local a out="" n=0 total=0
  for a in $all_areas; do
    total=$((total + 1))
    if want "$1.$a"; then out="$out${out:+,}$a"; n=$((n + 1)); fi
  done
  if [ "$n" = "$total" ]; then out=all; fi
  echo "$out"
}
report_areas() { # PREFIX AREAS START
  local a
  for a in $all_areas; do
    if want "$1.$a"; then report "$1.$a" "$3"; fi
  done
}
debug_areas=
publish_areas=
if [ "$app_tests" = 1 ]; then
  debug_areas=$(app_areas app-debug)
  publish_areas=$(app_areas app-publish)
fi
areas_arg() { # AREAS
  if [ "$1" != all ]; then printf -- '--areas %s' "$1"; fi
}
need_lib=0
if { [ "$only" != app ] && { want csharp-smoke || want video-selftest; }; } ||
  { [ "$app_tests" = 1 ] && { [ -n "$debug_areas" ] || [ -n "$publish_areas" ] || want app-update; }; } ||
  [ "$app_tests" = 0 ]; then
  need_lib=1
fi

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

# Link with rust-lld instead of link.exe on the worker (the shipping package
# build, scripts/windows-app.sh package, does not come through here and keeps
# link.exe). Measured here: the test build of all targets went from 96 s to
# 57 s with a warm compile cache, the release library from 21 s to 17 s. The
# linker is part of sccache's key, so the first run after a change recompiles.
# NVRVIEW_WORKER_LINKER=msvc keeps link.exe.
if [ "${NVRVIEW_WORKER_LINKER:-lld}" = lld ]; then
  lld_dir="$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-msvc/bin"
  if [ -f "$lld_dir/rust-lld.exe" ]; then
    export PATH="$lld_dir:$PATH"
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=rust-lld.exe
  fi
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
timed() { # LABEL COMMAND...  (prints "   (LABEL: Ns)", which the end table reads)
  local label="$1" t0
  t0=$(date +%s)
  shift
  "$@"
  printf '   (%s: %ss)\n' "$label" "$(($(date +%s) - t0))"
}
prebuild() {
  if [ "$need_lib" = 1 ]; then
    # shellcheck disable=SC2086 # a list of cargo arguments
    timed "build: release library and mock" cargo build --release $NVRVIEW_RELEASE_PACKAGES
  fi
  if [ "$only" != app ]; then
    if want csharp-smoke; then NVRVIEW_SMOKE_PARTS=smoke NVRVIEW_BUILD_ONLY=1 timed "build: C# smoke" scripts/csharp-smoke.sh; fi
    if want tiles-tests; then NVRVIEW_SMOKE_PARTS=tiles NVRVIEW_BUILD_ONLY=1 timed "build: C# tile tests" scripts/csharp-smoke.sh; fi
    if want video-selftest; then NVRVIEW_BUILD_ONLY=1 timed "build: C# video demo" scripts/csharp-video-selftest.sh; fi
  fi
  if want shell-tests; then
    timed "build: C# shell tests" dotnet build apps/windows/NvrView.Shell.Tests -c Release --artifacts-path target/dotnet -v quiet -clp:ErrorsOnly -nologo
  fi
  # Without the app tests the app build is itself the check.
  if [ "$app_tests" = 0 ] || [ -n "$debug_areas" ]; then
    timed "build: C# app" scripts/windows-app.sh build
  fi
}
prebuild >"$prebuild_log" 2>&1 &
prebuild_pid=$!
trap 'kill "$prebuild_pid" 2>/dev/null || true' EXIT

if [ -z "$only" ]; then
  # shellcheck disable=SC2086 # lists of cargo arguments
  {
    args=$(crate_args "${NVRVIEW_CLIPPY_CRATES:-all}")
    if [ -n "$args" ]; then
      step "clippy ($args; release library and C# builds run in the background)"
      cargo clippy $args --all-targets -- -D warnings
      report clippy "$step_start"
    fi

    args=$(crate_args "${NVRVIEW_TEST_CRATES:-all}")
    if [ -n "$args" ]; then
      step "tests (build: $args)"
      tests_start=$step_start
      cargo nextest run $args --no-run

      # cargo-nextest runs each test with the time limit in .config/nextest.toml
      # and names a test that hangs; it does not run doctests, cargo test does.
      step "tests (run, per-test time limit)"
      cargo nextest run $args
      report test "$tests_start"
    fi
    args=$(crate_args "${NVRVIEW_DOCTEST_CRATES:-all}")
    if [ -n "$args" ]; then
      step "doctests ($args)"
      cargo test $args --doc
      report doctest "$step_start"
    fi
  }
fi

step "wait for the background builds"
if ! wait "$prebuild_pid"; then
  cat "$prebuild_log"
  echo "the background builds failed (log above)" >&2
  exit 1
fi
trap - EXIT
cat "$prebuild_log"
if [ "$need_lib" = 1 ]; then ls -l target/release/nvrview.dll target/release/nvrview.dll.lib; fi

if [ "$only" != app ]; then
  if want csharp-smoke; then
    step "C# smoke test"
    NVRVIEW_SMOKE_PARTS=smoke scripts/csharp-smoke.sh
    report csharp-smoke "$step_start"
  fi

  if want tiles-tests; then
    step "C# tile tests"
    NVRVIEW_SMOKE_PARTS=tiles scripts/csharp-smoke.sh
    report tiles-tests "$step_start"
  fi

  if want video-selftest; then
    step "Windows video self-test (GPU, then WARP)"
    scripts/csharp-video-selftest.sh
    report video-selftest "$step_start"
  fi
fi

if want shell-tests; then
  step "C# shell tests (layouts, leases, sidebar reveal, window restore)"
  if ! timeout "${NVRVIEW_CSHARP_TEST_TIMEOUT:-300}" dotnet target/dotnet/bin/NvrView.Shell.Tests/release/NvrView.Shell.Tests.dll; then
    echo "NvrView.Shell.Tests failed or did not finish within ${NVRVIEW_CSHARP_TEST_TIMEOUT:-300} s (timeout exits 124)" >&2
    exit 1
  fi
  report shell-tests "$step_start"
fi

# The real-microphone step needs a microphone that hears sound; an unattended
# test computer usually has none, so it runs only with NVRVIEW_MIC_TEST=1.
mic_test=
if [ "${NVRVIEW_MIC_TEST:-0}" = 1 ]; then mic_test=--mic-test; else echo "real-microphone step not run (NVRVIEW_MIC_TEST=1 runs it)"; fi

if [ "$app_tests" = 1 ]; then
  if [ -n "$debug_areas" ]; then
    app_start=$(date +%s)
    if want app-debug.startup; then
      step "Windows app plain start, first run (interactive session)"
      scripts/windows-app.sh startup
    fi

    step "Windows app self-test, areas: $debug_areas (interactive session)"
    # shellcheck disable=SC2046,SC2086 # --areas and its list are two words
    scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test $mic_test $(areas_arg "$debug_areas")
    report_areas app-debug "$debug_areas" "$app_start"
  fi

  if [ -n "$publish_areas" ] || want app-update; then
    step "Windows app, trimmed release build (publish)"
    app_start=$step_start
    scripts/windows-app.sh publish
  fi

  if [ -n "$publish_areas" ]; then
    if want app-publish.startup; then
      step "Windows app plain start, trimmed release build (interactive session)"
      NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh startup
    fi

    step "Windows app self-test, trimmed release build, areas: $publish_areas (interactive session)"
    # shellcheck disable=SC2046,SC2086
    NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh selftest "${NVRVIEW_APP_SELFTEST_SECONDS:-10}" --credential-test $mic_test $(areas_arg "$publish_areas")
    report_areas app-publish "$publish_areas" "$app_start"
  fi

  if want app-update; then
    step "Windows side-by-side update, end to end, trimmed release build (interactive session)"
    NVRVIEW_APP_PUBLISHED=1 scripts/windows-app.sh update
    report app-update "$step_start"
  fi

  # Everything on the publish output passed: a later `windows-check.sh
  # --package` for the same tree ships exactly these files. (When a publish
  # suite was cached, the package step builds the publish output afresh.)
  if [ "$publish_areas" = all ] && want app-update; then
    scripts/windows-app.sh mark-tested
  fi
fi

now=$(date +%s)
printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
if command -v sccache >/dev/null 2>&1; then
  sccache --show-stats 2>/dev/null | grep -E 'Compile requests executed|Cache hits  |Cache misses  |Cache size|Max cache size' || true
fi
printf '\nWindows checks passed in %ss.\n' "$((now - check_start))"
