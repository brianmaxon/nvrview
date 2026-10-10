#!/usr/bin/env bash
# Builds the C# bindings (apps/windows) and runs their smoke test against
# mock-nvr and the release build of the C interface library. CI runs it on
# Windows and Linux; scripts/windows-worker.sh runs it on a local Windows
# computer. Expects `cargo build --release -p viewer-ffi` to have run.
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export NVRVIEW_TEST_PASSWORD="${NVRVIEW_TEST_PASSWORD:-example-password}"

cargo build --release -p mock-nvr
# Relative paths (build/ is git-ignored) keep Windows drive-letter paths out
# of bash.
out=build/csharp
mkdir -p "$out"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    # clang targets the MSVC ABI the DLL is built for.
    command -v clang >/dev/null || PATH="/c/Program Files/LLVM/bin:$PATH"
    cc=clang exe=.exe lib=target/release/nvrview.dll
    ;;
  Darwin)
    cc=cc exe='' lib=target/release/libnvrview.dylib
    ;;
  *)
    cc=cc exe='' lib=target/release/libnvrview.so
    ;;
esac
"$cc" -std=c11 -Wall -Wextra -Werror -I crates/viewer-ffi/include \
  apps/windows/layout/nvlayout.c -o "$out/nvlayout$exe"
"$out/nvlayout$exe" >"$out/layout.txt"
dotnet build apps/windows/NvrView.Smoke -c Release --artifacts-path "$out/artifacts"
dotnet "$out/artifacts/bin/NvrView.Smoke/release/NvrView.Smoke.dll" \
  --mock-nvr "target/release/mock-nvr$exe" --fixtures fixtures/synthetic \
  --lib "$lib" --layout "$out/layout.txt" \
  --header crates/viewer-ffi/include/nvrview.h
# The tile features' logic, on simulated time.
dotnet build apps/windows/NvrView.Tiles.Tests -c Release --artifacts-path "$out/artifacts"
dotnet "$out/artifacts/bin/NvrView.Tiles.Tests/release/NvrView.Tiles.Tests.dll"
