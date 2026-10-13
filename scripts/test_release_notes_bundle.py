#!/usr/bin/env python3
"""Tests of scripts/release-notes-bundle.py with synthetic notes."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "release-notes-bundle.py")


def run(*args):
    return subprocess.run([sys.executable, "-I", SCRIPT, *args], capture_output=True, text=True)


class Bundle(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = self.dir.name

    def write(self, name, text):
        path = os.path.join(self.root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
        return path

    def test_history_is_newest_first_and_notes_replace_the_same_version(self):
        notes = self.write("n.txt", "Current.\n- one\n")
        self.write("h/0.2.0.txt", "Two.")
        self.write("h/0.10.0.txt", "Old ten.")
        self.write("h/1.0.0.txt", "Newer.")
        self.write("h/readme.txt", "not a version")
        out = os.path.join(self.root, "o.json")
        r = run("--version", "0.10.0", "--notes", notes, "--history", os.path.join(self.root, "h"), "--out", out)
        self.assertEqual(r.returncode, 0, r.stderr)
        releases = json.load(open(out, encoding="utf-8"))["releases"]
        self.assertEqual([x["version"] for x in releases], ["1.0.0", "0.10.0", "0.2.0"])
        self.assertEqual(releases[1]["notes"], "Current.\n- one")

    def test_only_notes_is_just_that_version(self):
        out = os.path.join(self.root, "o.json")
        r = run("--version", "0.3.0", "--notes", self.write("n.txt", "Only."), "--out", out)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.load(open(out, encoding="utf-8")), {"releases": [{"version": "0.3.0", "notes": "Only."}]})

    def test_refuses_long_empty_and_missing_notes(self):
        out = os.path.join(self.root, "o.json")
        self.assertNotEqual(run("--version", "0.3.0", "--notes", self.write("a.txt", "x" * 2001), "--out", out).returncode, 0)
        self.assertNotEqual(run("--version", "0.3.0", "--notes", self.write("b.txt", "  \n"), "--out", out).returncode, 0)
        self.assertNotEqual(run("--version", "0.3.0", "--notes", os.path.join(self.root, "none.txt"), "--out", out).returncode, 0)
        self.assertNotEqual(run("--version", "v3", "--notes", self.write("c.txt", "x"), "--out", out).returncode, 0)
        self.assertFalse(os.path.exists(out))

    def test_adds_to_a_zip_once(self):
        z = os.path.join(self.root, "p.zip")
        with zipfile.ZipFile(z, "w") as f:
            f.writestr("NvrView.exe", "x")
        notes = self.write("n.txt", "Zip.")
        self.assertEqual(run("--version", "0.3.0", "--notes", notes, "--add-to-zip", z).returncode, 0)
        with zipfile.ZipFile(z) as f:
            self.assertIn("release-notes.json", f.namelist())
        self.assertNotEqual(run("--version", "0.3.0", "--notes", notes, "--add-to-zip", z).returncode, 0)


if __name__ == "__main__":
    unittest.main()
