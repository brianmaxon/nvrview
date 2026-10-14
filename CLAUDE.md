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
  focus, a doorbell's speaker): a separate allowlist tier, rate-limited (well
  above any human pace, so manual use never notices) and logged. The user's
  own manual PTZ controls work without a prior enable; talk-back still needs
  the camera's permission (the "Allow talk-back?" prompt). Nothing automated
  (the local API, automatic correction) sends motion commands unless the user
  granted that client or feature for that camera. See "Device commands" in
  docs/ARCHITECTURE.md.
- One session per device carrying all channels; close it on every exit path.
  NVRs allow only a few logins, and exhausting them locks out other clients.
- Tests and development run against `tools/mock-nvr`, not real hardware, unless
  the task is specifically to confirm a protocol fact.

## 4. Engineering habits

- **Measure; do not infer.** Confirm protocol behavior on hardware or with a
  test before building on it. Before claiming something builds, works, or is
  fixed, run it, and say exactly what was and was not checked.
- Tests wait on conditions, simulated or paused clocks, or frame and event counts,
  never on guessed real-time delays; a fixed sleep needs a reason (for example
  proving that nothing happens).
- Keyframe gating, bounded queues, rate-limited error logs, and log rotation
  capped by count and size are requirements, not polish.
- Keep the Rust core free of platform frameworks; Apple and Windows APIs live in
  the apps. The core must build and test on Linux and Windows.
- A user-visible feature lands on both apps (or [docs/PARITY.md](docs/PARITY.md) says why not), and its table is updated in the same change; a new self-test step gets a `// parity: id` marker.
- **Small, focused units, going forward.** New Mac app code goes in the
  feature module it belongs to and reaches the app only through its host
  protocol (docs/DEVELOPMENT.md, "Swift app modules"). Prefer files under
  about 800 lines: when a change would push a file past that, split off the
  part it touches by responsibility (an extension per concern, as
  `StageView+Capture.swift` does) in the same change. The same applies to C#
  and Rust. Existing large files are not refactored for their own sake.
- American English spelling everywhere.

## Working efficiently

Agent time and tokens go mostly into steps and the context each step
rereads, so take fewer, better-aimed steps.

- **Start current.** In a new worktree, `git merge main` first if the branch
  is behind local `main` (worktrees can start from the last pushed commit).
- **Targeted while working, full once at the end.** While iterating, run only
  what the change touches (`cargo nextest run -p <crate>`, `swift test
  --filter <Suite>`, one self-test). Run `scripts/check.sh` once when the work
  is done, and the Windows check once after that; rerun only what failed.
- **Logs to files.** Send build, test, and self-test output to a log file and
  read the summary and the failures (`tail`, `grep -E 'FAIL|error|panicked'`),
  never a whole log. Read source files by range when you know where to look.
- **Wait in one command.** `scripts/windows-check.sh` takes the Windows lock
  itself and waits for it inside that one command (agents queue behind any
  waiting merge); do not take the lock by hand or loop on `mkdir`. For any
  other long wait, block in a single command, not in repeated short checks.
- **Use the pointers you are given.** Prompts name files, types, and fixtures;
  start there instead of surveying the tree. Search with `rg` before opening
  files.
- **Docker leaves nothing behind.** Every `docker run` uses `--rm`, and no new
  named volumes: only the shared caches (`nvrview-linux-cargo`,
  `nvrview-sccache`, `nvrview-csharp-nuget`) exist; build output goes in
  anonymous volumes that die with the container. Old per-checkout volumes:
  `scripts/clean-docker-volumes.sh`.
- **Report briefly.** The final report says what changed, what ran and passed,
  and what was not verified; no full logs or file dumps.

## Checking your work

`scripts/check.sh` runs everything below in order and stops at the first
failure. Run it before every commit.

