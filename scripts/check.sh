#!/bin/sh
# shellcheck disable=SC2086 # $args holds lists of cargo arguments
# Run every local check CI would run, plus the macOS app tests. Stops at the
# first failure. Prefer this over pushing to find out whether something builds.
set -eu
cd "$(git rev-parse --show-toplevel)"

if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

# Build caching, for this repository only: with sccache installed, a shared
# compile cache keyed by each crate's exact inputs, so checkouts and agent
# worktrees reuse each other's dependency builds and can never overwrite each
# other's results; no incremental caches (the largest part of target/, and
# sccache cannot use them). Either can be overridden from the environment.
if [ -z "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
  export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
fi
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Disk guard: a full disk fails builds halfway and leaves every tool unable to
# write. NVRVIEW_MIN_FREE_GB changes the threshold.
min_free_gb="${NVRVIEW_MIN_FREE_GB:-15}"
free_kb=$(df -Pk . | awk 'NR == 2 { print $4 }')
if [ "$free_kb" -lt $((min_free_gb * 1024 * 1024)) ]; then
  echo "check.sh: $((free_kb / 1024 / 1024)) GB free, needs $min_free_gb GB. Free space first:" >&2
  echo "  remove merged worktrees (git worktree list) and old Docker volumes (scripts/clean-docker-volumes.sh)." >&2
  exit 1
fi


# Test result cache (scripts/suite-cache.py): a suite whose inputs (its
# files as committed or staged, its workspace crates, tool versions) have a
# clean pass on this Mac within the last 72 hours is skipped and printed as
# "cached pass ...". Rust runs only the crates whose fingerprint changed.
# A full run (nothing cached) happens with NVRVIEW_FULL_CHECK=1, when no full
# run passed in the last NVRVIEW_FULL_MAX_AGE_H hours (default 72), and on
# every NVRVIEW_FULL_EVERY-th run when that is set. A table at the end shows
# what ran and what was cached.
sc() { python3 -I scripts/suite-cache.py "$@"; }
run_file=""

# Each step prints how long the previous one took, and the end prints the
# total. "step ROW LABEL": ROW names the step's row in the table and, for a
# cached suite or Rust family, what a pass records.
check_start=$(date +%s)
step_start=$check_start
step_name=""
step_row=""
end_step() {
  now=$(date +%s)
  if [ -n "$step_name" ]; then
    printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
    if [ -n "$run_file" ] && [ "$step_row" != "-" ]; then sc pass "$run_file" "$((now - step_start))" "$step_row"; fi
  fi
  step_name=""
}
step() { # ROW LABEL ("-" for no row)
  end_step
  step_start=$now
  step_row="$1"
  step_name="$2"
  printf '\n== %s\n' "$2"
}
# Runs the step for SUITE unless it is cached (then it prints why and fails).
want() { # SUITE LABEL
  end_step
  if [ -n "$run_file" ] && ! sc want "$run_file" "$1"; then return 1; fi
  step "$1" "$2"
}
# Sets args to the cargo arguments for the crates of a Rust family that must
# run (empty: all cached); the cached ones are printed.
crates() { # FAMILY
  end_step
  if [ -n "$run_file" ]; then args=$(sc crates "$run_file" "$1"); else args=--workspace; fi
}
on_exit() {
  rc=$?
  if [ -n "$run_file" ]; then
    sc finish "$run_file" --failed "$step_name" || true
  fi
  exit "$rc"
}
trap on_exit EXIT

# Check stamps (scripts/checked.sh): a full pass on a clean tree is recorded
# for HEAD's tree. "skipped" collects steps that run by default but were
# skipped (no stamp then); "ran_optional" lists the opt-in steps that ran.
skipped=""
ran_optional=""
skip() { skipped="$skipped, $1"; }
tree_clean_at_start=0
if [ -z "$(git status --porcelain)" ]; then tree_clean_at_start=1; fi

# Docs-only fast path. The changes being checked are the staged, unstaged,
# and untracked changes, plus the commits not yet on the branch's upstream
# (when it has one). A clean tree with no upstream has no changes to judge
# and gets the full check. When there are changes and every one is a *.md
# file, only sanitization, self-test parity, and the secret scan run. The fast
# path never writes a check stamp. NVRVIEW_FULL_CHECK=1 forces the full run;
# NVRVIEW_DOCS_FAST_PATH=0 turns the fast path off but keeps the cache
# (scripts/merge-branch.sh).
fast=0
if [ "${NVRVIEW_FULL_CHECK:-0}" != "1" ] && [ "${NVRVIEW_DOCS_FAST_PATH:-1}" != "0" ]; then
  changed=$( {
    git diff --name-only HEAD
    git ls-files --others --exclude-standard
    if upstream=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null); then
      git diff --name-only "$upstream"...HEAD
    fi
  } | sort -u)
  if [ -n "$changed" ] && ! printf '%s\n' "$changed" | grep -qv '\.md$'; then
    fast=1
    echo "docs-only change ($(printf '%s\n' "$changed" | wc -l | tr -d ' ') markdown files): taking the fast path (sanitization, parity, secret scan; NVRVIEW_FULL_CHECK=1 runs everything)"
  fi
