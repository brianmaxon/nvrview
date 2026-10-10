#!/usr/bin/env python3
"""Refuse content that could identify a real network, device, or person.

Usage:
  check-sanitized.py --staged   scan lines added in the staged diff (pre-commit)
  check-sanitized.py --all      scan every tracked file (CI)

See docs/SANITIZATION.md for the policy these rules enforce.
"""

from __future__ import annotations

import ipaddress
import os
import re
import subprocess
import sys
from pathlib import Path

ALLOW_MARKER = "sanitize:allow"
DENYLIST_PATH = Path.home() / ".config" / "nvrview" / "denylist.txt"

# Files whose job is to describe these patterns.
SKIP_FILES = {"scripts/check-sanitized.py", "docs/SANITIZATION.md"}

# Real captures and media are never committed, wherever they appear.
BLOCKED_SUFFIXES = {".pcap", ".pcapng", ".cap", ".h264", ".h265", ".hevc",
                    ".mp4", ".mov", ".mkv", ".jpg", ".jpeg", ".har", ".log"}
SYNTHETIC_DIR = "fixtures/synthetic/"
# Editor and OS leftovers. Swap files record the user name, host name, and
# path of the edited file in their header, and binary files are not scanned
# line by line, so they are blocked by name.
JUNK_NAME_RE = re.compile(r"(^|/)(\.[^/]*\.sw[a-p]|[^/]*~|\.#[^/]*|#[^/]*#|\.DS_Store)$")

IPV4_RE = re.compile(r"(?<![\d.])(\d{1,3}(?:\.\d{1,3}){3})(?![\d.])")
MAC_RE = re.compile(r"\b[0-9A-Fa-f]{2}(?:[:-][0-9A-Fa-f]{2}){5}\b")
REOLINK_UID_RE = re.compile(r"\b9527[0-9A-Z]{12}\b")
HOME_PATH_RE = re.compile(r"(/Users/[A-Za-z0-9._-]+|/home/[A-Za-z0-9._-]+|[A-Za-z]:\\Users\\[^\\\s]+"
                          r"|(?<![\w~])~[A-Za-z][A-Za-z0-9._-]*/)")
EMAIL_RE = re.compile(r"\b[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+\.)+[A-Za-z]{2,}\b")

ALLOWED_NETS = [ipaddress.ip_network(n) for n in (
    "192.0.2.0/24", "198.51.100.0/24", "203.0.113.0/24",  # RFC 5737
    "127.0.0.0/8", "0.0.0.0/32", "255.255.255.255/32",
)]
ALLOWED_EMAIL_DOMAINS = ("example.com", "example.org", "example.net",
                         "users.noreply.github.com")
DOC_MAC_PREFIX = "00:00:5e:00:53"  # RFC 7042


def ip_is_allowed(text: str) -> bool:
    try:
        addr = ipaddress.ip_address(text)
    except ValueError:
        return True  # not an address (for example a version string like 1.2.300.4)
    return any(addr in net for net in ALLOWED_NETS)


def load_denylist() -> list[str]:
    try:
        lines = DENYLIST_PATH.read_text(encoding="utf-8").splitlines()
    except OSError:
        return []
    return [line.strip() for line in lines if line.strip() and not line.startswith("#")]


def check_line(line: str, denylist: list[str]) -> list[str]:
    if ALLOW_MARKER in line:
        return []
    problems = []
    for match in IPV4_RE.finditer(line):
        if not ip_is_allowed(match.group(1)):
            problems.append(f"IP address {match.group(1)} (use 192.0.2.x)")
    for match in MAC_RE.finditer(line):
        if not match.group(0).lower().replace("-", ":").startswith(DOC_MAC_PREFIX):
            problems.append("MAC address (use 00:00:5E:00:53:xx)")
    if REOLINK_UID_RE.search(line):
        problems.append("Reolink device UID (use UID-EXAMPLE)")
    if HOME_PATH_RE.search(line):
        problems.append("absolute home-directory path")
    for match in EMAIL_RE.finditer(line):
        if not match.group(0).lower().endswith(ALLOWED_EMAIL_DOMAINS):
            problems.append("email address")
    lowered = line.lower()
    for entry in denylist:
        if entry.lower() in lowered:
            problems.append("entry from your private deny list")
    return problems


def check_path(path: str) -> list[str]:
    if JUNK_NAME_RE.search(path):
        return [f"{path}: editor or OS leftover (swap, backup, or .DS_Store file)"]
    suffix = Path(path).suffix.lower()
    if suffix in BLOCKED_SUFFIXES and not path.startswith(SYNTHETIC_DIR):
        return [f"{path}: capture/media file type {suffix} outside {SYNTHETIC_DIR}"]
    return []


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True,
                          text=True, encoding="utf-8", errors="replace").stdout


def scan_staged(denylist: list[str]) -> list[str]:
    findings = []
    for path in git("diff", "--cached", "--name-only", "--diff-filter=ACMR").splitlines():
        findings += check_path(path)
    current = None
    line_no = 0
    for raw in git("diff", "--cached", "-U0", "--diff-filter=ACMR").splitlines():
        if raw.startswith("+++ "):
            current = raw[6:] if raw.startswith("+++ b/") else None
        elif raw.startswith("@@"):
            hunk = re.search(r"\+(\d+)", raw)
            line_no = int(hunk.group(1)) if hunk else 0
        elif raw.startswith("+") and current and current not in SKIP_FILES:
            for problem in check_line(raw[1:], denylist):
                findings.append(f"{current}:{line_no}: {problem}")
            line_no += 1
    return findings


def scan_all(denylist: list[str]) -> list[str]:
    findings = []
    for path in git("ls-files").splitlines():
        findings += check_path(path)
        if path in SKIP_FILES or not os.path.isfile(path):
            continue
        try:
            text = Path(path).read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue  # binary; covered by the suffix rule
        for line_no, line in enumerate(text.splitlines(), 1):
            for problem in check_line(line, denylist):
                findings.append(f"{path}:{line_no}: {problem}")
    return findings


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "--staged"
    denylist = load_denylist()
    findings = scan_staged(denylist) if mode == "--staged" else scan_all(denylist)
    if findings:
        print("Sanitization check failed (see docs/SANITIZATION.md):", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
