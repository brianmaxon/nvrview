# nvrview

A lightweight, native live viewer for **Reolink** NVRs and cameras. It speaks
the devices' own protocol (Baichuan, TCP 9000) directly, with hardware video
decoding and no browser engine: a low-power alternative to the official
Reolink desktop client. It runs on macOS (Apple Silicon) and Windows 11, from
one shared Rust core.

Status: in development. Both apps play live video from real NVRs and cameras
(split-pane layouts and groups, several windows, focus with the main stream,
listening, detections, digital zoom, fisheye dewarping, finding devices on
the network) and doorbell calls with captions and talk-back, by voice or typed
reply, that can also be started without a ring. Recordings and playback are
designed but not built yet. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and milestones.

nvrview is an independent project. It is not affiliated with, endorsed by, or
sponsored by Reolink. "Reolink" is a trademark of its owner and is used here
only to describe compatibility.

## Repository

| Path | What |
| --- | --- |
| `crates/` | The shared Rust core: protocol, sessions, live and recorded media, audio, the C interface (`viewer-ffi`) |
| `apps/macos/` | The macOS app (Swift, AppKit/SwiftUI, Metal, VideoToolbox) |
| `apps/windows/` | The Windows app (C#, Media Foundation, Direct3D 11, WinUI 3) |
| `tools/` | `mock-nvr` (a fake device for tests and development), `bc-cli` (command-line client), `soak` |
| `fixtures/synthetic/` | Generated test media (never real recordings) |
| `docs/` | Design and contracts: architecture, protocol notes, C interface, rendering, audio, playback, interface |
| `scripts/` | Checks, packaging, setup |

## Building on macOS

Requirements: an Apple silicon Mac with macOS 26, Xcode 26 (Swift 6.2), and
Rust 1.88 or later through [rustup](https://rustup.rs). Optional:
`cbindgen` (checks the generated C header: `cargo install cbindgen --locked`),
Docker (Linux and C# test steps), `shellcheck`, `ffmpeg`, and `gitleaks`.

```sh
git clone https://github.com/brianmaxon/nvrview.git
cd nvrview
scripts/setup-dev.sh        # commit hook, identity check
scripts/check.sh            # everything CI runs, plus the macOS app tests
scripts/package-macos.sh    # builds build/NVRView.app with the Rust core
open build/NVRView.app
```

Add your NVR or camera in **Settings**: the new-device form lists what answers
on the network (macOS asks once to allow Local Network access; Windows may ask
the firewall), and choosing one fills the address and port, or type them
(port 9000), then the user and password;
the password is kept in the macOS Keychain. `package-macos.sh` signs with an
Apple Development identity from your keychain when there is one (otherwise ad
hoc), so the Keychain keeps trusting the app across rebuilds;
`NVRVIEW_SIGN_IDENTITY` picks another. The app is not notarized.

### Without a camera

`tools/mock-nvr` serves synthetic (or your own recorded) streams, so the app
and the tests run without hardware:

```sh
cargo run -p mock-nvr -- --port 9100 --channels 4
NVRVIEW_PASSWORD=example-password \
  build/NVRView.app/Contents/MacOS/NVRView --host 127.0.0.1 --port 9100 --user example-user
```

`build/NVRView.app/Contents/MacOS/NVRView --fake fixtures/synthetic/*.h264`
runs the app against a built-in fake core with no network at all.

### Working on the app

```sh
cd apps/macos
swift test                                  # app and bridge tests (fake core)
swift run NVRView --fake ../../fixtures/synthetic/h264-basic.h264
scripts/build-xcframework.sh                # from the repository root: the real core
NVRVIEW_REAL_CORE=1 swift test              # against the real core (after copying the XCFramework, see Package.swift)
```

## Building on Windows

Requirements: Windows 11 (x64) and the toolchain that
`scripts/setup-windows.ps1` installs (Rust, Visual Studio build tools, .NET 10,
LLVM, Python, Git Bash; optionally key-only SSH). In Git Bash, from the
repository root:

```sh
scripts/windows-app.sh build
target/dotnet/bin/NvrView.App/release_win-x64/NvrView.App.exe
```

The result is a self-contained folder (the .NET runtime and the Windows App
SDK are inside); nothing is installed, and it runs from wherever it is copied.
Add devices in **Settings** as on the Mac; passwords are kept in Windows
Credential Manager, and layouts and settings in `%LOCALAPPDATA%\NvrView`, so
replacing the app folder with a newer one keeps them. The details, options,
and the Windows App SDK license terms are in
[apps/windows/README.md](apps/windows/README.md).

`scripts/windows-check.sh` runs the Windows checks on such a computer over SSH
from a Mac, including uncommitted work:

```sh
NVRVIEW_WINDOWS_HOST=<ssh host> scripts/windows-check.sh
```

The script takes a lock on the computer itself (`scripts/windows-lock.sh`:
merges and releases go before agents), so parallel sessions queue without
extra steps. Several computers can share the load:
`NVRVIEW_WINDOWS_HOSTS="host-a host-b"` (see
[apps/windows/README.md](apps/windows/README.md#adding-a-windows-test-computer)).
Each run prints the computer's free disk space (and warns under 10 GB), then
the seconds every step took. Parallel sessions give themselves a worker folder
with `NVRVIEW_WINDOWS_WORKER=<name>` (default `main`); each keeps about 5 GB of
build output, so delete a finished one with
`NVRVIEW_WINDOWS_HOST=<ssh host> scripts/windows-check.sh --clean <name>`.
The worker builds without debug information (`NVRVIEW_WORKER_DEBUGINFO=1`
keeps it) and builds the release library and the C# projects in the
background while the Rust tests run. Optionally, in an administrator
PowerShell on that computer, exclude its build folders from Microsoft
Defender real-time scanning, which slows builds:

```powershell
Add-MpPreference -ExclusionPath "$env:USERPROFILE\nvrview", "$env:USERPROFILE\.cargo", "$env:USERPROFILE\.rustup", "$env:USERPROFILE\.nuget\packages", "$env:LOCALAPPDATA\Mozilla\sccache"
```

| folder | holds |
| --- | --- |
| `nvrview` | the check workers (source and `target`, several GB each) and the clone; most build traffic |
| `.cargo`, `.rustup` | the Rust toolchain, crate sources, and installed tools |
| `.nuget\packages` | the .NET packages |
| `Mozilla\sccache` | the shared compile cache (sccache's default folder, which the workers use) |

If Defender still scans heavily during builds, the build programs can be
excluded too, so the files they open are not scanned. This is broader: what
those programs touch anywhere goes unscanned.

```powershell
Add-MpPreference -ExclusionProcess "cargo.exe", "rustc.exe", "link.exe", "sccache.exe", "dotnet.exe", "MSBuild.exe"
```

`Get-MpPreference | Select-Object -ExpandProperty ExclusionPath` lists the
folder exclusions. That is your decision: it trades some protection for those
folders (and programs) for speed.

## Checks

`scripts/check.sh` runs, in order and stopping at the first failure:
sanitization and a full-history secret scan, `cargo fmt`, `clippy`, the Rust
tests, the C header check, the C smoke test (plain and with AddressSanitizer),
Windows and Linux compile checks, the C# smoke test in Docker, the macOS app
tests, and the macOS GUI self-tests (their windows open behind other windows).
Environment variables skip the slow or optional steps:
`NVRVIEW_CSHARP_TESTS=0`, `NVRVIEW_GUI_TESTS=0`, `NVRVIEW_SECRET_SCAN=0`.
`NVRVIEW_LINUX_TESTS=1` adds the full Linux tests in Docker (CI runs them on
every push) and `NVRVIEW_BROWSER_TESTS=1` the browser playback check. `scripts/clean-docker.sh` removes what the Docker steps leave behind.

### Test result cache

`scripts/check.sh` and `scripts/windows-check.sh` skip a test suite whose
inputs have not changed since a clean pass on the same platform in the last
72 hours, and print it as `cached pass <suite> (<age>, key <short>)`; a table
at the end shows each suite as ran or cached, with its time.
`scripts/suite-cache.py` holds the rules:

- **Inputs.** A suite's fingerprint covers its files as committed or staged
  (unstaged edits and untracked files outside its inputs do not matter), the
  tool versions, and the suite name. Each Rust crate's clippy, tests, and
  doctests cover the crate, every workspace crate it depends on (from
  `cargo metadata`; dev-dependencies count for the crate itself),
  `Cargo.lock`, the workspace and Cargo settings, and `fixtures/synthetic/`.
  Changing one crate reruns that crate and the crates above it, with
  `-p` arguments. The C header and C smoke tests cover the `viewer-ffi` and
  `mock-nvr` crates; the Swift tests and demo self-tests `apps/macos/`, the C
  header, and the fixtures; the C# and Windows app suites `apps/windows/`, the
  core crates they load, and the fixtures. The Windows app self-test is split
  into areas (startup, viewer, tiles, layouts, header, settings, calls,
  recordings, capture, ptz), each cached for the debug and the publish build:
  `scripts/windows-selftest-areas.txt` maps every `// parity:` step to one
  area (`scripts/check-selftest-areas.py` fails otherwise), the app runs only
  the areas asked for with `--areas a,b` (starting once), and an area's
  fingerprint is everything under `apps/windows/` except the files that only
  another area exercises (`WINDOWS_OWNED` in `scripts/suite-cache.py`), so
  shared code reruns every area and a change to a recordings file reruns
  recordings and the always-run startup smoke. A full run covers all areas.
  The other Windows suites cover only the C# projects they build (and what
  those reference): the video self-test the video demo project, the smoke test
  the smoke project, the tile tests and the shell tests their own projects,
  each with the shared build settings; the update test covers the updater and
  start-up files, the packaging script, and the signing tool's crates (the
  project table in `scripts/suite-cache.py` is checked against the project
  files by `suite-cache.py check-projects`). Sanitization, self-test parity,
  the secret scan, and `cargo fmt` always run.
- **Trimmed build.** The trimmed publish and its app self-test
  (`app-publish.*`) and the update test run in full runs (releases) and when
  the last passing trimmed run on Windows is more than 24 hours old
  (`NVRVIEW_TRIMMED_MAX_AGE_H`); other runs print them as `deferred` and count
  them like cached suites (merge stamps, never a release). The trade-off: a
  problem that only trimming causes shows at the daily run or the release, not
  at the merge that introduced it.
- **Secret scan.** Normal checks scan the commits not yet covered by a clean
  scan (`scripts/secret-scan.sh` records them in the git directory) and the
  staged and unstaged changes; the whole history is scanned when the last full
  scan is a day old, with `NVRVIEW_FULL_CHECK=1` (releases), and when the
  gitleaks ignore list changes. A failed scan records nothing.
- **Timings.** Every check ends with a table of its step times; merges and
  releases add the total and the slowest three steps. Each run appends a line
  (JSON: time, kind, total, steps) to `check-timings.jsonl` in the git
  directory, which keeps the last 500.
- **Parallel merges.** `scripts/merge-branch.sh` and `scripts/merge-train.sh`
  run the Mac check and the Windows check at the same time, each with its own
  log; the merge reaches main only when both pass.
- **Never cached.** A suite with an unstaged or untracked input runs and is
  not recorded. Failures are never recorded. A pass from a Windows run retried
  after a known flake is recorded as retried and never skips a suite.
- **Full runs.** Nothing is skipped with `NVRVIEW_FULL_CHECK=1`, in the release
  scripts (`scripts/release.sh --full-check`, `scripts/release-check.sh`),
  and when no full run on that platform passed in the last 72 hours
  (`NVRVIEW_FULL_MAX_AGE_H`; `NVRVIEW_CACHE_MAX_AGE_H` sets how long a pass
  stays valid). `NVRVIEW_FULL_EVERY=N` also makes every Nth run full.
- **Stamps.** A check stamp from a run with cached suites says so and serves
  `scripts/merge-branch.sh`, which prints the cached suites; a release needs a
  full stamp.
- **Where.** The cache lives in the git directory
  (`.git/nvrview-checks/suite-cache/`), shared by all worktrees, and is
  pruned after 14 days. On Windows the Mac decides which suites to run and
  records the worker's results.
- **One decision per run.** `scripts/suite-cache.py plan` decides every suite
  once at the start of a check (the tree, tools, and store as they are then;
  a pass recorded mid-run never changes a later decision) and writes the
  answers to a plan file that `scripts/check.sh` and `scripts/windows-check.sh`
  read with `awk` (`scripts/suite-cache-lib.sh`). The scripts append each
  result to a run log with `printf`, and one `finish` call folds the log into
  the store, also from the exit trap, so an interrupted run keeps the passes
  of the suites that completed. Python runs twice per check, resolved once to
  the real interpreter (not the pyenv shim).
- **Tests.** `scripts/test_suite_cache.py` tests the decision rules in one
  process; `scripts/test-suite-cache.sh` tests the shell wiring (plan file,
  run log, finish, interrupted and retried runs). `scripts/run-script-tests.sh`
  runs them with the stamp and merge-train tests at the same time, printing
  each file's output in order.

## Versions

The apps share one version, the `VERSION` file at the repository root. The
macOS bundle (`scripts/package-macos.sh`) and the Windows app (its file and
product version, and the first line of its log) both read it, so a build of
either shows the same number. Change it there only. Tags and releases need the
maintainer's approval.

## Updates

Both apps can update themselves from a folder or web server you host, with
packages signed by your own Ed25519 key; it is off until you set a source and
trust a key, and nothing is installed without your say. See
[docs/UPDATES.md](docs/UPDATES.md).

## Contributing

This repository is public and the software talks to private security
equipment. Before your first commit, read:

- [docs/SANITIZATION.md](docs/SANITIZATION.md): what must never be committed
- [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md): how protocol knowledge enters the code

and run `scripts/setup-dev.sh`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
