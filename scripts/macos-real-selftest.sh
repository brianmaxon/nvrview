#!/bin/bash
# Runs the macOS app's self-test on the real core (the Rust library through
# NVRViewCore.xcframework) against a local tools/mock-nvr with three
# channels, three days of recordings and AAC audio, so the recordings
# window's and the capture's real-core steps run too (SelfTest*.swift,
# `recordings-real-core`, `capture-record-real-core`,
# `capture-call-record-real-core`). Never a real NVR:
# the device is the mock on 127.0.0.1, with made-up credentials, and the
# app's data directory is a throwaway one.
#
#   scripts/macos-real-selftest.sh            # the fast debug build (development)
#   scripts/macos-real-selftest.sh --bundle   # the shipped app: the release build, as the signed bundle
#
# --bundle (what scripts/check.sh runs in full runs and releases) builds
# build/NVRView.app the way scripts/package-macos.sh does, with its bundled
# files (the update defaults and the release notes, when given through
# NVRVIEW_BUNDLE_KEY_FILE, NVRVIEW_BUNDLE_UPDATE_SOURCE, NVRVIEW_BUNDLE_NOTES and
# NVRVIEW_BUNDLE_NOTES_HISTORY), and runs the self-test on a copy of that
# bundle whose executable also links the self-test module (the shipped app
# does not; docs/DEVELOPMENT.md, "The self-test is a module of its own"). After a pass on a clean tree it leaves build/NVRView.app.tested
# (the tree hash and a hash of the bundled files); a later
# scripts/package-macos.sh for the same tree zips this very bundle instead of
# building another one, and a second run for the same tree and files skips the
# test. The core framework is built once per tree (scripts/build-xcframework.sh).
#
# scripts/check.sh runs it in full runs (NVRVIEW_FULL_CHECK=1, the release
# checks, and the periodic full run); it builds the release core, so ordinary
# checks leave it out. Run it by hand after changing the recordings screens or
# the C interface they use.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
root=$(pwd)
if [ -d /opt/homebrew/opt/rustup/bin ] && [ -z "${NVRVIEW_KEEP_PATH:-}" ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

bundle=0
case "${1:-}" in
  --bundle) bundle=1 ;;
  '') ;;
  *) echo "usage: $0 [--bundle]" >&2; exit 2 ;;
esac

work=$(mktemp -d)
mock_pid=""
cleanup() {
  if [ -n "$mock_pid" ]; then kill "$mock_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

tree=""
if [ -z "$(git status --porcelain)" ]; then tree=$(git rev-parse 'HEAD^{tree}'); fi

if [ "$bundle" = 1 ]; then
  set --
  if [ -n "${NVRVIEW_BUNDLE_KEY_FILE:-}" ]; then set -- "$@" --trusted-key-file "$NVRVIEW_BUNDLE_KEY_FILE"; fi
  if [ -n "${NVRVIEW_BUNDLE_UPDATE_SOURCE:-}" ]; then set -- "$@" --update-source "$NVRVIEW_BUNDLE_UPDATE_SOURCE"; fi
  if [ -n "${NVRVIEW_BUNDLE_NOTES:-}" ]; then
    set -- "$@" --release-notes "$NVRVIEW_BUNDLE_NOTES"
    if [ -n "${NVRVIEW_BUNDLE_NOTES_HISTORY:-}" ]; then set -- "$@" --notes-history "$NVRVIEW_BUNDLE_NOTES_HISTORY"; fi
  fi
  if ! scripts/package-macos.sh "$@" >"$work/package.log" 2>&1; then
    tail -n 30 "$work/package.log" >&2
    echo "the app bundle could not be built" >&2
    exit 1
  fi
  grep -E '^reusing|signing' "$work/package.log" || true
  app=build/NVRView.app
  files=$( (cat "$app/Contents/Resources/update-defaults.json" "$app/Contents/Resources/release-notes.json" 2>/dev/null || true) | shasum -a 256 | cut -d' ' -f1)
  if [ -n "$tree" ] && [ "$(sed -n 's/^tree=//p' build/NVRView.app.tested 2>/dev/null)" = "$tree" ] &&
    [ "$(sed -n 's/^files=//p' build/NVRView.app.tested 2>/dev/null)" = "$files" ]; then
    echo "selftest: the shipped app bundle already passed for this tree and these bundled files; not run again"
    exit 0
  fi
  rm -f build/NVRView.app.tested
  # The shipped bundle has no self-test (the app code is the module
  # NVRViewApp; the self-test is the module NVRViewSelfTest, which only
  # NVRVIEW_SELFTEST=1 links). Build the self-test variant of the executable
  # in the same build folder, so NVRViewApp, the core and every other module
  # are the very objects the bundle was linked from, and run it from a copy of
  # the bundle: same Info.plist, resources and layout.
  (cd apps/macos && NVRVIEW_REAL_CORE=1 NVRVIEW_SELFTEST=1 swift build --quiet -c release --arch arm64 --product NVRView --scratch-path .build-real-core)
  st_bin=$(cd apps/macos && NVRVIEW_REAL_CORE=1 NVRVIEW_SELFTEST=1 swift build -c release --arch arm64 --product NVRView --scratch-path .build-real-core --show-bin-path)/NVRView
  ditto "$app" "$work/NVRView.app"
  cp "$st_bin" "$work/NVRView.app/Contents/MacOS/NVRView"
  exe="$work/NVRView.app/Contents/MacOS/NVRView"
else
  scripts/build-xcframework.sh --install >/dev/null
  (cd apps/macos && NVRVIEW_REAL_CORE=1 NVRVIEW_SELFTEST=1 swift build --scratch-path .build-real-core)
  exe="$root/apps/macos/.build-real-core/debug/NVRView"
fi
cargo build --quiet -p mock-nvr

MOCK_NVR_PASSWORD=example-password target/debug/mock-nvr --port 0 --channels 3 --recordings 3 --recordings-fps 20 --multi-lens 2:0,0 --ptz 1,2.1 --video-burst 2 --refuse-play-speed 16 \
  --fixture "$root/fixtures/synthetic/h264-basic.h264" --audio "$root/fixtures/synthetic/aac-smoke-t3.aac" >"$work/mock.log" 2>&1 &
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
# The device is the mock: the core skips a real device's pauses (talk-back
# pacing, a play group's seek-to-play pause; docs/FFI.md "Self-tests").
NVRVIEW_MOCK_SPEEDUP=16 NVRVIEW_PASSWORD=example-password "$exe" \
  --host 127.0.0.1 --port "$port" --user example-user --data-dir "$work/data" \
  --keychain-service nvrview-selftest-unused --selftest 60
if [ "$bundle" = 1 ] && [ -n "$tree" ]; then
  printf 'tree=%s\nfiles=%s\n' "$tree" "$files" >build/NVRView.app.tested
fi