fi

# Per-test time limits come from .config/nextest.toml. Install cargo-nextest
# when it is missing, as the check cannot run its tests without it.
if [ "$fast" = 0 ] && ! cargo nextest --version >/dev/null 2>&1; then
  echo "installing cargo-nextest (cargo install cargo-nextest --locked)"
  cargo install cargo-nextest --locked
fi

if [ "$fast" = 0 ]; then
  run_file=$(sc plan mac)
fi

step sanitization "sanitization"
python3 -I scripts/check-sanitized.py --all

step parity "self-test parity"
python3 -I scripts/check-parity.py

step selftest-areas "Windows self-test areas"
python3 -I scripts/check-selftest-areas.py

# The same full-history secret scan as CI, when Docker or gitleaks is
# available. NVRVIEW_SECRET_SCAN=0 skips it. False positives go in
# .gitleaksignore (synthetic examples only).
if [ "${NVRVIEW_SECRET_SCAN:-1}" = "0" ]; then
  step - "secret scan skipped (NVRVIEW_SECRET_SCAN=0)"
  skip "secret scan"
elif command -v gitleaks >/dev/null 2>&1; then
  step secret-scan "secret scan (full history)"
  gitleaks detect --source=. --redact --no-banner
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step secret-scan "secret scan (full history, Docker)"
  # Mounted at the same paths as on the host, with the repository's common
  # git directory too: in a git worktree .git is a file pointing there, and
  # without it git fails and gitleaks scans nothing yet exits 0. A scan that
  # reports no commits is therefore a failure.
  common="$(cd "$(git rev-parse --git-common-dir)" && pwd)"
  scan_log="$(mktemp)"
  if docker run --rm -v "$PWD:$PWD" -v "$common:$common" -w "$PWD" \
    zricethezav/gitleaks:v8.18.4 detect --source="$PWD" --redact --no-banner \
    >"$scan_log" 2>&1; then scan_rc=0; else scan_rc=$?; fi
  cat "$scan_log"
  if [ "$scan_rc" -ne 0 ]; then
    rm -f "$scan_log"
    exit "$scan_rc"
  fi
  if ! grep -Eq '[1-9][0-9]* commits scanned' "$scan_log"; then
    rm -f "$scan_log"
    echo "secret scan: no commits were scanned" >&2
    exit 1
  fi
  rm -f "$scan_log"
else
  step - "secret scan skipped (no gitleaks or Docker)"
  skip "secret scan"
fi

if [ "$fast" = 1 ]; then
  now=$(date +%s)
  printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
  printf '\nDocs-only fast path passed in %ss (sanitization, parity, secret scan). No check stamp: run a full check before a release.\n' "$((now - check_start))"
  exit 0
fi

step rustfmt "rustfmt"
cargo fmt --all --check

# The Rust families run only the crates whose fingerprint missed the cache
# (--workspace when all did). Arguments are lists of "-p crate".
crates clippy
if [ -n "$args" ]; then
  step clippy "clippy ($args)"
  cargo clippy $args --all-targets -- -D warnings
fi

# nextest fails a test that runs past its limit (.config/nextest.toml) and
# names it; it does not run doctests, so those run separately.
crates test
if [ -n "$args" ]; then
  step test "tests, per-test time limit ($args)"
  cargo nextest run $args
fi
crates doctest
if [ -n "$args" ]; then
  step doctest "doctests ($args)"
  cargo test $args --doc
fi

if want script-tests "check-stamp, merge, merge-train, lock, and cache scripts (throwaway repositories, fake checks)"; then
  sh scripts/test-stamps.sh
  sh scripts/test-suite-cache.sh
  sh scripts/test-merge-train.sh
fi

if command -v cbindgen >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cbindgen" ]; then
  if want c-header "C header up to date"; then
    scripts/gen-header.sh --check
  fi
else
  step - "C header up to date"
  echo "cbindgen not installed (cargo install cbindgen --locked); skipped"
fi

if want c-smoke "C interface smoke test"; then
  scripts/test-c-ffi.sh
fi
if [ "$(uname)" = "Darwin" ] && want c-smoke-asan "C interface smoke test, AddressSanitizer"; then
  scripts/test-c-ffi.sh --asan
fi

crates wincheck
if [ -n "$args" ]; then
  step wincheck "Windows compile check ($args)"
  cargo check $args --all-targets --target x86_64-pc-windows-msvc
