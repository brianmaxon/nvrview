#!/usr/bin/env python3
"""Writes update-defaults.json, the public keys and update source a release
build bundles (docs/UPDATES.md, "Bundled defaults").

    update-defaults.py [--trusted-key-file FILE]... [--update-source SOURCE]
                       (--out FILE | --add-to-zip ZIP)

--trusted-key-file  a file with one or more `nvrview-ed25519-pub <64 hex>`
                    lines (blank lines and lines starting with # are skipped);
                    repeatable
--update-source     an absolute folder path or an http:// / https:// URL
--out FILE          write the JSON there
--add-to-zip ZIP    add it as update-defaults.json at the top of an existing
                    zip (the Windows package); refuses if one is already there

Validates before writing: every key line parses, the source is an absolute
folder path (POSIX, a Windows drive, or a UNC share) or an http(s) URL
without a user name or password, and at least one of the two is given. The
inputs are never committed; nothing is printed except the key IDs' count and
the source kind.
"""

import argparse
import json
import re
import sys
import zipfile

NAME = "update-defaults.json"
KEY = re.compile(r"^nvrview-ed25519-pub[ \t]+([0-9a-fA-F]{64})$")


def die(message):
    print(f"update-defaults: {message}", file=sys.stderr)
    sys.exit(1)


def keys_from(path):
    try:
        text = open(path, encoding="utf-8").read()
    except OSError as e:
        die(f"cannot read the key file: {e.strerror}")
    keys = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        m = KEY.match(line)
        if not m:
            die("a line in the key file is not an `nvrview-ed25519-pub <64 hex digits>` public key")
        key = "nvrview-ed25519-pub " + m.group(1).lower()
        if key not in keys:
            keys.append(key)
    return keys


def valid_source(s):
    if not s or s != s.strip() or any(c in s for c in "\r\n\0"):
        return False
    low = s.lower()
    for scheme in ("https://", "http://"):
        if low.startswith(scheme):
            rest = low[len(scheme):]
            authority = re.split(r"[/?#]", rest, maxsplit=1)[0]
            return bool(authority) and "@" not in authority and " " not in s
    if s.startswith("/") or s.startswith("\\\\"):
        return len(s) > 1
    return bool(re.match(r"^[A-Za-z]:[\\/]", s))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--trusted-key-file", action="append", default=[])
    ap.add_argument("--update-source")
    out = ap.add_mutually_exclusive_group(required=True)
    out.add_argument("--out")
    out.add_argument("--add-to-zip")
    args = ap.parse_args()

    keys = []
    for f in args.trusted_key_file:
        for k in keys_from(f):
            if k not in keys:
                keys.append(k)
    if args.trusted_key_file and not keys:
        die("the key file holds no public key")
    source = args.update_source
    if source is not None and not valid_source(source):
        die("--update-source must be an absolute folder path or an http:// or https:// URL (no user name or password)")
    if not keys and source is None:
        die("give --trusted-key-file and/or --update-source")

    data = {"keys": keys}
    if source is not None:
        data["source"] = source
    text = json.dumps(data, separators=(",", ":"), sort_keys=True) + "\n"

    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text)
    else:
        with zipfile.ZipFile(args.add_to_zip, "a", zipfile.ZIP_DEFLATED) as z:
            if NAME in z.namelist():
                die(f"{NAME} is already in the package")
            z.writestr(NAME, text)
    kind = "none" if source is None else ("url" if source.lower().startswith("http") else "folder")
    print(f"update-defaults: {len(keys)} key(s), source: {kind}")


main()
