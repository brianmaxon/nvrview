#!/bin/bash
# Runs the macOS app's self-test on the real core (the Rust library through
# NVRViewCore.xcframework) against a local tools/mock-nvr with three
# channels, three days of recordings and AAC audio, so the recordings
# window's and the capture's real-core steps run too (SelfTest.swift,
# `recordings-real-core`, `capture-record-real-core`,
# `capture-call-record-real-core`). Never a real NVR:
# the device is the mock on 127.0.0.1, with made-up credentials, and the
# app's data directory is a throwaway one.
#
#   scripts/macos-real-selftest.sh            # builds what is missing first
#
# Not part of scripts/check.sh (it builds the release core); run it after
# changing the recordings screens or the C interface they use.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
root=$(pwd)
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

xcf=$(scripts/build-xcframework.sh)
rm -rf apps/macos/NVRViewCore.xcframework
cp -R "$xcf" apps/macos/NVRViewCore.xcframework
cargo build --quiet -p mock-nvr

work=$(mktemp -d)
mock_pid=""
cleanup() {
  if [ -n "$mock_pid" ]; then kill "$mock_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

(cd apps/macos && NVRVIEW_REAL_CORE=1 swift build --scratch-path .build-real-core)

MOCK_NVR_PASSWORD=example-password target/debug/mock-nvr --port 0 --channels 3 --recordings 3 \
  --audio "$root/fixtures/synthetic/aac-tone.aac" --fixture "$root/fixtures/synthetic/h264-basic.h264" >"$work/mock.log" 2>&1 &
mock_pid=$!
for _ in $(seq 1 100); do
  grep -q "listening on" "$work/mock.log" && break
  sleep 0.1
done
port=$(sed -n 's/.*listening on .*:\([0-9][0-9]*\)$/\1/p' "$work/mock.log" | head -1)
if [ -z "$port" ]; then
  echo "mock-nvr did not start" >&2
  cat "$work/mock.log" >&2
  exit 1
fi

mkdir -p "$work/data"
NVRVIEW_PASSWORD=example-password apps/macos/.build-real-core/debug/NVRView \
  --host 127.0.0.1 --port "$port" --user example-user --data-dir "$work/data" \
  --keychain-service nvrview-selftest-unused --selftest 60
