#!/bin/sh
# Builds build/NVRView.app (git-ignored): a release build of the macOS app
# laid out as an app bundle, signed with the hardened runtime (an Apple
# Development identity when one is available, else ad hoc; see below).
#
#   scripts/package-macos.sh              real Rust core (builds the XCFramework)
#   scripts/package-macos.sh --fake-only  without the core (only `--fake` works)
#
# This signing is for local use. Distributing the app needs a Developer ID
# signature and notarization, which this script does not do.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

real=1
case "${1:-}" in
  "") ;;
  --fake-only) real=0 ;;
  *) echo "usage: $0 [--fake-only]" >&2; exit 2 ;;
esac

pack=apps/macos/Packaging
version=$(tr -d "[:space:]" <VERSION)
app=build/NVRView.app
work=build/package-work

if [ "$real" = 1 ]; then
  xcf=$(scripts/build-xcframework.sh)
  rm -rf apps/macos/NVRViewCore.xcframework
  cp -R "$xcf" apps/macos/NVRViewCore.xcframework
  NVRVIEW_REAL_CORE=1; export NVRVIEW_REAL_CORE
else
  unset NVRVIEW_REAL_CORE || true
fi

(cd apps/macos && swift build -c release --arch arm64 --product NVRView)
bin=$(cd apps/macos && swift build -c release --arch arm64 --show-bin-path)/NVRView

rm -rf "$app" "$work"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/AppIcon.iconset"
cp "$bin" "$app/Contents/MacOS/NVRView"
sed "s/@VERSION@/$version/g" "$pack/Info.plist.in" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Placeholder icon, drawn at build time.
for s in 16 32 64 128 256 512 1024; do
  python3 -I "$pack/make-icon.py" "$s" "$work/icon_$s.png"
done
# iconset file names: icon_WxH.png and the double-density icon_WxH<at>2x.png
at=@
set_icon() { cp "$work/icon_$1.png" "$work/AppIcon.iconset/icon_$2.png"; }
set_icon 16 16x16;     set_icon 32 "16x16${at}2x"
set_icon 32 32x32;     set_icon 64 "32x32${at}2x"
set_icon 128 128x128;  set_icon 256 "128x128${at}2x"
set_icon 256 256x256;  set_icon 512 "256x256${at}2x"
set_icon 512 512x512;  set_icon 1024 "512x512${at}2x"
iconutil -c icns "$work/AppIcon.iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$work"

# Sign with a stable identity when one is available, so the Keychain keeps
# trusting the app across rebuilds (an ad hoc signature changes with every
# build, and macOS then asks again for each saved password).
# NVRVIEW_SIGN_IDENTITY picks one ("-" forces ad hoc); otherwise the first
# "Apple Development" identity in the login keychain is used, if any.
identity="${NVRVIEW_SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
  identity=$(security find-identity -v -p codesigning 2>/dev/null |
    sed -n 's/.*\([0-9A-F]\{40\}\) "Apple Development:.*/\1/p' | head -n 1)
fi
if [ -z "$identity" ]; then identity=-; fi
if [ "$identity" = - ]; then echo "signing ad hoc"; else echo "signing with identity $identity"; fi
codesign --force --deep --sign "$identity" --options runtime \
  --entitlements "$pack/NVRView.entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
# Expected to be rejected here: the app is not notarized.
spctl --assess --type execute --verbose=2 "$app" || echo "spctl: rejected, as expected without notarization"

echo "$(pwd)/$app"
du -sh "$app" | awk '{print "size: " $1}'
