#!/bin/bash
# Tests that the macOS app is built once per tree and that what ships is what
# was tested (scripts/package-macos.sh, scripts/macos-real-selftest.sh --bundle,
# scripts/build-xcframework.sh) in a throwaway repository with fake build tools
# (swift, cargo, xcodebuild, codesign, ...): no real build, app, or network, and
# no waiting on the clock.
#
#   scripts/test-macos-package.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }

r="$work/repo"
bin="$work/bin"
log="$work/calls.log"
mkdir -p "$r/scripts" "$r/apps/macos/Packaging" "$r/crates/viewer-ffi/include" "$r/fixtures/synthetic" "$bin"
for f in package-macos.sh build-xcframework.sh macos-real-selftest.sh update-defaults.py release-notes-bundle.py detector-model.json; do
  cp "$src/scripts/$f" "$r/scripts/"
done
cp "$src"/apps/macos/Packaging/* "$r/apps/macos/Packaging/"
echo "// header" >"$r/crates/viewer-ffi/include/nvrview.h"
printf '%s\n' fakebin/ build/ target/ .build-real-core/ '*.xcframework/' >"$r/.gitignore"
echo 0.1.0 >"$r/VERSION"
echo 'nvrview-ed25519-pub 0000000000000000000000000000000000000000000000000000000000000001' >"$work/key.pub"
printf 'Notes for the test release.\n' >"$work/notes.txt"

# Fake tools. Each logs its call; the build outputs are tiny stand-ins.
cat >"$bin/swift" <<'EOF'
#!/bin/sh
echo "swift $*" >>"$FAKE_LOG"
case "$*" in
  *--show-bin-path*) echo "$PWD/fakebin" ;;
  *) mkdir -p fakebin; printf '#!/bin/sh\nexit 0\n' >fakebin/NVRView; chmod +x fakebin/NVRView ;;
esac
EOF
cat >"$bin/cargo" <<'EOF'
#!/bin/sh
echo "cargo $*" >>"$FAKE_LOG"
case "$*" in
  *viewer-ffi*) mkdir -p target/aarch64-apple-darwin/release; echo lib >target/aarch64-apple-darwin/release/libnvrview.a ;;
  *mock-nvr*) mkdir -p target/debug; printf '#!/bin/sh\necho "listening on 127.0.0.1:4242"\n' >target/debug/mock-nvr; chmod +x target/debug/mock-nvr ;;
esac
EOF
cat >"$bin/xcodebuild" <<'EOF'
#!/bin/sh
echo "xcodebuild" >>"$FAKE_LOG"
while [ $# -gt 0 ]; do if [ "$1" = -output ]; then mkdir -p "$2"; fi; shift; done
EOF
cat >"$bin/iconutil" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do if [ "$1" = -o ]; then : >"$2"; fi; shift; done
EOF
cat >"$bin/codesign" <<'EOF'
#!/bin/sh
echo "codesign $*" >>"$FAKE_LOG"
EOF
cat >"$bin/ditto" <<'EOF'
#!/bin/sh
echo "ditto" >>"$FAKE_LOG"
if [ "$1" != -c ]; then cp -R "$1" "$2"; exit 0; fi
for last; do :; done
echo zip >"$last"
EOF
printf '#!/bin/sh\nexit 0\n' >"$bin/plutil"
printf '#!/bin/sh\nexit 0\n' >"$bin/security"
printf '#!/bin/sh\nexit 0\n' >"$bin/spctl"
chmod +x "$bin"/*
export FAKE_LOG="$log"
export PATH="$bin:$PATH"
export NVRVIEW_SIGN_IDENTITY=- NVRVIEW_KEEP_PATH=1
unset NVRVIEW_BUNDLE_KEY_FILE NVRVIEW_BUNDLE_UPDATE_SOURCE NVRVIEW_BUNDLE_NOTES NVRVIEW_BUNDLE_NOTES_HISTORY

(
  cd "$r"
  git init -q -b main .
  git config user.email test@example.com
  git config user.name test
  git config commit.gpgsign false
  git add -A
  git commit -q -m init
)
count() { grep -c "$1" "$log" 2>/dev/null || true; }
run() { # NAME COMMAND...: output to $work/NAME.out
  name="$1"; shift
  (cd "$r" && "$@") >"$work/$name.out" 2>&1
}

# --- the core framework is built once per tree --------------------------------
: >"$log"
run xc1 sh scripts/build-xcframework.sh --install && pass "the framework builds" || { cat "$work/xc1.out"; fail "the framework builds"; }
[ "$(count 'cargo build.*viewer-ffi')" = 1 ] && [ -d "$r/apps/macos/NVRViewCore.xcframework" ] && pass "...and is installed for the app" || fail "...and is installed for the app"
run xc2 sh scripts/build-xcframework.sh --install
[ "$(count 'cargo build.*viewer-ffi')" = 1 ] && [ "$(count xcodebuild)" = 1 ] && pass "a second call for the same tree builds nothing" || fail "a second call for the same tree builds nothing"
grep -q 'reusing the build of tree' "$work/xc2.out" && pass "...and says so" || fail "...and says so"
echo "// changed" >>"$r/crates/viewer-ffi/include/nvrview.h"
run xc3 sh scripts/build-xcframework.sh --install
[ "$(count 'cargo build.*viewer-ffi')" = 2 ] && pass "a tree with changes builds again" || fail "a tree with changes builds again"
[ ! -f "$r/target/xcframework/built.tree" ] && pass "...and leaves no marker" || fail "...and leaves no marker"
(cd "$r" && git commit -q -am "header change")

# --- the bundle is built once, tested, and then zipped as tested --------------
: >"$log"
run pk1 bash scripts/package-macos.sh --trusted-key-file "$work/key.pub" && pass "a first package builds" || { cat "$work/pk1.out"; fail "a first package builds"; }
[ "$(count '^swift build.*-c release')" = 2 ] && pass "...with the release build" || fail "...with the release build"
tree=$(cd "$r" && git rev-parse 'HEAD^{tree}')
[ "$(cat "$r/build/NVRView.app.tree")" = "$tree" ] && pass "...and marks the bundle as built for this tree" || fail "...and marks the bundle as built for this tree"
[ ! -f "$r/build/NVRView.app.tested" ] && pass "...but not as tested" || fail "...but not as tested"
: >"$log"
run pk2 bash scripts/package-macos.sh --trusted-key-file "$work/key.pub"
[ "$(count '^swift build')" -ge 2 ] && pass "an untested bundle is built again" || fail "an untested bundle is built again"

# The self-test on the bundle: runs once, marks the bundle tested.
: >"$log"
export NVRVIEW_BUNDLE_KEY_FILE="$work/key.pub" NVRVIEW_BUNDLE_NOTES="$work/notes.txt"
run st1 bash scripts/macos-real-selftest.sh --bundle && pass "the bundle self-test passes" || { cat "$work/st1.out"; fail "the bundle self-test passes"; }
[ "$(sed -n 's/^tree=//p' "$r/build/NVRView.app.tested")" = "$tree" ] && pass "...and marks the bundle as tested for this tree" || fail "...and marks the bundle as tested for this tree"
[ -s "$r/build/NVRView.app/Contents/Resources/update-defaults.json" ] && [ -s "$r/build/NVRView.app/Contents/Resources/release-notes.json" ] && pass "...with the bundled files inside the tested bundle" || fail "...with the bundled files inside the tested bundle"
: >"$log"
run st2 bash scripts/macos-real-selftest.sh --bundle
grep -q 'already passed for this tree' "$work/st2.out" && pass "a second run for the same tree and files is skipped" || fail "a second run for the same tree and files is skipped"
export NVRVIEW_BUNDLE_NOTES="$work/notes2.txt"
printf 'Other notes.\n' >"$work/notes2.txt"
run st3 bash scripts/macos-real-selftest.sh --bundle
! grep -q 'already passed' "$work/st3.out" && [ "$(sed -n 's/^tree=//p' "$r/build/NVRView.app.tested")" = "$tree" ] && pass "other bundled files run the test again" || fail "other bundled files run the test again"
unset NVRVIEW_BUNDLE_KEY_FILE NVRVIEW_BUNDLE_NOTES

# The package zips the tested bundle: no build, new bundled files, signed again.
: >"$log"
run pk3 bash scripts/package-macos.sh --trusted-key-file "$work/key.pub" --update-source /srv/updates && pass "packaging a tested bundle works" || { cat "$work/pk3.out"; fail "packaging a tested bundle works"; }
[ "$(count '^swift build')" = 0 ] && [ "$(count 'cargo build')" = 0 ] && pass "...without building anything" || { cat "$log"; fail "...without building anything"; }
[ "$(count '^codesign --force')" = 1 ] && [ "$(count '^ditto')" = 1 ] && pass "...signing and zipping once" || fail "...signing and zipping once"
grep -q '/srv/updates' "$r/build/NVRView.app/Contents/Resources/update-defaults.json" && pass "...with this release's bundled files" || fail "...with this release's bundled files"
[ ! -f "$r/build/NVRView.app/Contents/Resources/release-notes.json" ] && pass "...and not the previous run's notes" || fail "...and not the previous run's notes"
: >"$log"
run pk4 bash scripts/package-macos.sh --rebuild
[ "$(count '^swift build')" -ge 2 ] && pass "--rebuild builds anyway" || fail "--rebuild builds anyway"

# A tree with changes is never reused, and a test result never carries over.
run st4 bash scripts/macos-real-selftest.sh --bundle

# A VERSION-only commit after the tested tree reuses the tested bundle: nothing
# is built, Info.plist gets the new version, the bundle is signed and zipped.
echo 0.2.0 >"$r/VERSION"
(cd "$r" && git add -A && git commit -q -m "bump the version")
: >"$log"
run pk6 bash scripts/package-macos.sh && pass "packaging after a VERSION-only commit works" || { cat "$work/pk6.out"; fail "packaging after a VERSION-only commit works"; }
[ "$(count '^swift build')" = 0 ] && [ "$(count 'cargo build')" = 0 ] && [ "$(count xcodebuild)" = 0 ] && pass "...without building anything" || { cat "$log"; fail "...without building anything"; }
grep -q 'differs only in VERSION' "$work/pk6.out" && pass "...and says why the tested bundle is reused" || fail "...and says why the tested bundle is reused"
grep -q '<string>0.2.0</string>' "$r/build/NVRView.app/Contents/Info.plist" && ! grep -q '0.1.0' "$r/build/NVRView.app/Contents/Info.plist" && pass "...with the new version in Info.plist" || fail "...with the new version in Info.plist"
[ "$(count '^codesign --force')" = 1 ] && [ "$(count '^ditto')" = 1 ] && pass "...signing and zipping once" || fail "...signing and zipping once"
[ -f "$r/build/NVRView-0.2.0-macos-arm64.zip" ] && pass "...into a package named for the new version" || fail "...into a package named for the new version"
# VERSION plus another file is not covered by the rule.
echo 0.3.0 >"$r/VERSION"
echo "// more" >>"$r/crates/viewer-ffi/include/nvrview.h"
(cd "$r" && git add -A && git commit -q -m "bump the version and change the header")
: >"$log"
run pk7 bash scripts/package-macos.sh
[ "$(count '^swift build')" -ge 2 ] && pass "a commit that changes VERSION and another file builds" || fail "a commit that changes VERSION and another file builds"
run st5 bash scripts/macos-real-selftest.sh --bundle

echo "// edit" >>"$r/VERSION.note"
: >"$log"
run pk5 bash scripts/package-macos.sh
[ "$(count '^swift build')" -ge 2 ] && pass "a tree with changes is built, not reused" || fail "a tree with changes is built, not reused"
[ ! -f "$r/build/NVRView.app.tested" ] && [ ! -f "$r/build/NVRView.app.tree" ] && pass "...and carries no markers" || fail "...and carries no markers"

# release.sh hands its bundled files to the checks and packages without rebuilding.
grep -q 'NVRVIEW_BUNDLE_KEY_FILE' "$src/scripts/release.sh" && grep -q 'macos-real-selftest.sh --bundle' "$src/scripts/check.sh" &&
  pass "release.sh passes the bundled files to the check, which tests the bundle" || fail "release.sh passes the bundled files to the check, which tests the bundle"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all macOS package tests passed"
