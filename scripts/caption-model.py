#!/usr/bin/env python3
"""The Windows captions download (apps/windows/README.md, "Captions"). Never committed; this
script makes it from pinned sources.

    python3 -I scripts/caption-model.py build [--out DIR] [--cache DIR] [--write]

Downloads every source apps/windows/NvrView.App/caption-model.json names (the Moonshine
streaming model files, moonshine.dll from the Moonshine release's Windows wheel, onnxruntime.dll
from Microsoft's ONNX Runtime package, and their licenses), refuses any file whose SHA-256
differs from the pin, and zips them as <name>/... into DIR/<name>.zip (default build/caption-model,
git-ignored) with fixed timestamps and order, so the same sources give the same bytes. Prints
the archive's size and SHA-256; --write records them in the manifest (the app pins them at build
time). Publishing: copy <name>.zip into the `models` folder next to updates.json on the update
source before the release that pins it. Downloads are cached in --cache (default
build/caption-model/cache).
"""

import argparse
import hashlib
import io
import json
import os
import sys
import urllib.request
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "apps", "windows", "NvrView.App", "caption-model.json")
STAMP = (1980, 1, 1, 0, 0, 0)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fetch(url, want, cache):
    """The bytes at url, from the cache when they are there and match; refuses a mismatch."""
    os.makedirs(cache, exist_ok=True)
    path = os.path.join(cache, want)
    if os.path.exists(path):
        with open(path, "rb") as f:
            data = f.read()
        if sha256(data) == want:
            return data
    # Some of these hosts refuse Python's default user agent.
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "curl/8"})) as r:
        data = r.read()
    got = sha256(data)
    if got != want:
        sys.exit(f"caption-model: {url} has SHA-256 {got}, the manifest pins {want}")
    with open(path, "wb") as f:
        f.write(data)
    return data


def build(args):
    with open(MANIFEST) as f:
        manifest = json.load(f)
    name = manifest["name"]
    src = manifest["sources"]
    files = {}  # path in the folder -> bytes
    model = src["model"]
    for file, want in model["sha256"].items():
        files[file] = fetch(model["base_url"] + file, want, args.cache)
    for key in ("moonshine", "onnxruntime"):
        package = fetch(src[key]["url"], src[key]["sha256"], args.cache)
        with zipfile.ZipFile(io.BytesIO(package)) as z:
            for inner, out in src[key]["extract"].items():
                files[out] = z.read(inner)
    lic = src["licenses"]
    for path, (out, want) in lic["files"].items():
        files[out] = fetch(lic["base_url"] + path, want, args.cache)
    files["README.txt"] = readme(manifest).encode()
    missing = [f for f in manifest["files"] if f not in files]
    if missing:
        sys.exit(f"caption-model: the sources do not give {', '.join(missing)}")
    os.makedirs(args.out, exist_ok=True)
    archive = os.path.join(args.out, name + ".zip")
    with zipfile.ZipFile(archive, "w") as z:
        for path in sorted(files):
            info = zipfile.ZipInfo(f"{name}/{path}", date_time=STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, files[path], compresslevel=9)
    with open(archive, "rb") as f:
        data = f.read()
    print(f"artifact: {archive}")
    print(f"bytes:    {len(data)}")
    print(f"sha256:   {sha256(data)}")
    if args.write:
        manifest["bytes"] = len(data)
        manifest["sha256"] = sha256(data)
        with open(MANIFEST, "w") as f:
            json.dump(manifest, f, indent=2)
            f.write("\n")
        print(f"recorded in {os.path.relpath(MANIFEST, ROOT)}")


def readme(manifest):
    src = manifest["sources"]
    lines = [
        "NVRView's Windows captions engine (downloaded on first use; see apps/windows/README.md, \"Captions\").",
        "",
        "Model: Moonshine streaming, " + src["model"]["base_url"] + " (MIT).",
        "moonshine.dll: from " + src["moonshine"]["url"] + " (MIT; third-party notices in licenses/).",
        "onnxruntime.dll: from " + src["onnxruntime"]["url"] + " (MIT; licenses/onnxruntime-*).",
        "Moonshine's library also contains the Silero voice activity detector (MIT).",
        "",
    ]
    return "\n".join(lines)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--out", default=os.path.join(ROOT, "build", "caption-model"))
    b.add_argument("--cache", default=os.path.join(ROOT, "build", "caption-model", "cache"))
    b.add_argument("--write", action="store_true")
    args = p.parse_args()
    if args.cmd == "build":
        build(args)


if __name__ == "__main__":
    main()
