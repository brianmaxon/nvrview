#!/bin/sh
# Builds build/NVRView.app (git-ignored): a release build of the macOS app
# laid out as an app bundle, signed with the hardened runtime (an Apple
# Development identity when one is available, else ad hoc; see below).
#
#   scripts/package-macos.sh              real Rust core (builds the XCFramework)
#   scripts/package-macos.sh --fake-only  without the core (only `--fake` works)
#
#   --trusted-key-file FILE   public key line(s) (`nvrview-ed25519-pub ...`) the
#                             app trusts out of the box; repeatable
#   --update-source SOURCE    the update source the app uses until the user sets
#                             one (an absolute folder path or an http(s) URL)
#
#   --release-notes FILE      this version's release notes (plain text); written
#                             with --notes-history DIR (a folder of <version>.txt
#                             files) as Contents/Resources/release-notes.json
#                             before code signing (docs/UPDATES.md, "Bundled
#                             release notes"). Never committed.
#
#   --bundle-detector         also install the on-device detector's model (about
#                             37 MB) into Contents/Resources/Models, checked
#                             against scripts/detector-model.json (see
#                             scripts/detector-model.sh); without it the app
#                             finds the model in its per-user Models folder
#
# With either key or source, Contents/Resources/update-defaults.json is written before code
# signing, so the signature covers it (docs/UPDATES.md, "Bundled defaults").
# Neither is ever committed; without them nothing changes and the app trusts
# nothing until the user adds a key.
#
# The bundle is reused, not rebuilt, when it is the one the real-core self-test
# ran against for this exact tree (scripts/macos-real-selftest.sh --bundle
# leaves build/NVRView.app.tested with the tree hash; both markers need a clean
# tree). Only the bundled files are then replaced (update-defaults.json,
# release-notes.json; the binary is untouched, so what ships is what was
# tested, apart from the self-test module only the test build links in; see
# scripts/macos-real-selftest.sh), and the bundle is signed again and zipped.
# A tree that differs from the tested one in VERSION alone (a version bump)
# reuses it too: the version is not compiled in, it is only in Info.plist, so
# that file is written again from VERSION before the bundle is signed. --rebuild forces a build
# from scratch. A tree with uncommitted changes always builds.
#
# This signing is for local use. Distributing the app needs a Developer ID
# signature and notarization, which this script does not do.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ] && [ -z "${NVRVIEW_KEEP_PATH:-}" ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

