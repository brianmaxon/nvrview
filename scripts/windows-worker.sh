#!/usr/bin/env bash
# Runs on a Windows computer under Git Bash, in a copy of the tree that
# scripts/windows-check.sh unpacked: the Windows part of CI (clippy, tests,
# the C interface library, the C# smoke test). Stops at the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

step "toolchain"
rustc --version
dotnet --version

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "tests"
cargo test --workspace

step "C interface library (release)"
cargo build --release -p viewer-ffi
ls -l target/release/nvrview.dll target/release/nvrview.dll.lib

step "C# smoke test"
scripts/csharp-smoke.sh

printf '\nWindows checks passed.\n'
