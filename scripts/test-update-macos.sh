#!/bin/sh
# End-to-end test of a macOS self-update (docs/UPDATES.md) on a throwaway app
# bundle in a temporary folder; the real NVRView.app is never touched.
#
#   1. builds a tiny app (a Mach-O that records its version and path when it
#      runs), version 0.3.0 "installed" and 0.4.0 packaged with ditto and
#      signed ad hoc;
#   2. publishes 0.4.0 in a folder: manifest and signature from a key made
#      here (tools/sign-update), the files marked quarantined as a browser
#      download would be;
#   3. stages it with the core's own check, download, verify, and unpack
#      (`sign-update check --stage`), next to the installed app;
#   4. starts the old app, then the update helper (`NVRView --apply-update`,
#      the debug build) with that process's ID: the helper must wait for it to
#      exit, swap the bundles, keep the old one, and reopen the app with
#      `open`;
#   5. reports quarantine attributes, the code signature, and Gatekeeper's
#      assessment of the installed update.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

cargo build -q -p sign-update
(cd apps/macos && swift build -q --product NVRView)
helper=apps/macos/.build/debug/NVRView
sign=target/debug/sign-update

tmp=${TMPDIR:-/tmp}
work=$(mktemp -d "${tmp%/}/nvrview-update-e2e.XXXXXX")
trap 'rm -rf "$work"' EXIT
mark="$work/ran.txt"
echo "work folder: $work"

# The throwaway app: records "<version> <its executable path>" and exits
# after a moment.
make_app() { # make_app DIR VERSION
  mkdir -p "$1/Contents/MacOS"
  cat >"$1/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.example.nvrview-update-test</string>
<key>CFBundleExecutable</key><string>UpdateTest</string>
<key>CFBundleName</key><string>UpdateTest</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$2</string>
<key>CFBundleVersion</key><string>$2</string>
<key>LSBackgroundOnly</key><true/>
</dict></plist>
EOF
  cat >"$work/main.c" <<EOF
#include <stdio.h>
#include <unistd.h>
#include <mach-o/dyld.h>
int main(void) {
    char path[4096]; uint32_t size = sizeof path;
    if (_NSGetExecutablePath(path, &size) != 0) path[0] = 0;
    FILE *f = fopen("$mark", "a");
    if (f) { fprintf(f, "$2 %s\n", path); fclose(f); }
    sleep(2);
    return 0;
}
EOF
  clang -O1 -o "$1/Contents/MacOS/UpdateTest" "$work/main.c"
  codesign --force --sign - "$1" >/dev/null 2>&1
}

installed="$work/Apps/UpdateTest.app"
make_app "$installed" 0.3.0
make_app "$work/build/UpdateTest.app" 0.4.0

mkdir -p "$work/keys" "$work/source"
"$sign" keygen --secret "$work/keys/update.key" --public "$work/keys/update.pub" >/dev/null
pkg="$work/source/UpdateTest-0.4.0-macos-arm64.zip"
ditto -c -k --norsrc --noextattr --keepParent "$work/build/UpdateTest.app" "$pkg"
"$sign" manifest --version 0.4.0 --notes "End-to-end test." --out "$work/source/updates.json" "$pkg" >/dev/null
"$sign" sign --secret "$work/keys/update.key" "$work/source/updates.json" >/dev/null
# As a browser would mark a download.
q="0083;$(printf %x "$(date +%s)");Safari;"
for f in "$work/source"/*; do xattr -w com.apple.quarantine "$q" "$f"; done
echo "source files quarantined: $(xattr -p com.apple.quarantine "$pkg" >/dev/null 2>&1 && echo yes || echo no)"

"$sign" check --source "$work/source" --key "$work/keys/update.pub" --current 0.3.0 \
  --platform macos-arm64 --stage "$work/Apps/.NVRView-update"
staged="$work/Apps/.NVRView-update/staging-0.4.0/content"
test -d "$staged/UpdateTest.app"

if xattr -r -l "$staged" 2>/dev/null | grep -q com.apple.quarantine; then
  echo "RESULT quarantine on the staged bundle: PRESENT"
else
  echo "RESULT quarantine on the staged bundle: none"
fi
codesign --verify --deep --strict "$staged/UpdateTest.app" && echo "RESULT staged signature: valid"

# The old app runs; the helper must wait for it.
"$installed/Contents/MacOS/UpdateTest" &
old=$!
start=$(date +%s)
"$helper" --apply-update --pid "$old" --staged "$staged" --target "$installed" --version 0.4.0 \
  --update-folder "$work/Apps/.NVRView-update" --relaunch
took=$(($(date +%s) - start))
if kill -0 "$old" 2>/dev/null; then echo "FAIL: the helper did not wait for the old app" >&2; exit 1; fi
echo "RESULT helper returned after ${took}s (the old app ran about 2 s)"
cat "$work/Apps/.NVRView-update/update.log"

v=$(defaults read "$installed/Contents/Info.plist" CFBundleShortVersionString)
pv=$(defaults read "$work/Apps/.NVRView-update/previous/UpdateTest.app/Contents/Info.plist" CFBundleShortVersionString)
[ "$v" = 0.4.0 ] || { echo "FAIL: installed version $v" >&2; exit 1; }
[ "$pv" = 0.3.0 ] || { echo "FAIL: previous version $pv" >&2; exit 1; }
echo "RESULT installed $v, previous $pv kept"

# `open` relaunched it: wait for its mark.
i=0
while ! grep -q '^0.4.0 ' "$mark" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -lt 100 ] || { echo "FAIL: the new version never ran" >&2; cat "$mark" >&2; exit 1; }
  sleep 0.1
done
ran=$(grep '^0.4.0 ' "$mark" | tail -n 1 | cut -d' ' -f2-)
case "$ran" in
  */AppTranslocation/*) echo "RESULT relaunched from a TRANSLOCATED path" ;;
  */Apps/UpdateTest.app/Contents/MacOS/UpdateTest) echo "RESULT relaunched in place (not translocated)" ;;
  *) echo "RESULT relaunched from $ran" ;;
esac
if xattr -r -l "$installed" 2>/dev/null | grep -q com.apple.quarantine; then
  echo "RESULT quarantine on the installed update: PRESENT"
else
  echo "RESULT quarantine on the installed update: none"
fi
codesign --verify --deep --strict "$installed" && echo "RESULT installed signature: valid"
echo "RESULT Gatekeeper assessment (ad hoc; rejection is expected and does not block an unquarantined app):"
spctl --assess --type execute --verbose=2 "$installed" 2>&1 | sed 's/^/  /' || true
sleep 2
echo "update end-to-end test: PASS"
