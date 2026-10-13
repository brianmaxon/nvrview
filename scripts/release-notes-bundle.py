#!/usr/bin/env python3
"""Writes release-notes.json, the release notes a build bundles so the apps
can show them (docs/UPDATES.md, "Bundled release notes").

    release-notes-bundle.py --version V --notes FILE [--history DIR]
                            (--out FILE | --add-to-zip ZIP)

--version V      the version being built (VERSION)
--notes FILE     that version's notes (plain text)
--history DIR    a folder of <version>.txt files, the notes of past versions;
                 the entry for V, if the folder has one, is replaced by --notes
--out FILE       write the JSON there
--add-to-zip ZIP add it as release-notes.json at the top of an existing zip
                 (the Windows package); refuses if one is already there

The file is {"releases": [{"version": "1.2.3", "notes": "..."}, ...]}, newest
first. Notes are plain text of at most 2000 characters (the manifest's limit);
files whose name is not a version are skipped. The notes are never committed:
they come from outside the repository and are only bundled.
"""

import argparse
import json
import os
import re
import sys
import zipfile

NAME = "release-notes.json"
MAX_CHARS = 2000
VERSION = re.compile(r"^[0-9]+(\.[0-9]+){0,3}$")


def die(message):
    print(f"release-notes-bundle: {message}", file=sys.stderr)
    sys.exit(1)


def key(version):
    return tuple(int(p) for p in version.split("."))


def read_notes(path):
    try:
        text = open(path, encoding="utf-8").read().replace("\r\n", "\n").strip()
    except (OSError, UnicodeDecodeError) as e:
        die(f"cannot read {os.path.basename(path)}: {e}")
    if not text:
        die(f"{os.path.basename(path)} is empty")
    if len(text) > MAX_CHARS:
        die(f"{os.path.basename(path)} has {len(text)} characters; the limit is {MAX_CHARS}")
    return text


def build(version, notes, history):
    if not VERSION.match(version):
        die("the version is not dotted numbers")
    found = {}
    if history:
        if not os.path.isdir(history):
            die("--notes-history is not a folder")
        for name in os.listdir(history):
            stem, ext = os.path.splitext(name)
            if ext == ".txt" and VERSION.match(stem):
                found[stem] = read_notes(os.path.join(history, name))
    found[version] = read_notes(notes)
    ordered = sorted(found, key=key, reverse=True)
    releases = [{"version": v, "notes": found[v]} for v in ordered]
    return json.dumps({"releases": releases}, ensure_ascii=False, indent=1) + "\n"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--version", required=True)
    p.add_argument("--notes", required=True)
    p.add_argument("--history")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--out")
    g.add_argument("--add-to-zip")
    a = p.parse_args()
    text = build(a.version, a.notes, a.history)
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(text)
    else:
        with zipfile.ZipFile(a.add_to_zip, "a") as z:
            if NAME in z.namelist():
                die(f"{NAME} is already in the zip")
            z.writestr(NAME, text)
    print(f"release-notes-bundle: {text.count(chr(34) + 'version' + chr(34))} version(s) of notes")


if __name__ == "__main__":
    main()