| what | command |
| --- | --- |
| sanitization, whole tree | `python3 -I scripts/check-sanitized.py --all` |
| self-test parity (both apps' step IDs) | `python3 -I scripts/check-parity.py` (`--list` prints the matrix) |
| Rust core | `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace && cargo test --workspace --doc` |
| Windows portability (from macOS) | `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc` |
| macOS app | `cd apps/macos && swift test` |

Docs-only fast path: when every change being checked is a `*.md` file (staged,
unstaged, and untracked changes, plus commits not yet on the upstream if the
branch has one, or any uncommitted change that differs from a checked tree only
in `*.md` files and `VERSION`, however many unpushed commits ahead),
`scripts/check.sh` runs only the sanitization check, the self-test parity
check, and the secret scan (`scripts/check-docs.sh`), and says so. Any other
change, or `NVRVIEW_FULL_CHECK=1`, runs everything. The uncommitted fast path
never writes a check stamp. A *committed* tree that differs from a stamped tree
only in `*.md` files and `VERSION` is checked already: the markdown checks run
once, then the stamps are carried over (`carried_from`) and no build or test
runs; `scripts/release.sh` counts such a stamp (the shipped binaries are the
same). Markdown plus any other file is a normal change.

**Test result cache.** Both checks skip a suite (one crate's tests, the Swift
tests, a Windows app self-test, ...) whose inputs, as committed or staged,
have a clean pass on that platform from the last 72 hours, print it as
`cached pass ...`, and end with a table of what ran. Rust reruns only the
changed crates and the crates above them. `NVRVIEW_FULL_CHECK=1` and the
release scripts run everything, as does the first run after 72 hours without
a full pass. A stamp with cached suites serves merges, never a release. Rules
and inputs: `scripts/suite-cache.py` and docs/DEVELOPMENT.md ("Test result cache");
tested by `scripts/test_suite_cache.py` and `scripts/test-suite-cache.sh`. The Windows app self-test is cached
per area (`scripts/windows-selftest-areas.txt` puts every `// parity:` step in
one area; `--areas a,b` runs some); a new step needs a table entry, and a new
file that only one area exercises goes in `WINDOWS_OWNED` in the cache script.
The other Windows suites are cached by the projects they build (video
self-test, smoke test, tile tests, shell tests; `DOTNET_REFS` in the cache
script mirrors the project references) and the update test by its updater
files, so a recordings-only change reruns only the recordings area.

**Trimmed build: daily, not per merge.** Trimming is checked statically on
every build (the trim analyzer's warnings are errors for the shipped projects,
and publish fails if a shipped assembly loses more than 10% of its size). The
trimmed publish itself, one `app-publish` smoke suite (startup plus one wiring
step per area) and the update test run in release checks, with
`NVRVIEW_FULL_CHECK=1`, and when the last passing trimmed run on Windows is over
24 h old (`NVRVIEW_TRIMMED_MAX_AGE_H`); agent checks and merges print them as
`deferred` (a stamp with them serves merges, never a release). When one fails,
bisect the day's merges.

**App self-tests on both apps.** The Mac check runs the fake-core app self-test
by area (`mac-app.AREA`, `scripts/macos-selftest-areas.txt`), cached like the
Windows areas; the real-core Mac self-test runs in full checks and releases. No
self-test step waits on the real clock: timed behavior uses a test clock, and
steps wait on conditions.

**Secret scan and timings.** The scan covers the commits added since the last
clean scan plus staged and unstaged changes (`scripts/secret-scan.sh`; record in
the common git directory); the full history runs once a day, with
`NVRVIEW_FULL_CHECK=1`, in releases, and when the ignore list changes. Every
check ends with its step times and the slowest three; each run appends a line
to `check-timings.jsonl` in the common git directory (last 500).

**Windows check and merging.** Run the final
`NVRVIEW_APP_SELFTEST=1 scripts/windows-check.sh` on a *clean committed tree*
(commit first): only then does it leave a stamp (shared through the common git
directory) that `scripts/merge-branch.sh` can reuse, so the merge does not run
the Windows check again. Commit nothing after it that touches code; a later
change means a new Windows run. The lead merges with
`scripts/merge-branch.sh <branch> "<message>"`: merge on a temporary branch,
then the Mac check (cached suites allowed and listed) and, only when the
branch's stamp does not cover the result or no full Windows run passed in 72
hours (rules in the script header), the Windows check at the same time; main
moves only when both pass. To check Mac and Windows together on any tree, use
`scripts/release-check.sh` (full runs).

**Merge trains.** `scripts/merge-train.sh BRANCH[=MESSAGE]...` merges several
finished branches (each with its own merge commit and message) on a temporary
branch, runs the Mac check and the Windows check once each, at the same time, on the
result (the reuse rules above apply to each merge step), and fast-forwards main. A branch
that conflicts is skipped and reported (a conflict only in `Cargo.lock` is
resolved by regenerating it); when a check fails, the train is bisected until
the failing branch is isolated, and the others merge. It prints a table of
merged / skipped / failed.

**Windows lock and hosts.** `scripts/windows-lock.sh` (taken by
`windows-check.sh`, `merge-branch.sh`, `merge-train.sh`, `release-check.sh`) is
the one lock for the Windows computer. Merges and releases wait at priority
`merge`, agents at `agent`: an agent check does not take a computer a waiting merge could use (priority is per host). The lock
records its owner; one whose process has been gone for 2 minutes is broken
with a log line. `NVRVIEW_WINDOWS_HOSTS="host-a host-b"` makes a pool with one
lock per host (setup: apps/windows/README.md); stamps and the cache are keyed
by tree and platform, so any host's result counts. With a pool, one check
splits over two free computers (Rust half on one, the rest on the other) when
that pays off: the second host is a try, never a wait, held only while its
half runs (`NVRVIEW_WINDOWS_SPLIT=0` turns it off; apps/windows/README.md).

**Branches arrive with a Windows result.** Before reporting a branch finished,
run `scripts/branch-ready.sh <branch>`. If the Windows stamp is missing
because of an environmental failure (a flaky self-test step, a lost
connection), rerun `windows-check.sh` until the final committed tree has one,
and report the stamp's tree hash (printed by `branch-ready.sh`). Merges and
trains print this readiness for each branch.
`scripts/test-stamps.sh` tests these rules with fake checks (part of
`scripts/check.sh`).

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