fi

if rustup target list --installed 2>/dev/null | grep -qx x86_64-unknown-linux-gnu; then
  crates linuxcheck
  if [ -n "$args" ]; then
    step linuxcheck "Linux compile check ($args)"
    cargo check $args --all-targets --target x86_64-unknown-linux-gnu
  fi
else
  step - "Linux compile check"
  echo "Linux target not installed (rustup target add x86_64-unknown-linux-gnu); skipped"
fi

# The full Linux tests in Docker repeat the Rust tests in a slower VM, and CI
# runs them on every push, so they are opt-in: NVRVIEW_LINUX_TESTS=1 runs them
# when the Docker daemon is running.
if [ "${NVRVIEW_LINUX_TESTS:-0}" != "1" ]; then
  step - "Linux tests skipped (NVRVIEW_LINUX_TESTS=1 runs them)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  ran_optional="$ran_optional, Linux tests"
  if want linux-tests "Linux tests (Docker)"; then
    scripts/linux-check.sh
  fi
else
  step - "Linux tests skipped (Docker is not running)"
fi

# The C# bindings and smoke test (apps/windows) on Linux in Docker, when the
# Docker daemon is running. NVRVIEW_CSHARP_TESTS=0 skips them.
if [ "${NVRVIEW_CSHARP_TESTS:-1}" = "0" ]; then
  step - "C# smoke test skipped (NVRVIEW_CSHARP_TESTS=0)"
  skip "C# tests"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  if want csharp-docker "C# smoke test (Docker)"; then
    scripts/csharp-check.sh
  fi
else
  step - "C# smoke test skipped (Docker is not running)"
  skip "C# tests"
fi

if [ "$(uname)" = "Darwin" ] && [ -d apps/macos ]; then
  if want swift-test "macOS app tests"; then
    # An overall limit (the whole run takes a few minutes); on a hang the
    # test that last started is named.
    (cd apps/macos &&
      python3 -I ../../scripts/with-timeout.py "${NVRVIEW_SWIFT_TEST_TIMEOUT:-600}" "swift test" swift test)
  fi

  # Opens a window for a few seconds, behind all other windows and without
  # taking focus, so it needs a desktop session. NVRVIEW_GUI_TESTS=0 skips it.
  if [ "${NVRVIEW_GUI_TESTS:-1}" = "0" ]; then
    step - "macOS demo self-tests skipped (NVRVIEW_GUI_TESTS=0)"
    skip "GUI self-tests"
  elif [ "$(launchctl managername 2>/dev/null)" = "Aqua" ]; then
    if want macos-demo-metal "macOS demo self-test, Metal renderer"; then
      (cd apps/macos && swift build --product NVRViewDemo &&
        .build/debug/NVRViewDemo --tiles 6 --fps 200 --selftest 60 --until-frames 20 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
    fi
    if want macos-demo-layer "macOS demo self-test, display-layer renderer"; then
      (cd apps/macos && swift build --product NVRViewDemo &&
        .build/debug/NVRViewDemo --renderer layer --tiles 6 --fps 200 --selftest 60 --until-frames 20 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
    fi
  else
    step - "macOS demo self-tests skipped (no desktop session)"
    skip "GUI self-tests"
  fi
fi

# Plays the Media Source samples in real browsers (headless Chrome and
# WebKit through playwright-cli). Off by default: it needs playwright-cli and
# browsers and takes a few minutes. NVRVIEW_BROWSER_TESTS=1 runs it.
if [ "${NVRVIEW_BROWSER_TESTS:-0}" = "1" ]; then
  ran_optional="$ran_optional, browser tests"
  if want browser "browser playback check"; then
    scripts/browser-check.sh
  fi
else
  step - "browser playback check skipped (NVRVIEW_BROWSER_TESTS=1 runs it)"
fi

end_step
step_name=""
summary=$(mktemp)
sc finish "$run_file" --ok ${skipped:+--partial} | tee "$summary"
run_file=""
cached=$(sed -n 's/.*; cached suites: //p' "$summary")
rm -f "$summary"
printf '\nAll checks passed in %ss.\n' "$((now - check_start))"

if [ "$(uname)" != "Darwin" ]; then
  echo "no check stamp written (the stamp is for the Mac checks)"
elif [ -n "$skipped" ]; then
  echo "no check stamp written; skipped:${skipped#,}"
elif [ "$tree_clean_at_start" != 1 ] || [ -n "$(git status --porcelain)" ]; then
  echo "no check stamp written (the working tree is not clean)"
else
  # A stamp with cached suites serves merges; a release needs a full one.
  scripts/checked.sh --record mac "$((now - check_start))" "all default steps${ran_optional:+; optional:${ran_optional#,}}" "${cached:-none}"
fi
