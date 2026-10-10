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

step "Windows compile check"
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc

if [ "$(uname)" = "Darwin" ] && [ -d apps/macos ]; then
  step "macOS app tests"
  (cd apps/macos && swift test)
fi

printf '\nAll checks passed.\n'
