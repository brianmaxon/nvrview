#!/bin/sh
# Builds the core as a static library for Apple silicon (release) and
# packages it as NVRViewCore.xcframework, with the generated header and a
# module map (`import NVRViewCore` in Swift), under target/xcframework/
# (git-ignored). Prints the framework's path.
#
#   scripts/build-xcframework.sh             build (or reuse) and print the path
#   scripts/build-xcframework.sh --install   also put a copy where the app's
#                                            package expects it
#                                            (apps/macos/NVRViewCore.xcframework)
#
# Built once per tree: on a clean tree the framework carries a marker with the
# tree hash (target/xcframework/built.tree), and the next call for the same
# tree prints the path without running cargo or xcodebuild. A tree with
# uncommitted changes always rebuilds (cargo's own cache keeps that cheap) and
# leaves no marker. The real-core self-test and the macOS package
# (scripts/macos-real-selftest.sh, scripts/package-macos.sh) both call this, so
# a release builds the core once, not once per script.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

install=0
if [ "${1:-}" = "--install" ]; then install=1; fi

target=aarch64-apple-darwin
out=target/xcframework
staging="$out/staging"
framework="$out/NVRViewCore.xcframework"
tree=""
if [ -z "$(git status --porcelain)" ]; then tree=$(git rev-parse 'HEAD^{tree}'); fi

if [ -n "$tree" ] && [ -d "$framework" ] && [ "$(cat "$out/built.tree" 2>/dev/null || true)" = "$tree" ]; then
  echo "core framework: reusing the build of tree $tree" >&2
else
  rm -f "$out/built.tree" "$out/installed.tree"
  cargo build --quiet --release -p viewer-ffi --target "$target"
  lib="target/$target/release/libnvrview.a"

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
  if [ -n "$tree" ]; then echo "$tree" >"$out/built.tree"; fi
fi

if [ "$install" = 1 ]; then
  dest=apps/macos/NVRViewCore.xcframework
  if [ -n "$tree" ] && [ -d "$dest" ] && [ "$(cat "$out/installed.tree" 2>/dev/null || true)" = "$tree" ]; then
    :
  else
    rm -rf "$dest" "$out/installed.tree"
    cp -R "$framework" "$dest"
    if [ -n "$tree" ]; then echo "$tree" >"$out/installed.tree"; fi
  fi
fi
echo "$(pwd)/$framework"
