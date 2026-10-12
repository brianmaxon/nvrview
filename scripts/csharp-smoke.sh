#!/usr/bin/env bash
# Builds the C# bindings (apps/windows) and runs their smoke test against
# mock-nvr and the release build of the C interface library, and the tile
# features' tests (NvrView.Tiles.Tests). CI runs it on Windows and Linux;
# scripts/windows-worker.sh runs it on a local Windows computer. Expects
# `cargo build --release -p viewer-ffi` to have run.
#
# NVRVIEW_SMOKE_PARTS="smoke tiles" (the default) picks the parts: "smoke" is
# the bindings against mock-nvr and the library, "tiles" the tile tests (managed
# code only; they need neither). The Windows worker caches them as two suites.
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export NVRVIEW_TEST_PASSWORD="${NVRVIEW_TEST_PASSWORD:-example-password}"
parts=" ${NVRVIEW_SMOKE_PARTS:-smoke tiles} "
# Relative paths keep Windows drive-letter paths out of bash. Everything under
# target/ survives between worker runs, and all C# projects build into the one
# artifacts folder, so a shared project is built once and a run with no
# change rebuilds nothing.
out=target/csharp
art="${NVRVIEW_DOTNET_ARTIFACTS:-target/dotnet}"
mkdir -p "$out"

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) cc=clang exe=.exe lib=target/release/nvrview.dll ;;
  Darwin) cc=cc exe='' lib=target/release/libnvrview.dylib ;;
  *) cc=cc exe='' lib=target/release/libnvrview.so ;;
esac

case "$parts" in
  *" smoke "*)
    # The Windows worker sets NVRVIEW_RELEASE_PACKAGES to build the library
    # and the mock together: separate invocations unify features differently
    # and compile shared dependencies twice.
    # shellcheck disable=SC2086 # a list of cargo arguments
    cargo build --release ${NVRVIEW_RELEASE_PACKAGES:--p mock-nvr}
    if [ "${cc}" = clang ]; then
      # clang targets the MSVC ABI the DLL is built for.
      command -v clang >/dev/null || PATH="/c/Program Files/LLVM/bin:$PATH"
    fi
    "$cc" -std=c11 -Wall -Wextra -Werror -I crates/viewer-ffi/include \
      apps/windows/layout/nvlayout.c -o "$out/nvlayout$exe"
    "$out/nvlayout$exe" >"$out/layout.txt"
    dotnet build apps/windows/NvrView.Smoke -c Release --artifacts-path "$art" -v quiet -clp:ErrorsOnly -nologo
    ;;
esac
case "$parts" in
  *" tiles "*)
    # The tile features' logic, on simulated time.
    dotnet build apps/windows/NvrView.Tiles.Tests -c Release --artifacts-path "$art" -v quiet -clp:ErrorsOnly -nologo
    ;;
esac
# NVRVIEW_BUILD_ONLY=1 stops here (scripts/windows-worker.sh builds ahead).
if [ -n "${NVRVIEW_BUILD_ONLY:-}" ]; then exit 0; fi
case "$parts" in
  *" smoke "*)
    dotnet "$art/bin/NvrView.Smoke/release/NvrView.Smoke.dll" \
      --mock-nvr "target/release/mock-nvr$exe" --fixtures fixtures/synthetic \
      --lib "$lib" --layout "$out/layout.txt" \
      --header crates/viewer-ffi/include/nvrview.h
    ;;
esac
case "$parts" in
  *" tiles "*)
    dotnet "$art/bin/NvrView.Tiles.Tests/release/NvrView.Tiles.Tests.dll"
    ;;
esac
