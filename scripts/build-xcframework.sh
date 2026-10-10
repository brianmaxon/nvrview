#!/bin/sh
# Builds the core as a static library for Apple silicon (release) and
# packages it as NVRViewCore.xcframework, with the generated header and a
# module map (`import NVRViewCore` in Swift), under target/xcframework/
# (git-ignored). Prints the framework's path.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

target=aarch64-apple-darwin
cargo build --quiet --release -p viewer-ffi --target "$target"
lib="target/$target/release/libnvrview.a"

out=target/xcframework
staging="$out/staging"
framework="$out/NVRViewCore.xcframework"
rm -rf "$staging" "$framework"
mkdir -p "$staging/include"
cp crates/viewer-ffi/include/nvrview.h "$staging/include/"
cat >"$staging/include/module.modulemap" <<'MAP'
module NVRViewCore {
    header "nvrview.h"
    export *
}
MAP
# Not stripped: stripping a Rust archive in place breaks symbol
# visibility (duplicate symbols at link time); the app's link strips.
cp "$lib" "$staging/libnvrview.a"

xcodebuild -create-xcframework \
  -library "$staging/libnvrview.a" -headers "$staging/include" \
  -output "$framework" >/dev/null
rm -rf "$staging"
echo "$(pwd)/$framework"
