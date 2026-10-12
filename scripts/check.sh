#!/bin/sh
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
  echo "  remove merged worktrees (git worktree list) and old Docker volumes (scripts/clean-docker.sh)." >&2
  exit 1
fi

# Each step prints how long the previous one took, and the end prints the total.
check_start=$(date +%s)
step_start=$check_start
step_name=""
step() {
  now=$(date +%s)
  if [ -n "$step_name" ]; then printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"; fi
  step_start=$now
  step_name="$*"
  printf '\n== %s\n' "$*"
}

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
# path never writes a check stamp. NVRVIEW_FULL_CHECK=1 forces the full run.
fast=0
if [ "${NVRVIEW_FULL_CHECK:-0}" != "1" ]; then
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

step "sanitization"
python3 -I scripts/check-sanitized.py --all

step "self-test parity"
python3 -I scripts/check-parity.py

# The same full-history secret scan as CI, when Docker or gitleaks is
# available. NVRVIEW_SECRET_SCAN=0 skips it. False positives go in
# .gitleaksignore (synthetic examples only).
if [ "${NVRVIEW_SECRET_SCAN:-1}" = "0" ]; then
  step "secret scan skipped (NVRVIEW_SECRET_SCAN=0)"
  skip "secret scan"
elif command -v gitleaks >/dev/null 2>&1; then
  step "secret scan (full history)"
  gitleaks detect --source=. --redact --no-banner
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step "secret scan (full history, Docker)"
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
  step "secret scan skipped (no gitleaks or Docker)"
  skip "secret scan"
fi

if [ "$fast" = 1 ]; then
  now=$(date +%s)
  printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
  printf '\nDocs-only fast path passed in %ss (sanitization, parity, secret scan). No check stamp: run a full check before a release.\n' "$((now - check_start))"
  exit 0
fi

step "rustfmt"
cargo fmt --all --check

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings

# nextest fails a test that runs past its limit (.config/nextest.toml) and
# names it; it does not run doctests, so those run separately.
step "tests (per-test time limit)"
cargo nextest run --workspace
step "doctests"
cargo test --workspace --doc

step "check-stamp and merge scripts (throwaway repositories, fake checks)"
sh scripts/test-stamps.sh

step "C header up to date"
if command -v cbindgen >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cbindgen" ]; then
  scripts/gen-header.sh --check
else
  echo "cbindgen not installed (cargo install cbindgen --locked); skipped"
fi

step "C interface smoke test"
scripts/test-c-ffi.sh
if [ "$(uname)" = "Darwin" ]; then
  step "C interface smoke test, AddressSanitizer"
  scripts/test-c-ffi.sh --asan
fi

step "Windows compile check"
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc

step "Linux compile check"
if rustup target list --installed 2>/dev/null | grep -qx x86_64-unknown-linux-gnu; then
  cargo check --workspace --all-targets --target x86_64-unknown-linux-gnu
else
  echo "Linux target not installed (rustup target add x86_64-unknown-linux-gnu); skipped"
fi

# The full Linux tests in Docker repeat the Rust tests in a slower VM, and CI
# runs them on every push, so they are opt-in: NVRVIEW_LINUX_TESTS=1 runs them
# when the Docker daemon is running.
if [ "${NVRVIEW_LINUX_TESTS:-0}" != "1" ]; then
  step "Linux tests skipped (NVRVIEW_LINUX_TESTS=1 runs them)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step "Linux tests (Docker)"
  ran_optional="$ran_optional, Linux tests"
  scripts/linux-check.sh
else
  step "Linux tests skipped (Docker is not running)"
fi

# The C# bindings and smoke test (apps/windows) on Linux in Docker, when the
# Docker daemon is running. NVRVIEW_CSHARP_TESTS=0 skips them.
if [ "${NVRVIEW_CSHARP_TESTS:-1}" = "0" ]; then
  step "C# smoke test skipped (NVRVIEW_CSHARP_TESTS=0)"
  skip "C# tests"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step "C# smoke test (Docker)"
  scripts/csharp-check.sh
else
  step "C# smoke test skipped (Docker is not running)"
  skip "C# tests"
fi

if [ "$(uname)" = "Darwin" ] && [ -d apps/macos ]; then
  step "macOS app tests"
  # An overall limit (the whole run takes a few minutes); on a hang the
  # test that last started is named.
  (cd apps/macos &&
    python3 -I ../../scripts/with-timeout.py "${NVRVIEW_SWIFT_TEST_TIMEOUT:-600}" "swift test" swift test)

  # Opens a window for a few seconds, behind all other windows and without
  # taking focus, so it needs a desktop session. NVRVIEW_GUI_TESTS=0 skips it.
  if [ "${NVRVIEW_GUI_TESTS:-1}" = "0" ]; then
    step "macOS demo self-tests skipped (NVRVIEW_GUI_TESTS=0)"
    skip "GUI self-tests"
  elif [ "$(launchctl managername 2>/dev/null)" = "Aqua" ]; then
    step "macOS demo self-test, Metal renderer"
    (cd apps/macos && swift build --product NVRViewDemo &&
      .build/debug/NVRViewDemo --tiles 6 --fps 200 --selftest 60 --until-frames 20 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
    step "macOS demo self-test, display-layer renderer"
    (cd apps/macos &&
      .build/debug/NVRViewDemo --renderer layer --tiles 6 --fps 200 --selftest 60 --until-frames 20 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
  else
    step "macOS demo self-tests skipped (no desktop session)"
    skip "GUI self-tests"
  fi
fi

# Plays the Media Source samples in real browsers (headless Chrome and
# WebKit through playwright-cli). Off by default: it needs playwright-cli and
# browsers and takes a few minutes. NVRVIEW_BROWSER_TESTS=1 runs it.
if [ "${NVRVIEW_BROWSER_TESTS:-0}" = "1" ]; then
  step "browser playback check"
  ran_optional="$ran_optional, browser tests"
  scripts/browser-check.sh
else
  step "browser playback check skipped (NVRVIEW_BROWSER_TESTS=1 runs it)"
fi

now=$(date +%s)
printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
printf '\nAll checks passed in %ss.\n' "$((now - check_start))"

if [ "$(uname)" != "Darwin" ]; then
  echo "no check stamp written (the stamp is for the Mac checks)"
elif [ -n "$skipped" ]; then
  echo "no check stamp written; skipped:${skipped#,}"
elif [ "$tree_clean_at_start" != 1 ] || [ -n "$(git status --porcelain)" ]; then
  echo "no check stamp written (the working tree is not clean)"
else
  scripts/checked.sh --record mac "$((now - check_start))" "all default steps${ran_optional:+; optional:${ran_optional#,}}"
fi
