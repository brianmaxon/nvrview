#!/bin/sh
# Builds, signs, and verifies the update packages for the version in VERSION
# (docs/UPDATES.md, "Maintainer workflow"). It never tags, pushes, or uploads;
# it ends by printing what to upload and in what order.
#
#   NVRVIEW_WINDOWS_HOST=<ssh host alias> scripts/release.sh --notes FILE
#       [--key-file PATH] [--out DIR] [--update-source SOURCE] [--skip-windows]
#       [--full-check] [--notes-history DIR]
#
#   --notes FILE      the release notes (plain text) shown by the apps; required
#   --notes-history DIR
#                     a folder of <version>.txt files, the notes of past
#                     versions. With --notes they are bundled in both packages
#                     as release-notes.json (newest first, signed with the
#                     package), for the apps' What's New and Help > Release
#                     Notes. Without it only this version's notes are bundled.
#   --key-file PATH   the secret signing key (default ~/.config/nvrview-signing/update.key);
#                     never printed; the script refuses if it is missing
#   --out DIR         where the packages and the signed manifest go
#                     (default dist/<version>, git-ignored)
#   --update-source SOURCE
#                     bundle this update source (an absolute folder path or an
#                     http(s) URL) as the default in both packages. The public
#                     key derived from the signing key is always bundled, so a
#                     fresh install needs no setup (docs/UPDATES.md, "Bundled
#                     defaults"). Without it only the key is bundled.
#   --skip-windows    build only the macOS package (the result is labeled
#                     macOS only and is not a complete release)
#
#   --full-check      run both checks fresh first (scripts/release-check.sh,
#                     Mac and Windows in parallel) instead of looking for stamps
#
# Refuses unless the working tree is clean and the full checks already
# passed for HEAD's tree (scripts/checked.sh --mac --windows; the stamps come
# from scripts/check.sh and NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh,
# or both at once from scripts/release-check.sh). Without --full-check it does
# not rerun them. Version-only rule: stamps of another tree are accepted when
# the only file that differs between that tree and HEAD's is VERSION (a
# version bump after a full check); the output says so. A stamp written for a
# merge that skipped its Windows run (scripts/merge-branch.sh) never counts
# for Windows. The packages are always built and signed fresh, and the Windows
# package reuses the publish output the Windows check tested when it is the
# same tree (scripts/windows-check.sh --package).
#
# The macOS package is signed for local use (scripts/package-macos.sh).
# Distributing the macOS app beyond this Mac needs a Developer ID signature
# and notarization, which this script does not do.
set -eu
cd "$(git rev-parse --show-toplevel)"
. scripts/stamps-lib.sh
if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi
if [ -z "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
fi
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

script_start=$(date +%s)
die() { echo "release: $*" >&2; exit 1; }

notes=""
key="$HOME/.config/nvrview-signing/update.key"
out=""
update_source=""
notes_history=""
skip_windows=0
full_check=0
while [ $# -gt 0 ]; do
  case "$1" in
    --notes) notes="${2:?--notes needs a file}"; shift 2 ;;
    --key-file) key="${2:?--key-file needs a path}"; shift 2 ;;
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --update-source) update_source="${2:?--update-source needs a folder path or URL}"; shift 2 ;;
    --notes-history) notes_history="${2:?--notes-history needs a folder}"; shift 2 ;;
    --skip-windows) skip_windows=1; shift ;;
    --full-check) full_check=1; shift ;;
    *) echo "usage: $0 --notes FILE [--key-file PATH] [--out DIR] [--update-source SOURCE] [--notes-history DIR] [--skip-windows] [--full-check]" >&2; exit 2 ;;
  esac
done
[ -n "$notes" ] || die "--notes FILE is required"
[ -s "$notes" ] || die "the notes file $notes is missing or empty"
[ -z "$notes_history" ] || [ -d "$notes_history" ] || die "the notes history $notes_history is not a folder"
[ -f "$key" ] || die "the signing key $key does not exist (docs/UPDATES.md, Maintainer workflow: sign-update keygen)"

