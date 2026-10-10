#!/bin/sh
# Run every local check CI would run, plus the macOS app tests. Stops at the
# first failure. Prefer this over pushing to find out whether something builds.
set -eu
cd "$(git rev-parse --show-toplevel)"

if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

step() { printf '\n== %s\n' "$*"; }

step "sanitization"
python3 -I scripts/check-sanitized.py --all

step "rustfmt"
cargo fmt --all --check

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "tests"
cargo test --workspace

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

# Linux tests in Docker, when the Docker daemon is running.
# NVRVIEW_LINUX_TESTS=0 skips them.
if [ "${NVRVIEW_LINUX_TESTS:-1}" = "0" ]; then
  step "Linux tests skipped (NVRVIEW_LINUX_TESTS=0)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step "Linux tests (Docker)"
  scripts/linux-check.sh
else
  step "Linux tests skipped (Docker is not running)"
fi

# The C# bindings and smoke test (apps/windows) on Linux in Docker, when the
# Docker daemon is running. NVRVIEW_CSHARP_TESTS=0 skips them.
if [ "${NVRVIEW_CSHARP_TESTS:-1}" = "0" ]; then
  step "C# smoke test skipped (NVRVIEW_CSHARP_TESTS=0)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  step "C# smoke test (Docker)"
  scripts/csharp-check.sh
else
  step "C# smoke test skipped (Docker is not running)"
fi

if [ "$(uname)" = "Darwin" ] && [ -d apps/macos ]; then
  step "macOS app tests"
  (cd apps/macos && swift test)

  # Opens a window for a few seconds, behind all other windows and without
  # taking focus, so it needs a desktop session. NVRVIEW_GUI_TESTS=0 skips it.
  if [ "${NVRVIEW_GUI_TESTS:-1}" = "0" ]; then
    step "macOS demo self-tests skipped (NVRVIEW_GUI_TESTS=0)"
  elif [ "$(launchctl managername 2>/dev/null)" = "Aqua" ]; then
    step "macOS demo self-test, Metal renderer"
    (cd apps/macos && swift build --product NVRViewDemo &&
      .build/debug/NVRViewDemo --tiles 6 --selftest 3 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
    step "macOS demo self-test, display-layer renderer"
    (cd apps/macos &&
      .build/debug/NVRViewDemo --renderer layer --tiles 6 --selftest 3 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
  else
    step "macOS demo self-tests skipped (no desktop session)"
  fi
fi

printf '\nAll checks passed.\n'
