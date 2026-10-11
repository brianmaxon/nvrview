# Working in this repository

nvrview is a public, MIT OR Apache-2.0 licensed native viewer for Reolink NVRs
and cameras: a shared Rust core (`crates/`) and per-platform apps (`apps/`).
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) is the design. These rules apply to
every session and every change.

@~/.config/nvrview/local-notes.md

## 1. Nothing private, ever

This repository is public and its subject is home security equipment. Follow
[docs/SANITIZATION.md](docs/SANITIZATION.md) in code, comments, tests,
fixtures, docs, commit messages, and PR text.

- No real IP addresses, hostnames, MACs, UIDs, serials, usernames, passwords,
  nonces, keys, or hashes. Use the placeholders that document lists
  (`192.0.2.x`, `UID-EXAMPLE`, `example-user`, ...).
- **No references to anyone's own installation.** Not the number or layout of
  cameras, what they point at, camera or channel names, which channel holds
  which lens, other systems on the network (home automation, media servers,
  NAS), or anything learned while working on a specific home setup. Write every
  example as a generic "an NVR with N channels" or "a multi-lens camera".
- Private context (local notes imported above, other local projects, logs,
  captures) informs the work but is never quoted, paraphrased into comments, or
  linked by absolute path.
- No absolute paths from a developer machine. No personal names or emails.
- Fixtures are synthetic, generated from made-up credentials. Real captures stay
  in the git-ignored `captures/` or `fixtures/private/`.
- Before every commit, review the staged diff for anything identifying, in
  addition to the automated hook. When unsure, leave it out and ask.

## 2. Clean-room protocol work

Follow [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md).

- Protocol facts go into `docs/protocol.md` with provenance (**observed** on
  hardware or **reported** by another project) before code depends on them.
- Never copy or closely paraphrase code, comments, or structure from neolink,
  other projects, or decompiled vendor code. Write from our own notes.
- Never commit decompiled or disassembled vendor code.

## 3. Device safety

- Never change device configuration: the session layer has a command
  allowlist, and nothing that changes device settings goes on it.
- The one exception is **camera motion control** (PTZ preset recall and moves,
  focus, a doorbell's speaker): a separate allowlist tier, off by default,
  enabled per camera by the user, rate-limited, and logged. Never send it
  automatically unless the user enabled automatic correction for that camera.
  See "Device commands" in docs/ARCHITECTURE.md.
- One session per device carrying all channels; close it on every exit path.
  NVRs allow only a few logins, and exhausting them locks out other clients.
- Tests and development run against `tools/mock-nvr`, not real hardware, unless
  the task is specifically to confirm a protocol fact.

## 4. Engineering habits

- **Measure; do not infer.** Confirm protocol behavior on hardware or with a
  test before building on it. Before claiming something builds, works, or is
  fixed, run it, and say exactly what was and was not checked.
- Keyframe gating, bounded queues, rate-limited error logs, and log rotation
  capped by count and size are requirements, not polish.
- Keep the Rust core free of platform frameworks; Apple and Windows APIs live in
  the apps. The core must build and test on Linux and Windows.
- American English spelling everywhere.

## Checking your work

`scripts/check.sh` runs everything below in order and stops at the first
failure. Run it before every commit.

| what | command |
| --- | --- |
| sanitization, whole tree | `python3 -I scripts/check-sanitized.py --all` |
| self-test parity (both apps' step IDs) | `python3 -I scripts/check-parity.py` (`--list` prints the matrix) |
| Rust core | `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` |
| Windows portability (from macOS) | `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc` |
| macOS app | `cd apps/macos && swift test` |

When chaining checks in a shell, make failures stop the chain (`set -e`, and
`set -o pipefail` when piping output); a filtered pipeline can otherwise hide
a failing step.

## Commits

- Commit identity must be a GitHub noreply address (the pre-commit hook
  enforces it). Run `scripts/setup-dev.sh` in a fresh clone.
- Commit locally as work completes, in small focused commits, after
  `scripts/check.sh` passes.
- **Build and test locally; do not push to find out.** Push only when a change
  needs verification on a platform this machine cannot run (Windows or Linux
  tests in CI), at a milestone, or when the user asks. Batch commits into those
  pushes. Never create releases or tags without explicit approval.
- Parallel agents work in git worktrees under `.worktrees/` (git-ignored), one
  branch each; review, verify, and merge them locally. After merging branches
  that both touched `Cargo.lock`, regenerate it rather than trusting the
  textual merge.
- Never bypass the pre-commit hook (`--no-verify`).