version=$(tr -d '[:space:]' <VERSION)
[ -n "$version" ] || die "VERSION is empty"
[ -n "$out" ] || out="dist/$version"
if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then
  die "$out is not empty; remove it or pick another --out"
fi

[ -z "$(git status --porcelain)" ] || die "the working tree is not clean; commit or remove changes first"
if [ "$skip_windows" != 1 ]; then
  [ -n "${NVRVIEW_WINDOWS_HOSTS:-${NVRVIEW_WINDOWS_HOST:-}}" ] || die "set NVRVIEW_WINDOWS_HOST (or NVRVIEW_WINDOWS_HOSTS) to the SSH host alias of the Windows computer"
  # Releases get the Windows computer before agents' checks.
  NVRVIEW_WINDOWS_PRIORITY=merge
  export NVRVIEW_WINDOWS_PRIORITY
fi
# The public key, derived from the secret one (only the public line is kept in
# a temporary file); it is bundled into both packages and used to verify them.
cargo build -q -p sign-update
sign=target/debug/sign-update
pubfile=$(mktemp)
wlog=$(mktemp)
trap 'rm -f "$pubfile" "$wlog"' EXIT
"$sign" pubkey --secret "$key" | sed -n 1p >"$pubfile"
[ -s "$pubfile" ] || die "could not derive the public key from $key"
if [ -n "$update_source" ]; then
  # Validated here, before the long builds (the packaging scripts check again).
  python3 -I scripts/update-defaults.py --update-source "$update_source" --out /dev/null >/dev/null ||
    die "--update-source is not usable"
fi
# The bundled release notes (docs/UPDATES.md, "Bundled release notes"),
# validated here before the long builds and given to both packagers.
set -- --release-notes "$notes"
if [ -n "$notes_history" ]; then set -- "$@" --notes-history "$notes_history"; fi
python3 -I scripts/release-notes-bundle.py --version "$version" --notes "$notes" ${notes_history:+--history "$notes_history"} \
  --out /dev/null >/dev/null || die "the release notes cannot be bundled"
# The Mac check tests the shipped app bundle with the same bundled files the
# package gets (scripts/macos-real-selftest.sh --bundle), so what is zipped is
# what was tested.
NVRVIEW_BUNDLE_KEY_FILE="$pubfile"
NVRVIEW_BUNDLE_NOTES="$notes"
export NVRVIEW_BUNDLE_KEY_FILE NVRVIEW_BUNDLE_NOTES
if [ -n "$update_source" ]; then NVRVIEW_BUNDLE_UPDATE_SOURCE="$update_source"; export NVRVIEW_BUNDLE_UPDATE_SOURCE; fi
if [ -n "$notes_history" ]; then NVRVIEW_BUNDLE_NOTES_HISTORY="$notes_history"; export NVRVIEW_BUNDLE_NOTES_HISTORY; fi
if [ "$full_check" = 1 ]; then
  echo "== fresh checks (--full-check)"
  # Full runs: nothing is taken from the test result cache.
  if [ "$skip_windows" = 1 ]; then
    NVRVIEW_FULL_CHECK=1 scripts/check.sh || die "the Mac check failed"
  else
    scripts/release-check.sh || die "the checks failed"
  fi
fi
checks_done=$(date +%s)
# Each requested stamp must exist for HEAD's tree or, by the version-only
# rule, for a tree that differs from it in VERSION alone.
head_tree=$(git rev-parse 'HEAD^{tree}')
kinds="mac"
if [ "$skip_windows" != 1 ]; then kinds="mac windows"; fi
for kind in $kinds; do
  if found=$(release_stamp_tree "$kind" "$head_tree"); then
    case "$found" in
      *" version-only") echo "$kind check: accepted by the version-only rule: tree ${found% *} passed it and differs from HEAD's tree only in VERSION" ;;
      *)
        carried=$(sed -n 's/^carried_from=//p' "$(stamp_dir)/$head_tree.$kind" 2>/dev/null || true)
        if [ -n "$carried" ]; then
          echo "$kind check: passed for HEAD's tree (carried over a VERSION-only change from the full check of tree $carried)"
        else
          echo "$kind check: passed for HEAD's tree"
        fi ;;
    esac
  else
    die "the $kind check has not passed for HEAD's tree (run scripts/release-check.sh, or use --full-check)"
  fi