real=1
rebuild=0
key_files=""
update_source=""
usage="usage: $0 [--fake-only] [--rebuild] [--trusted-key-file FILE]... [--update-source SOURCE] [--release-notes FILE [--notes-history DIR]] [--bundle-detector]"
bundle_detector=0
release_notes=""
notes_history=""
while [ $# -gt 0 ]; do
  case "$1" in
    --fake-only) real=0; shift ;;
    --rebuild) rebuild=1; shift ;;
    --bundle-detector) bundle_detector=1; shift ;;
    --trusted-key-file)
      [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
      [ -f "$2" ] || { echo "package-macos: the key file $2 does not exist" >&2; exit 2; }
      key_files="$key_files
$2"
      shift 2 ;;
    --update-source)
      [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
      update_source="$2"; shift 2 ;;
    --release-notes)
      [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
      release_notes="$2"; shift 2 ;;
    --notes-history)
      [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
      notes_history="$2"; shift 2 ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
done
# Validate before the long build: a bad key file or source stops here.
if [ -n "$key_files" ] || [ -n "$update_source" ]; then
  check_out=$(mktemp)
  set -- --out "$check_out"
  oldifs=$IFS; IFS='
'
  for f in $key_files; do if [ -n "$f" ]; then set -- "$@" --trusted-key-file "$f"; fi; done
  IFS=$oldifs
  if [ -n "$update_source" ]; then set -- "$@" --update-source "$update_source"; fi
  python3 -I scripts/update-defaults.py "$@"
  rm -f "$check_out"
fi

pack=apps/macos/Packaging
version=$(tr -d "[:space:]" <VERSION)
app=build/NVRView.app
work=build/package-work
# Its own build folder, so a check's other Swift builds never wait for this one.
scratch=""
if [ "$real" = 1 ]; then scratch="--scratch-path .build-real-core"; fi

# Reuse the bundle the self-test ran against (see the header).
tree=""
if [ -z "$(git status --porcelain)" ]; then tree=$(git rev-parse 'HEAD^{tree}'); fi
reuse=0
tested_tree=$(sed -n 's/^tree=//p' build/NVRView.app.tested 2>/dev/null || true)
built_tree=$(cat build/NVRView.app.tree 2>/dev/null || true)
# The bundle's tree is the tree it was tested for, or a tree that differs from
# it in VERSION alone.
same_but_version() { # TREE: whether it is the tree being packaged or differs from it in VERSION alone
  [ "$1" = "$tree" ] ||
    { [ -n "$1" ] && git cat-file -e "$1^{tree}" 2>/dev/null && [ "$(git diff --name-only "$1" "$tree")" = VERSION ]; }
}
if [ "$rebuild" = 0 ] && [ "$real" = 1 ] && [ "$bundle_detector" = 0 ] && [ -n "$tree" ] && [ -x "$app/Contents/MacOS/NVRView" ] &&
  [ "$tested_tree" = "$built_tree" ] && same_but_version "$tested_tree"; then
  reuse=1
  if [ "$tested_tree" = "$tree" ]; then
    echo "reusing the app bundle that passed the real-core self-test for tree $tree (only the bundled files and the signature are redone)"
  else
    echo "reusing the app bundle that passed the real-core self-test for tree $tested_tree; this tree differs only in VERSION, so only Info.plist, the bundled files and the signature are redone"
  fi
  rm -f "$app/Contents/Resources/update-defaults.json" "$app/Contents/Resources/release-notes.json"
  sed "s/@VERSION@/$version/g" "$pack/Info.plist.in" >"$app/Contents/Info.plist"
  plutil -lint "$app/Contents/Info.plist" >/dev/null
fi
if [ "$reuse" = 0 ]; then
rm -f build/NVRView.app.tree build/NVRView.app.tested
if [ "$real" = 1 ]; then
  scripts/build-xcframework.sh --install >/dev/null
  NVRVIEW_REAL_CORE=1; export NVRVIEW_REAL_CORE
else
  unset NVRVIEW_REAL_CORE || true
fi

(cd apps/macos && swift build $scratch -c release --arch arm64 --product NVRView)
bin=$(cd apps/macos && swift build $scratch -c release --arch arm64 --show-bin-path)/NVRView

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
# The skeleton is for this tree; the self-test marks it tested.
if [ "$real" = 1 ] && [ -n "$tree" ]; then echo "$tree" >build/NVRView.app.tree; fi
fi

# The bundled update public key(s) and source, inside the bundle before it is
# signed (docs/UPDATES.md, "Bundled defaults").
if [ -n "$key_files" ] || [ -n "$update_source" ]; then
  set -- --out "$app/Contents/Resources/update-defaults.json"
  oldifs=$IFS; IFS='
'
  for f in $key_files; do if [ -n "$f" ]; then set -- "$@" --trusted-key-file "$f"; fi; done
  IFS=$oldifs
  if [ -n "$update_source" ]; then set -- "$@" --update-source "$update_source"; fi
  python3 -I scripts/update-defaults.py "$@"
fi

# The release notes of this and earlier versions, inside the bundle before it
# is signed (docs/UPDATES.md, "Bundled release notes").
if [ -n "$release_notes" ]; then
  python3 -I scripts/release-notes-bundle.py --version "$version" --notes "$release_notes" \
    ${notes_history:+--history "$notes_history"} --out "$app/Contents/Resources/release-notes.json"
fi

# The pinned size and checksum of the detector model, so the app can fetch it
# on first use from the folder next to the update source and check it.
cp scripts/detector-model.json "$app/Contents/Resources/detector-model.json"

if [ "$bundle_detector" = 1 ]; then
  scripts/detector-model.sh fetch --into "$app/Contents/Resources/Models"
fi

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

# The update package (docs/UPDATES.md): the bundle with Unix permissions and
# links, without extended attributes (unpacked, AppleDouble files would break
# the signature).
zip="build/NVRView-$version-macos-arm64.zip"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"

echo "$(pwd)/$app"
du -sh "$app" | awk '{print "size: " $1}'
echo "update package: $(pwd)/$zip"
