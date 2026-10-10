# Keeping this repository clean

This is a public repository for software that talks to private home security
equipment. Nothing that identifies a real network, device, person, or
credential may ever be committed: not in code, comments, tests, fixtures,
commit messages, or documentation.

## What must never be committed

| Never commit | Use instead |
| --- | --- |
| Real IP addresses (LAN or public) | `192.0.2.x`, `198.51.100.x`, `203.0.113.x` (RFC 5737 documentation ranges) or `127.0.0.1` |
| Hostnames, Wi-Fi names, domain names of a real network | `nvr.example`, `example.com` |
| Device UIDs, serial numbers, MAC addresses | `UID-EXAMPLE`, `SERIAL-EXAMPLE`, `00:00:5E:00:53:xx` (RFC 7042 documentation MACs) |
| Usernames, passwords, nonces, session keys, password hashes | `example-user`, `example-password`, values generated in the test itself |
| Camera names, channel names, locations ("front door") | `Camera 1`, `Channel 1` |
| Real names and email addresses | nothing, or `someone@example.com` |
| Absolute paths from a developer machine (`/Users/...`, `C:\Users\...`) | paths relative to the repository |
| Packet captures, video, snapshots, or logs from real devices | synthetic fixtures (see below) |

## Fixtures are synthetic

Tests never use bytes captured from a real device. A real capture contains the
login nonce, hashed credentials that can be attacked offline, device UIDs,
addresses, and video of a real place. Instead:

- `fixtures/synthetic/` holds only files produced by the fixture generator from
  a made-up user, password, and nonce. The generator and its inputs are
  committed so anyone can regenerate the files byte for byte.
- Real captures used during reverse engineering live in `captures/` or
  `fixtures/private/`, which are git-ignored. Findings from them are written up
  in prose (command numbers, field names, structure), never pasted.

## Credentials at runtime

- The apps keep credentials in the operating system's credential store (macOS
  Keychain, Windows Credential Manager).
- Command-line tools read credentials from environment variables or the
  credential store, never from command-line arguments (which end up in shell
  history and process listings).
- Logs redact passwords, nonces, keys, and device UIDs. Addresses are logged
  only at debug level.

## Commit identity

Commits must use a GitHub `noreply` address
(`<id>+<username>@users.noreply.github.com`), set per repository:

```sh
git config user.email "<id>+<username>@users.noreply.github.com"
```

The pre-commit hook refuses commits made with any other address.

## Automated checks

- `scripts/check-sanitized.py` scans for the patterns above. It runs as a
  pre-commit hook on staged changes and in CI on the whole tree.
- CI also runs [gitleaks](https://github.com/gitleaks/gitleaks) over the full
  history to catch secrets the pattern list does not cover.
- A private deny list of your own real identifiers (addresses, UIDs, camera
  names, usernames) can live **outside** the repository at
  `~/.config/nvrview/denylist.txt`, one literal per line. The hook
  refuses any staged change containing one. Never put passwords in it.

Run `scripts/setup-dev.sh` once after cloning to install the hook.

A finding can be suppressed for one line with a trailing
`sanitize:allow` comment, and only when the value is provably not real (for
example a protocol constant that happens to look like an address).

## If something slips through

Assume it is public the moment it is pushed. Rotate any exposed credential
first, then rewrite history (`git filter-repo`) and force-push. Removing it in a
new commit is not enough.