done

start=$(date +%s)
mkdir -p "$out"
out=$(cd "$out" && pwd)
mac_zip="NVRView-$version-macos-arm64.zip"
win_zip="NvrView-$version-win-x64.zip"

win_pid=""
if [ "$skip_windows" != 1 ]; then
  # On the Windows computer that tested the publish output, at the same time as
  # the macOS package is zipped here.
  (
    if [ -n "$update_source" ]; then
      scripts/windows-check.sh --package "$out" --trusted-key-file "$pubfile" --update-source "$update_source" "$@"
    else
      scripts/windows-check.sh --package "$out" --trusted-key-file "$pubfile" "$@"
    fi
  ) >"$wlog" 2>&1 &
  win_pid=$!
fi

echo "== macOS package"
if [ -n "$update_source" ]; then
  scripts/package-macos.sh --trusted-key-file "$pubfile" --update-source "$update_source" "$@"
else
  scripts/package-macos.sh --trusted-key-file "$pubfile" "$@"
fi
cp "build/$mac_zip" "$out/$mac_zip"

packages="$out/$mac_zip"
if [ "$skip_windows" = 1 ]; then
  echo "== Windows package skipped (--skip-windows): this is a macOS-only result"
else
  echo "== Windows package (on the Windows computer that tested it)"
  win_rc=0
  wait "$win_pid" || win_rc=$?
  cat "$wlog"
  [ "$win_rc" = 0 ] || die "the Windows package failed (exit $win_rc)"
  [ -s "$out/$win_zip" ] || die "the Windows package $win_zip was not fetched"
  chmod 644 "$out/$win_zip"
  packages="$packages $out/$win_zip"
fi

[ -z "$(git status --porcelain)" ] && [ "$head_tree" = "$(git rev-parse 'HEAD^{tree}')" ] ||
  die "the working tree changed during the build"

echo "== manifest, signature, check"
# shellcheck disable=SC2086 # a list of package paths without spaces
"$sign" manifest --version "$version" --notes-file "$notes" --expires-in 90 \
  --out "$out/updates.json" $packages
"$sign" sign --secret "$key" "$out/updates.json"
# Both packages carry the bundled defaults (the key, and the source if given).
want_key=$(sed -n 's/^nvrview-ed25519-pub \([0-9a-f]*\)$/\1/p' "$pubfile")
unzip -p "$out/$mac_zip" NVRView.app/Contents/Resources/update-defaults.json | grep -q "$want_key" ||
  die "the macOS package does not bundle the public key"
unzip -p "$out/$mac_zip" NVRView.app/Contents/Resources/release-notes.json | grep -q "\"$version\"" ||
  die "the macOS package does not bundle the release notes"
if [ "$skip_windows" != 1 ]; then
  unzip -p "$out/$win_zip" update-defaults.json | grep -q "$want_key" || die "the Windows package does not bundle the public key"
  unzip -p "$out/$win_zip" release-notes.json | grep -q "\"$version\"" || die "the Windows package does not bundle the release notes"
fi
"$sign" check --source "$out" --key "$pubfile" --current 0.0.0

now=$(date +%s)
echo
if [ "$skip_windows" = 1 ]; then
  echo "macOS ONLY release $version (no Windows package); built in $((now - start)) s."
else
  echo "Release $version built, signed, and verified in $((now - start)) s."
fi
echo "release.sh took $((now - script_start)) s in all (checks, if run: $((checks_done - script_start)) s; stamps and packages: $((now - checks_done)) s)."
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
