#!/bin/sh
# Builds the C# bindings and smoke test (apps/windows) and runs the smoke test
# on Linux, in Docker, against the Linux libnvrview.so and tools/mock-nvr, so
# the bindings are exercised without .NET installed on this machine. Windows
# itself runs the same smoke test in CI (.github/workflows/ci.yml).
#
# Two containers, one after the other:
#   1. the Rust image builds libnvrview.so, mock-nvr, and the C layout helper
#      (apps/windows/layout/nvlayout.c) in the build volume shared with
#      scripts/linux-check.sh (nvrview-linux-target-<checkout>, nvrview-linux-cargo);
#   2. the .NET SDK image builds apps/windows (sources mounted read-only;
#      output in the nvrview-csharp-build-<checkout> volume, packages in
#      nvrview-csharp-nuget) and runs the smoke test.
# scripts/clean-docker.sh removes the volumes (--image also the images).
set -eu
cd "$(git rev-parse --show-toplevel)"
# One build volume per checkout (worktrees mount different trees at the same
# /src path, and Cargo would take another tree's newer artifacts as fresh);
# the cargo registry is content-addressed and shared.
checkout=$(printf '%s' "$PWD" | shasum | cut -c1-8)

rust_image="${NVRVIEW_LINUX_IMAGE:-rust:1-bookworm}"
dotnet_image="${NVRVIEW_DOTNET_IMAGE:-mcr.microsoft.com/dotnet/sdk:10.0.301-noble}"

docker run --rm \
  -v "$PWD":/src \
  -v "nvrview-linux-target-$checkout:/src/target" \
  -v nvrview-linux-cargo:/usr/local/cargo/registry \
  -w /src \
  -e CARGO_TERM_COLOR=never \
  "$rust_image" \
  sh -c 'set -e
    cargo build --locked --quiet -p viewer-ffi -p mock-nvr
    mkdir -p target/csharp
    cc -std=c11 -Wall -Wextra -Werror -I crates/viewer-ffi/include \
      apps/windows/layout/nvlayout.c -o target/csharp/nvlayout
    target/csharp/nvlayout >target/csharp/layout.txt'

docker run --rm \
  -v "$PWD":/src:ro \
  -v "nvrview-linux-target-$checkout:/src/target:ro" \
  -v "nvrview-csharp-build-$checkout:/build" \
  -v nvrview-csharp-nuget:/root/.nuget/packages \
  -w /src/apps/windows \
  -e DOTNET_CLI_TELEMETRY_OPTOUT=1 \
  -e DOTNET_NOLOGO=1 \
  -e DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
  -e NVRVIEW_TEST_PASSWORD=example-password \
  "$dotnet_image" \
  sh -c 'set -e
    dotnet build NvrView.Smoke --nologo -v quiet -clp:ErrorsOnly --artifacts-path /build/artifacts
    dotnet /build/artifacts/bin/NvrView.Smoke/debug/NvrView.Smoke.dll \
      --mock-nvr /src/target/debug/mock-nvr \
      --fixtures /src/fixtures/synthetic \
      --lib /src/target/debug/libnvrview.so \
      --layout /src/target/csharp/layout.txt \
      --header /src/crates/viewer-ffi/include/nvrview.h'
