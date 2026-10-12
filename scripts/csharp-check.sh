#!/bin/sh
# Builds the C# bindings and smoke test (apps/windows) and runs the smoke test
# on Linux, in Docker, against the Linux libnvrview.so and tools/mock-nvr, so
# the bindings are exercised without .NET installed on this machine. Windows
# itself runs the same smoke test in CI (.github/workflows/ci.yml).
#
# Two containers, one after the other:
#   1. the Rust image builds libnvrview.so, mock-nvr, and the C layout helper
#      (apps/windows/layout/nvlayout.c) and copies them to a temporary host
#      directory (removed on exit);
#   2. the .NET SDK image builds apps/windows (sources mounted read-only;
#      output in an anonymous volume deleted with the container, packages in
#      the shared nvrview-csharp-nuget volume) and runs the smoke test, then
#      the shell tests (NvrView.Shell.Tests: layouts, leases, window restore).
# No per-checkout volumes exist; the shared caches (cargo registry, sccache,
# NuGet) are described in scripts/docker-rust-image.sh.
# scripts/clean-docker.sh removes the caches (--image also the images).
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/docker-rust-image.sh

dotnet_image="${NVRVIEW_DOTNET_IMAGE:-mcr.microsoft.com/dotnet/sdk:10.0.301-noble}"

out=$(mktemp -d "${TMPDIR:-/tmp}/nvrview-csharp.XXXXXX")
trap 'rm -rf "$out"' EXIT INT TERM


# shellcheck disable=SC2086 # $rust_env is a list of -e options
docker run --rm \
  -v "$PWD":/src \
  -v /src/target \
  -v "$out:/out" \
  -v nvrview-linux-cargo:/usr/local/cargo/registry \
  -v nvrview-sccache:/sccache \
  -w /src \
  -e CARGO_TERM_COLOR=never \
  $rust_env \
  "$rust_image" \
  sh -c 'set -e
    cargo build --locked --quiet -p viewer-ffi -p mock-nvr
    mkdir -p target/csharp
    cc -std=c11 -Wall -Wextra -Werror -I crates/viewer-ffi/include \
      apps/windows/layout/nvlayout.c -o target/csharp/nvlayout
    target/csharp/nvlayout >target/csharp/layout.txt
    mkdir -p /out/debug /out/csharp
    cp target/debug/mock-nvr target/debug/libnvrview.so /out/debug/
    cp target/csharp/layout.txt /out/csharp/'

docker run --rm \
  -v "$PWD":/src:ro \
  -v "$out:/src/target:ro" \
  -v /build \
  -v nvrview-csharp-nuget:/root/.nuget/packages \
  -w /src/apps/windows \
  -e DOTNET_CLI_TELEMETRY_OPTOUT=1 \
  -e DOTNET_NOLOGO=1 \
  -e DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
  -e NVRVIEW_TEST_PASSWORD=example-password \
  "$dotnet_image" \
  sh -c 'set -e
    # Each test process has its own time limit (NVRVIEW_CSHARP_TEST_TIMEOUT,
    # default 300 s); a hang fails here naming the project.
    run_limited() {
      name=$1; shift; rc=0
      timeout "${NVRVIEW_CSHARP_TEST_TIMEOUT:-300}" "$@" || rc=$?
      if [ "$rc" -ne 0 ]; then
        echo "$name failed (exit $rc; 124 means it did not finish within ${NVRVIEW_CSHARP_TEST_TIMEOUT:-300} s)" >&2
        exit 1
      fi
    }
    dotnet build NvrView.Smoke --nologo -v quiet -clp:ErrorsOnly --artifacts-path /build/artifacts
    run_limited NvrView.Smoke dotnet /build/artifacts/bin/NvrView.Smoke/debug/NvrView.Smoke.dll \
      --mock-nvr /src/target/debug/mock-nvr \
      --fixtures /src/fixtures/synthetic \
      --lib /src/target/debug/libnvrview.so \
      --layout /src/target/csharp/layout.txt \
      --header /src/crates/viewer-ffi/include/nvrview.h
    dotnet build NvrView.Shell.Tests --nologo -v quiet -clp:ErrorsOnly --artifacts-path /build/artifacts
    run_limited NvrView.Shell.Tests dotnet /build/artifacts/bin/NvrView.Shell.Tests/debug/NvrView.Shell.Tests.dll
    dotnet build NvrView.Tiles.Tests --nologo -v quiet -clp:ErrorsOnly --artifacts-path /build/artifacts
    run_limited NvrView.Tiles.Tests dotnet /build/artifacts/bin/NvrView.Tiles.Tests/debug/NvrView.Tiles.Tests.dll'
