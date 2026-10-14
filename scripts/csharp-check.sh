#!/bin/sh
# Builds the C# bindings, shell tests, and tile tests (apps/windows) and runs
# them on Linux, in Docker, against the Linux libnvrview.so and tools/mock-nvr,
# so the bindings are exercised without .NET installed on this machine.
# Windows itself runs the same tests in CI (.github/workflows/ci.yml).
#
#   scripts/csharp-check.sh [smoke] [shell] [tiles]     (default: all three)
#
#   smoke  NvrView.Smoke: the bindings against the Linux library and mock-nvr
#          (structure layout and calling details; needs the Rust container)
#   shell  NvrView.Shell.Tests: layouts, leases, window restore
#   tiles  NvrView.Tiles.Tests: the tile features on simulated time
#
# scripts/check.sh runs each as its own cached suite (csharp-docker-smoke,
# -shell, -tiles), so only the ones whose inputs changed are asked for here.
#
# At most two containers, one after the other:
#   1. (smoke only) the Rust image builds libnvrview.so, mock-nvr, and the C
#      layout helper (apps/windows/layout/nvlayout.c) and copies them to a
#      temporary host directory (removed on exit);
#   2. the .NET SDK image builds the wanted projects with ONE restore and ONE
#      build (a generated MSBuild file; shared projects compile once), then
#      runs the test programs at the same time, each with its own time limit
#      and its output printed in order.
# Sources are mounted read-only; build output is in an anonymous volume deleted
# with the container, packages in the shared nvrview-csharp-nuget volume.
#
# Docker safeguards (scripts/docker-slot.sh): at most NVRVIEW_DOCKER_SLOTS
# (default 2) heavy Docker steps at a time across all worktrees; a memory cap
# per container (NVRVIEW_DOCKER_MEMORY, default 3g); a clear message when
# Docker reports "no space left on device".
# scripts/clean-docker.sh removes the caches (--image also the images).
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/docker-rust-image.sh
. scripts/docker-slot.sh

parts="${*:-smoke shell tiles}"
for p in $parts; do
  case "$p" in
    smoke | shell | tiles) ;;
    *) echo "csharp-check: unknown part '$p' (smoke, shell, tiles)" >&2; exit 2 ;;
  esac
done
has() { case " $parts " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

dotnet_image="${NVRVIEW_DOTNET_IMAGE:-mcr.microsoft.com/dotnet/sdk:10.0.301-noble}"
mem="${NVRVIEW_DOCKER_MEMORY:-3g}"

# Resolve symlinks: on macOS TMPDIR is under /var, a symlink to /private/var.
# Docker Desktop does not share the /var/folders spelling, so a mount of it
# becomes a directory in the Docker VM's memory that this script's cleanup
# never sees, and repeated runs fill the VM ("no space left on device").
out=$(mktemp -d "${TMPDIR:-/tmp}/nvrview-csharp.XXXXXX")
out=$(cd "$out" && pwd -P)
cleanup() { docker_slot_release; rm -rf "$out"; }
trap cleanup EXIT INT TERM

docker_slot_acquire

t_start=$(date +%s)
if has smoke; then
  # shellcheck disable=SC2086 # $rust_env is a list of -e options
  docker_run --rm --memory "$mem" --memory-swap "$mem" \
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
  echo "csharp-check: Rust container $(( $(date +%s) - t_start )) s"
fi

# One MSBuild file that restores and builds the wanted projects together: the
# restore runs first with its own properties (as `dotnet build` does), then
# the build of all of them in parallel.
items=""
if has smoke; then items="$items<P Include=\"/src/apps/windows/NvrView.Smoke/NvrView.Smoke.csproj\" />"; fi
if has shell; then items="$items<P Include=\"/src/apps/windows/NvrView.Shell.Tests/NvrView.Shell.Tests.csproj\" />"; fi
if has tiles; then items="$items<P Include=\"/src/apps/windows/NvrView.Tiles.Tests/NvrView.Tiles.Tests.csproj\" />"; fi
proj="<Project><ItemGroup>$items</ItemGroup><Target Name=\"Build\">"
proj="$proj<MSBuild Projects=\"@(P)\" Targets=\"Restore\" Properties=\"MSBuildRestoreSessionId=\$([System.Guid]::NewGuid())\" />"
proj="$proj<MSBuild Projects=\"@(P)\" BuildInParallel=\"true\" /></Target></Project>"

# shellcheck disable=SC2086 # parts is a list of words
docker_run --rm --memory "$mem" --memory-swap "$mem" \
  -v "$PWD":/src:ro \
  -v "$out:/src/target:ro" \
  -v /build \
  -v nvrview-csharp-nuget:/root/.nuget/packages \
  -w /src/apps/windows \
  -e DOTNET_CLI_TELEMETRY_OPTOUT=1 \
  -e DOTNET_NOLOGO=1 \
  -e DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
  -e NVRVIEW_TEST_PASSWORD=example-password \
  -e "PARTS=$parts" \
  -e "PROJ=$proj" \
  -e "LIMIT=${NVRVIEW_CSHARP_TEST_TIMEOUT:-300}" \
  "$dotnet_image" \
  sh -c 'set -e
    printf %s "$PROJ" >/build/all.proj
    t0=$(date +%s)
    dotnet msbuild /build/all.proj -m -nologo -v:q -clp:ErrorsOnly -p:ArtifactsPath=/build/artifacts
    echo "csharp-check: restore and build $(( $(date +%s) - t0 )) s"
    t0=$(date +%s)
    bin=/build/artifacts/bin
    # The test programs run at the same time; each has its own time limit
    # (NVRVIEW_CSHARP_TEST_TIMEOUT, default 300 s) and a hang fails naming
    # the project. Outputs are printed in order.
    for p in $PARTS; do
      (
        rc=0
        case "$p" in
          smoke) timeout "$LIMIT" dotnet $bin/NvrView.Smoke/debug/NvrView.Smoke.dll \
              --mock-nvr /src/target/debug/mock-nvr \
              --fixtures /src/fixtures/synthetic \
              --lib /src/target/debug/libnvrview.so \
              --layout /src/target/csharp/layout.txt \
              --header /src/crates/viewer-ffi/include/nvrview.h >/tmp/$p.log 2>&1 || rc=$? ;;
          shell) timeout "$LIMIT" dotnet $bin/NvrView.Shell.Tests/debug/NvrView.Shell.Tests.dll >/tmp/$p.log 2>&1 || rc=$? ;;
          tiles) timeout "$LIMIT" dotnet $bin/NvrView.Tiles.Tests/debug/NvrView.Tiles.Tests.dll >/tmp/$p.log 2>&1 || rc=$? ;;
        esac
        echo $rc >/tmp/$p.rc
      ) &
    done
    wait
    echo "csharp-check: tests $(( $(date +%s) - t0 )) s (in parallel)"
    failed=""
    for p in $PARTS; do
      rc=$(cat /tmp/$p.rc)
      echo "== csharp $p: $([ "$rc" = 0 ] && echo passed || echo "FAILED (exit $rc; 124 means it did not finish within $LIMIT s)")"
      cat /tmp/$p.log
      [ "$rc" = 0 ] || failed="$failed $p"
    done
    if [ -n "$failed" ]; then echo "C# tests failed:$failed" >&2; exit 1; fi'
