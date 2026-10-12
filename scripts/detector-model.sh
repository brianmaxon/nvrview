#!/bin/sh
# The on-device detector's Core ML model (docs/VISION.md, "Models and
# licenses"). The model is never committed; this script makes it and gets it.
#
#   scripts/detector-model.sh build [OUTDIR]
#       Converts the pinned RT-DETR R18 weights (see tools/vision-lab/scripts/
#       convert-rtdetr.py and convert-requirements.txt) in a throwaway Python
#       environment, compiles the package to a .mlmodelc, zips it and prints
#       the size and SHA-256 to put in scripts/detector-model.json when the
#       zip is published. OUTDIR defaults to build/detector-model (git-ignored).
#
#   scripts/detector-model.sh fetch [--into DIR] [--file ZIP]
#       Installs the model into DIR (default: the per-user Models folder the
#       app looks in): downloads the manifest's url (or takes --file), checks
#       the size and SHA-256 the manifest pins, and unpacks rtdetr_r18vd.mlmodelc.
#       Refuses a file that does not match, and says so when the manifest has
#       no published artifact yet.
#
# scripts/package-macos.sh --bundle-detector runs `fetch` into the app bundle.
set -eu
cd "$(git rev-parse --show-toplevel)"
manifest=scripts/detector-model.json
name=rtdetr_r18vd

field() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$manifest" "$1"; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }

cmd="${1:-}"
[ $# -gt 0 ] && shift
case "$cmd" in
  build)
    out="${1:-build/detector-model}"
    mkdir -p "$out"
    python3 -m venv "$out/venv"
    "$out/venv/bin/pip" install -q -r tools/vision-lab/scripts/convert-requirements.txt
    rm -rf "$out/$name.mlpackage" "$out/$name.mlmodelc" "$out/$name.mlmodelc.zip"
    "$out/venv/bin/python" -I tools/vision-lab/scripts/convert-rtdetr.py "$out/$name.mlpackage"
    xcrun coremlcompiler compile "$out/$name.mlpackage" "$out" >/dev/null
    (cd "$out" && ditto -c -k --norsrc --noextattr --keepParent "$name.mlmodelc" "$name.mlmodelc.zip")
    echo "artifact: $out/$name.mlmodelc.zip"
    echo "bytes:    $(stat -f %z "$out/$name.mlmodelc.zip")"
    echo "sha256:   $(sha256 "$out/$name.mlmodelc.zip")"
    ;;
  fetch)
    into="$HOME/Library/Application Support/NVRView/Models"
    file=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --into) [ $# -ge 2 ] || { echo "usage: $0 fetch [--into DIR] [--file ZIP]" >&2; exit 2; }; into="$2"; shift 2 ;;
        --file) [ $# -ge 2 ] || { echo "usage: $0 fetch [--into DIR] [--file ZIP]" >&2; exit 2; }; file="$2"; shift 2 ;;
        *) echo "usage: $0 fetch [--into DIR] [--file ZIP]" >&2; exit 2 ;;
      esac
    done
    want=$(field sha256)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    if [ -z "$file" ]; then
      url=$(field url)
      if [ -z "$url" ]; then
        echo "detector-model: scripts/detector-model.json names no published artifact yet; run '$0 build', publish the zip, and record its url, size and sha256" >&2
        exit 1
      fi
      curl --fail --location --silent --show-error --output "$tmp/model.zip" "$url"
      file="$tmp/model.zip"
    fi
    [ -f "$file" ] || { echo "detector-model: $file does not exist" >&2; exit 2; }
    if [ -z "$want" ]; then
      echo "detector-model: the manifest pins no sha256, refusing to install an unchecked model" >&2
      exit 1
    fi
    [ "$(stat -f %z "$file")" = "$(field bytes)" ] || { echo "detector-model: size differs from the manifest" >&2; exit 1; }
    [ "$(sha256 "$file")" = "$want" ] || { echo "detector-model: sha256 differs from the manifest" >&2; exit 1; }
    ditto -x -k "$file" "$tmp/unpacked"
    [ -d "$tmp/unpacked/$name.mlmodelc" ] || { echo "detector-model: the archive has no $name.mlmodelc" >&2; exit 1; }
    mkdir -p "$into"
    rm -rf "$into/$name.mlmodelc"
    mv "$tmp/unpacked/$name.mlmodelc" "$into/$name.mlmodelc"
    echo "installed: $into/$name.mlmodelc"
    ;;
  *)
    echo "usage: $0 build [OUTDIR] | fetch [--into DIR] [--file ZIP]" >&2
    exit 2
    ;;
esac
