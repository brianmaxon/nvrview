#!/usr/bin/env bash
# Builds the headless Windows video demo (apps/windows/NvrView.Demo) and runs
# its self-test: every synthetic fixture as a mock-nvr channel, played through
# the core, Media Foundation, and the D3D11 grid renderer, checked by reading
# the rendered pixels back. Windows only; no window is shown, so it runs over
# SSH. Expects `cargo build --release -p viewer-ffi` to have run.
#
#   scripts/csharp-video-selftest.sh        GPU decoding (expected on every
#                                           tile), then WARP and CPU decoding
#   scripts/csharp-video-selftest.sh --ci   WARP and CPU decoding, H.264 only
#                                           (hosted runners have no GPU and
#                                           no HEVC Video Extension)
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
export NVRVIEW_TEST_PASSWORD="${NVRVIEW_TEST_PASSWORD:-example-password}"

# The Windows worker sets NVRVIEW_RELEASE_PACKAGES to build the library and
# the mock together: separate invocations unify features differently and
# compile shared dependencies twice.
# shellcheck disable=SC2086 # a list of cargo arguments
cargo build --release ${NVRVIEW_RELEASE_PACKAGES:--p mock-nvr}
# Relative paths (build/ is git-ignored) keep Windows drive-letter paths out
# of bash.
out=build/csharp
dotnet build apps/windows/NvrView.Demo -c Release --artifacts-path "$out/artifacts"
# NVRVIEW_BUILD_ONLY=1 stops here (scripts/windows-worker.sh builds ahead).
if [ -n "${NVRVIEW_BUILD_ONLY:-}" ]; then exit 0; fi
demo=(dotnet "$out/artifacts/bin/NvrView.Demo/release/NvrView.Demo.dll"
  --mock-nvr target/release/mock-nvr.exe --lib target/release/nvrview.dll)
f=fixtures/synthetic

case "${1:-}" in
  --ci)
    "${demo[@]}" --gpu warp --expect software --selftest 4 "$f"/*.h264
    ;;
  '')
    "${demo[@]}" --gpu hardware --expect hardware --selftest 4 "$f"/*.h264 "$f"/*.h265
    "${demo[@]}" --gpu warp --expect software --selftest 4 "$f"/*.h264 "$f"/*.h265
    ;;
  *)
    echo "usage: $0 [--ci]" >&2
    exit 2
    ;;
esac
