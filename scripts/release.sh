#!/bin/sh
# Builds, signs, and verifies the update packages for the version in VERSION
# (docs/UPDATES.md, "Maintainer workflow"). It never tags, pushes, or uploads;
# it ends by printing what to upload and in what order.
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> scripts/release.sh --notes FILE
#       [--key-file PATH] [--out DIR] [--skip-windows]
#
#   --notes FILE      the release notes (plain text) shown by the apps; required
#   --key-file PATH   the secret signing key (default ~/.config/nvrview-signing/update.key);
#                     never printed; the script refuses if it is missing
#   --out DIR         where the packages and the signed manifest go
#                     (default dist/<version>, git-ignored)
#   --skip-windows    build only the macOS package (the result is labeled
#                     macOS only and is not a complete release)
#
# Refuses unless the working tree is clean and the full checks already
# passed for HEAD's tree (scripts/checked.sh --mac --windows; the stamps come
# from scripts/check.sh and NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh).
# It does not rerun them.
#
# The macOS package is signed for local use (scripts/package-macos.sh).
# Distributing the macOS app beyond this Mac needs a Developer ID signature
# and notarization, which this script does not do.
set -eu
cd "$(git rev-parse --show-toplevel)"
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi
if [ -z "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
fi
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

die() { echo "release: $*" >&2; exit 1; }

notes=""
key="$HOME/.config/nvrview-signing/update.key"
out=""
skip_windows=0
while [ $# -gt 0 ]; do
  case "$1" in
    --notes) notes="${2:?--notes needs a file}"; shift 2 ;;
    --key-file) key="${2:?--key-file needs a path}"; shift 2 ;;
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --skip-windows) skip_windows=1; shift ;;
    *) echo "usage: $0 --notes FILE [--key-file PATH] [--out DIR] [--skip-windows]" >&2; exit 2 ;;
  esac
done
[ -n "$notes" ] || die "--notes FILE is required"
[ -s "$notes" ] || die "the notes file $notes is missing or empty"
[ -f "$key" ] || die "the signing key $key does not exist (docs/UPDATES.md, Maintainer workflow: sign-update keygen)"

version=$(tr -d '[:space:]' <VERSION)
[ -n "$version" ] || die "VERSION is empty"
[ -n "$out" ] || out="dist/$version"
if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then
  die "$out is not empty; remove it or pick another --out"
fi

[ -z "$(git status --porcelain)" ] || die "the working tree is not clean; commit or remove changes first"
if [ "$skip_windows" = 1 ]; then
  scripts/checked.sh --mac || die "the checks have not passed for HEAD's tree"
else
  scripts/checked.sh --mac --windows || die "the checks have not passed for HEAD's tree"
  [ -n "${NVRVIEW_WINDOWS_HOST:-}" ] || die "set NVRVIEW_WINDOWS_HOST to the SSH host alias of the Windows computer"
fi
head_tree=$(git rev-parse 'HEAD^{tree}')

start=$(date +%s)
mkdir -p "$out"
out=$(cd "$out" && pwd)
mac_zip="NVRView-$version-macos-arm64.zip"
win_zip="NvrView-$version-win-x64.zip"

echo "== macOS package"
scripts/package-macos.sh
cp "build/$mac_zip" "$out/$mac_zip"

packages="$out/$mac_zip"
if [ "$skip_windows" = 1 ]; then
  echo "== Windows package skipped (--skip-windows): this is a macOS-only result"
else
  echo "== Windows package (built on $NVRVIEW_WINDOWS_HOST)"
  scripts/windows-check.sh --package "$out"
  [ -s "$out/$win_zip" ] || die "the Windows package $win_zip was not fetched"
  chmod 644 "$out/$win_zip"
  packages="$packages $out/$win_zip"
fi

[ -z "$(git status --porcelain)" ] && [ "$head_tree" = "$(git rev-parse 'HEAD^{tree}')" ] ||
  die "the working tree changed during the build"

echo "== manifest, signature, check"
cargo build -q -p sign-update
sign=target/debug/sign-update
# shellcheck disable=SC2086 # a list of package paths without spaces
"$sign" manifest --version "$version" --notes-file "$notes" --expires-in 90 \
  --out "$out/updates.json" $packages
"$sign" sign --secret "$key" "$out/updates.json"
# The public key, derived from the secret one; only the public line is kept.
pubfile=$(mktemp)
trap 'rm -f "$pubfile"' EXIT
"$sign" pubkey --secret "$key" | sed -n 1p >"$pubfile"
"$sign" check --source "$out" --key "$pubfile" --current 0.0.0

now=$(date +%s)
echo
if [ "$skip_windows" = 1 ]; then
  echo "macOS ONLY release $version (no Windows package); built in $((now - start)) s."
else
  echo "Release $version built, signed, and verified in $((now - start)) s."
fi
echo "Files in $out:"
ls -l "$out"
echo
echo "Upload to the update location in this order (a client must never see a"
echo "manifest whose package is not there yet):"
n=1
for f in $packages; do
  echo "  $n. $(basename "$f")"
  n=$((n + 1))
done
echo "  $n. updates.json"
echo "  $((n + 1)). updates.json.sig   (last)"
echo
echo "Not done by this script: tagging, pushing, uploading, and the macOS Developer ID"
echo "signature and notarization needed to distribute the app beyond this Mac."
